// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! API 层模块
//!
//! 提供 REST 端点定义、状态共享和路由构建。

pub mod admin;
pub mod anthropic;
pub mod count_tokens;
pub mod global_state;
pub mod health;
pub mod models;
pub mod openai;
pub mod openapi_docs;
pub mod readiness;
pub mod response_cache;
pub mod route_difficulty_explain;
pub mod route_explain;
pub mod router_builder;
pub mod routing_hints;
pub mod semantic_cache;
pub mod sse;
pub mod state;
#[cfg(feature = "vault")]
pub mod vault;

pub use router_builder::create_router;
pub use routing_hints::RoutingHints;
pub use state::ApiState;

pub use admin::{get_config, get_models, update_config, update_model};
pub use anthropic::messages;
pub use global_state::{get_global_state, init_global_state};
pub use openai::{chat_completions, embeddings};

use crate::error::{GateError, NexusFlowError};
#[cfg(feature = "resilience")]
use crate::i18n::t;
use crate::i18n::t_args; // 仅 resilience 特性分支的重试预算代码（execute_request_with_fallback 等）使用
#[cfg(feature = "resilience")]
use std::time::Duration;

/// 执行成功后回写会话亲和绑定（只记"上次成功服务的 vendor"）；未接线或
/// 无会话键时 no-op。`affinity_tenant_scoped` 开：首绑定收敛；关：legacy 键无条件 set。
pub(crate) async fn bind_session_affinity(
    state: &std::sync::Arc<ApiState>,
    api_key_hash: &str,
    session_key: Option<&str>,
    vendor_id: &str,
) {
    if let (Some(affinity), Some(session)) = (state.affinity.as_ref(), session_key) {
        if state.config.read().routing.affinity_tenant_scoped {
            affinity
                .bind_if_absent(Some(api_key_hash), session, vendor_id)
                .await;
        } else {
            affinity.bind(None, session, vendor_id).await;
        }
    }
}

/// 失败迁移重绑定 — 换路成功后无条件 set 指向新成功 vendor；租户维度判定同 [`bind_session_affinity`]。
pub(crate) async fn rebind_session_affinity(
    state: &std::sync::Arc<ApiState>,
    api_key_hash: &str,
    session_key: Option<&str>,
    vendor_id: &str,
) {
    if let (Some(affinity), Some(session)) = (state.affinity.as_ref(), session_key) {
        let tenant = state
            .config
            .read()
            .routing
            .affinity_tenant_scoped
            .then_some(api_key_hash);
        affinity.bind(tenant, session, vendor_id).await;
    }
}

/// 最终失败（含换路 fallback 耗尽）后解除会话亲和绑定，避免旧键指向已失败 vendor；未接线/无会话键时 no-op。
pub(crate) async fn clear_session_affinity(
    state: &std::sync::Arc<ApiState>,
    api_key_hash: &str,
    session_key: Option<&str>,
) {
    if let (Some(affinity), Some(session)) = (state.affinity.as_ref(), session_key) {
        let tenant = state
            .config
            .read()
            .routing
            .affinity_tenant_scoped
            .then_some(api_key_hash);
        affinity.unbind(tenant, session).await;
    }
}

/// 大 payload 截断 — 序列化后 >64KB 的 messages/response 存前缀 + 截断标记（按 UTF-8 字符边界截断，避免劈开多字节字符）。
pub(crate) fn truncate_payload(s: String) -> String {
    const MAX: usize = 64 * 1024;
    if s.len() <= MAX {
        return s;
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}{}",
        &s[..end],
        t_args("api-log-truncated", &[("total_bytes", s.len().to_string())])
    )
}

/// 从错误中提取上游真实状态码 — VendorUnavailable 的 vendor 字段携带上游
/// 状态（格式 "id (500 ...)"），RateLimited 即 429；提取不到返回 None。
pub(crate) fn upstream_status_code(err: &NexusFlowError) -> Option<i32> {
    match err {
        NexusFlowError::Gate(GateError::RateLimited { .. }) => Some(429),
        NexusFlowError::Gate(GateError::VendorUnavailable { vendor }) => {
            let inner = vendor.rsplit('(').next()?;
            let digits: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        }
        _ => None,
    }
}

/// 失败路径落库的 status_code — 优先上游真实状态码，否则核心错误固定
/// 映射（GateError::status_code，与 IntoResponse 同源）；Database 错误 500。
pub(crate) fn status_code_for_error(err: &NexusFlowError) -> i32 {
    upstream_status_code(err).unwrap_or(match err {
        NexusFlowError::Gate(e) => e.status_code() as i32,
        NexusFlowError::Database { .. } => 500,
        #[cfg(feature = "vault")]
        NexusFlowError::Vault(e) => match e {
            crate::vault::VaultError::NotFound(_) => 404,
            crate::vault::VaultError::UnknownVendor(_) => 409,
            _ => 500,
        },
    })
}

/// 守门器入口评估 — `amplification_gate` 开启时：f > 0.8·f_c → 收紧尝试
/// 上限到 1（fail-safe），r·f > 0.95 → 告警；关闭（默认）返回 base。C=Σ 启用 vendor 并发上限。
#[cfg(feature = "resilience")]
fn gate_adjusted_max_attempts(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    retry_cfg: &crate::config::RetrySection,
) -> u32 {
    let base = retry_cfg.max_total_attempts.max(1);
    if !retry_cfg.amplification_gate {
        return base;
    }
    let config = state.config.read();
    let capacity: f64 = config
        .vendors
        .iter()
        .filter(|v| v.enabled)
        .filter_map(|v| v.concurrency.max_concurrent_requests)
        .sum::<usize>() as f64;
    let Some((r, f, lambda)) = state.vendor_manager.metrics().amplification_view() else {
        return base;
    };
    let (tighten, diverge) = crate::utils::retry::amplification_guard(r, f, capacity, lambda);
    if diverge {
        tracing::warn!(r, f, "{}", t("api-amplification-diverging"));
    }
    if tighten {
        tracing::warn!(
            r,
            f,
            capacity,
            lambda,
            "{}",
            t("api-amplification-tightening")
        );
        return 1;
    }
    base
}

/// 换路排除原因 — Failed（真实失败）/ Throttled（RateLimited 或实时判定限流），供 Retry-After 提示判定。
#[cfg(feature = "resilience")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExclusionReason {
    Failed,
    Throttled,
}

/// 排除原因判定 — RateLimited 或 vendor 实时状态为 Throttled → Throttled；其余 → Failed。
#[cfg(feature = "resilience")]
async fn exclusion_reason(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    vendor_id: &str,
    e: &GateError,
) -> ExclusionReason {
    if matches!(e, GateError::RateLimited { .. })
        || state.vendor_manager.get_status(vendor_id).await
            == crate::vendor::VendorStatus::Throttled
    {
        ExclusionReason::Throttled
    } else {
        ExclusionReason::Failed
    }
}

/// 耗尽收尾 — `retry_after_hint` 开启且全部排除原因为 Throttled 时，取
/// 各 vendor 窗口重置秒数 min 抛 AllVendorsThrottled（429 + Retry-After 头）；否则原错误冒泡。
#[cfg(feature = "resilience")]
async fn finalize_fallback_error(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    original: GateError,
    excluded: &[(String, ExclusionReason)],
) -> GateError {
    let all_throttled = !excluded.is_empty()
        && excluded
            .iter()
            .all(|(_, r)| *r == ExclusionReason::Throttled);
    let hint_enabled = state.config.read().routing.retry_after_hint;
    let mut resets: Vec<(String, Option<u64>)> = Vec::with_capacity(excluded.len());
    for (id, _) in excluded {
        resets.push((id.clone(), throttle_reset_secs(state, id).await));
    }
    finalize_all_throttled(all_throttled, &resets, hint_enabled, original)
}

/// vendor 限流窗口重置剩余秒数 — window_end 与当前时刻差（向上取整，
/// 边界处宁可让客户端多等一瞬）；无窗口信息 / 无 window_end（不限时）→ None。
#[cfg(feature = "resilience")]
async fn throttle_reset_secs(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    vendor_id: &str,
) -> Option<u64> {
    let info = state.vendor_manager.get_window_info(vendor_id).await?;
    let end = info.window_end?;
    Some(
        end.saturating_duration_since(std::time::Instant::now())
            .as_secs_f64()
            .ceil() as u64,
    )
}

/// 耗尽收尾纯判定（脱离 state 单测）— `all_throttled` 全为 Throttled、
/// hint 开启且至少一个重置秒数可得（None=不可得）→ AllVendorsThrottled{min}；否则原错误。
#[cfg(feature = "resilience")]
pub(crate) fn finalize_all_throttled(
    all_throttled: bool,
    resets: &[(String, Option<u64>)],
    hint_enabled: bool,
    original: GateError,
) -> GateError {
    if !hint_enabled || !all_throttled || resets.is_empty() {
        return original;
    }
    match resets.iter().filter_map(|(_, r)| *r).min() {
        Some(secs) => GateError::AllVendorsThrottled {
            retry_after_secs: secs,
        },
        None => original,
    }
}

/// 两条换 vendor fallback 循环共享的初始化前言：链构建、`[retry]` 段一次
/// 热读、守门器收紧后的尝试上限与累计等待预算锚点。
#[cfg(feature = "resilience")]
struct FallbackBudget {
    chain: Option<crate::vendor::resilience::FallbackChain>,
    max_attempts: u32,
    budget: Option<Duration>,
    budget_anchor: tokio::time::Instant,
}

#[cfg(feature = "resilience")]
fn build_fallback_budget(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    model: &str,
) -> FallbackBudget {
    let chain = state.router.build_fallback_chain(model);
    // [retry] 段一次热读：尝试上限（含守门器收紧）与时间预算同源；锚点统一 tokio Instant
    let retry_cfg = state.config.read().retry.clone();
    let max_attempts = gate_adjusted_max_attempts(state, &retry_cfg);
    let budget =
        (retry_cfg.retry_budget_secs > 0).then(|| Duration::from_secs(retry_cfg.retry_budget_secs));
    let budget_anchor = tokio::time::Instant::now();
    FallbackBudget {
        chain,
        max_attempts,
        budget,
        budget_anchor,
    }
}

/// `same_protocol_only` 重路由候选选择——透传载荷不可跨协议，与
/// `expected_type` 不同的候选计入排除集跳过（不发起 HTTP）；无候选或达上限返回 None。
#[cfg(feature = "resilience")]
async fn next_compatible_route(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    model: &str,
    expected_type: &str,
    same_protocol_only: bool,
    max_attempts: u32,
    excluded: &mut Vec<String>,
) -> Option<crate::router::RoutingDecision> {
    loop {
        let res = state
            .router
            .route_excluding(model, excluded.as_slice())
            .await;
        match res {
            Ok(n) if same_protocol_only && n.vendor.type_ != expected_type => {
                // 协议不匹配的候选直接计入排除集（不发起 HTTP）
                excluded.push(n.vendor.id.clone());
                if excluded.len() as u32 >= max_attempts {
                    return None;
                }
            }
            Ok(n) => return Some(n),
            Err(_) => return None,
        }
    }
}

/// 回退可观测响应头：trail 构造期注入的开关开启时返回 4 头（流式由调用方
/// 在响应头阶段写入），关闭时返回空表；`trail` 为本次实际尝试链。
#[cfg(feature = "resilience")]
pub(crate) fn fallback_observation_headers(
    decision: &crate::router::RoutingDecision,
    trail: &crate::vendor::resilience::AttemptTrail,
) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    // 开关在 trail 构造期读定（本请求口径一致）
    if !trail.exposed() {
        return headers;
    }
    // 头值为受控字符（vendor/model id 为配置 slugs），解析失败时跳过该项（不使成功响应失败）
    let pairs = [
        ("x-nexusflow-vendor", decision.vendor.id.clone()),
        ("x-nexusflow-model", decision.model.model_id.clone()),
        ("x-nexusflow-attempts", trail.attempts().to_string()),
        ("x-nexusflow-fallbacks", trail.fallbacks_value()),
    ];
    for (name, value) in pairs {
        if let (Ok(n), Ok(v)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(&value),
        ) {
            headers.insert(n, v);
        }
    }
    headers
}

/// 成功响应附回退观测头：空表时原样返回。以值取 Response（特性关闭组合
/// 无 `unused_mut` 告警）。
pub(crate) fn with_fallback_headers(
    response: axum::response::Response,
    obs: &axum::http::HeaderMap,
) -> axum::response::Response {
    if obs.is_empty() {
        return response;
    }
    let mut response = response;
    for (name, value) in obs {
        response.headers_mut().insert(name.clone(), value.clone());
    }
    response
}

/// 前缀亲和 + 长上下文价的路由上下文（两协议共用）：
/// enabled=false 返回 None（选路完全回退历史路径）
pub(crate) fn route_ctx(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    body: &serde_json::Value,
    batch: bool,
) -> Option<crate::router::RouteCtx> {
    // 恒产 ctx：batch/长上下文价与前缀亲和开关互相独立（前缀亲和的
    // enabled 由 Router::affinity_pair 内部热读判定）
    let prefix_chars = state.config.read().routing.prefix_affinity.prefix_chars;
    let bytes = serde_json::to_string(body).map(|s| s.len()).unwrap_or(0);
    Some(crate::router::RouteCtx {
        prefix_hash: crate::router::prefix_hash_key(body, prefix_chars),
        est_prompt_tokens: (bytes / 4) as u64,
        batch,
    })
}

/// 难度定档结果透出头名（仅定档路径出现；值 ∈ {low,mid,high}）
pub(crate) const X_DIFFICULTY_TIER: &str = "x-difficulty-tier";

/// 观测头合并难度档位（定档未发生 = None = 原样返回）。响应头组装点统一
/// 经此合并，保证 X-Difficulty-Tier 与回退观测头同栈透出
pub(crate) fn extend_obs_with_tier(
    mut obs: axum::http::HeaderMap,
    tier: Option<&str>,
) -> axum::http::HeaderMap {
    if let Some(label) = tier {
        if let Ok(value) = axum::http::HeaderValue::from_str(label) {
            obs.insert(X_DIFFICULTY_TIER, value);
        }
    }
    obs
}

/// N1/N2 观测头合并（batch 路由 / 词法压缩发生时打标）
pub(crate) fn extend_obs_with_flags(
    mut obs: axum::http::HeaderMap,
    batch: bool,
    compressed: bool,
) -> axum::http::HeaderMap {
    if batch {
        obs.insert("x-batch-routed", axum::http::HeaderValue::from_static("1"));
    }
    if compressed {
        obs.insert("x-compressed", axum::http::HeaderValue::from_static("1"));
    }
    obs
}

/// 响应直接附难度档位头（缓存 HIT 等不经 obs 组装的快速路径用）
pub(crate) fn with_difficulty_tier_response(
    mut response: axum::response::Response,
    tier: Option<&str>,
) -> axum::response::Response {
    if let Some(label) = tier {
        if let Ok(value) = axum::http::HeaderValue::from_str(label) {
            response.headers_mut().insert(X_DIFFICULTY_TIER, value);
        }
    }
    response
}

/// 四档成本：`model_catalog` 单价，未登记模型全零；档源随 `Arc<Config>`
/// 快照引用（零深拷贝），usage 提数单源 `parse_usage_detailed`。
pub(crate) fn catalog_costs(
    state: &crate::api::state::ApiState,
    vendor_type: &str,
    model: &str,
    prompt: u32,
    completion: u32,
    cache_read: u32,
    cache_creation: u32,
) -> (f64, f64, f64, f64) {
    use crate::vendor::cost::{estimate_cost_from_catalog, prompt_cache_semantics};
    let snapshot = state.config.read();
    let usage = crate::protocol::TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_tokens: cache_read as u64,
        cache_creation_tokens: cache_creation as u64,
        ..Default::default()
    };
    // cache 包含语义单源于 `prompt_cache_semantics`（与配额回补、CostTracker 同一判定）
    let semantics = prompt_cache_semantics(vendor_type);
    match estimate_cost_from_catalog(&snapshot.model_catalog, model, &usage, semantics) {
        Some(estimate) => (
            estimate.input_cost,
            estimate.output_cost,
            estimate.cache_read_cost,
            estimate.cache_creation_cost,
        ),
        None => (0.0, 0.0, 0.0, 0.0),
    }
}

/// 分账记账：usage 缺失跳过（保持预扣），记账失败记 warn。`api_key_hash`
/// 由调用方透传请求方 key 摘要（不复用 key_pool 随机盐指纹，跨重启不稳定）。
pub(crate) async fn record_keyed_usage(
    state: &crate::api::state::ApiState,
    vendor_id: &str,
    vendor_type: &str,
    model_id: &str,
    api_key_hash: &str,
    response: &serde_json::Value,
) {
    let pre_aggregator = match state.pre_aggregator.as_ref() {
        Some(pa) => pa,
        None => return,
    };
    let (prompt, completion, cache_read, cache_creation) =
        match crate::protocol::parse_usage_detailed(response) {
            Some(usage) => usage,
            None => return,
        };
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost) = catalog_costs(
        state,
        vendor_type,
        model_id,
        prompt,
        completion,
        cache_read,
        cache_creation,
    );
    if let Err(e) = pre_aggregator
        .record_keyed(&crate::storage::KeyedUsage {
            vendor: vendor_id.to_string(),
            model: model_id.to_string(),
            api_key_hash: api_key_hash.to_string(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            input_cost,
            output_cost,
            cache_read_cost,
            cache_creation_cost,
        })
        .await
    {
        // 记账失败只记 warn，错误显性化
        tracing::warn!(
            "{}",
            crate::i18n::t_args("api-usage-record-rejected", &[("error", e.to_string())])
        );
    }
}

/// 单发调用的换 vendor fallback 循环（resilience）：失败 vendor 进排除集
/// 重路由，`FallbackChain::should_trigger` 判定换路；总尝试预算
/// max_total_attempts（默认 8）、累计等待预算 retry_budget_secs（默认 0 =
/// 不限时）；无候选/预算耗尽/不触发 → 原错误冒泡；耗尽分支统一经
/// finalize_fallback_error 收尾。返回最终成功的 RoutingDecision
#[cfg(feature = "resilience")]
pub(crate) async fn execute_request_with_fallback(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    model: &str,
    initial: crate::router::RoutingDecision,
    request: crate::protocol::UnifiedRequest,
    trail: &mut crate::vendor::resilience::AttemptTrail,
) -> Result<
    (
        crate::protocol::UnifiedResponse,
        crate::router::RoutingDecision,
    ),
    GateError,
> {
    let mut decision = initial;
    trail.record(&decision.vendor.id);
    // 排除集带原因（Failed/Throttled），Throttled 供 Retry-After 提示判定
    let mut excluded: Vec<(String, ExclusionReason)> = Vec::new();
    let FallbackBudget {
        chain,
        max_attempts,
        budget,
        budget_anchor,
    } = build_fallback_budget(state, model);

    loop {
        match state
            .vendor_manager
            .execute_request(&decision.vendor.id, request.clone())
            .await
        {
            Ok(response) => return Ok((response, decision)),
            Err(e) => {
                let triggered = chain
                    .as_ref()
                    .map(|c| c.should_trigger(&e))
                    .unwrap_or(false);
                let reason = exclusion_reason(state, &decision.vendor.id, &e).await;
                excluded.push((decision.vendor.id.clone(), reason));
                if !triggered || excluded.len() as u32 >= max_attempts {
                    // trigger/预算耗尽收尾（全 Throttled 且 hint 开启 → AllVendorsThrottled）
                    return Err(finalize_fallback_error(state, e, &excluded).await);
                }
                // 跨 vendor 累计等待超预算 → 放弃换路
                if let Some(b) = budget {
                    if crate::utils::retry::budget_exceeded(budget_anchor, b, Duration::ZERO) {
                        return Err(finalize_fallback_error(state, e, &excluded).await);
                    }
                }
                tracing::warn!(
                    vendor = %decision.vendor.id,
                    error = %e,
                    excluded = ?excluded,
                    "{}",
                    t("api-vendor-failed-rerouting")
                );
                let excluded_ids: Vec<String> = excluded.iter().map(|(id, _)| id.clone()).collect();
                match state.router.route_excluding(model, &excluded_ids).await {
                    Ok(next) => {
                        trail.record(&next.vendor.id);
                        decision = next;
                    }
                    // 无可用候选：收尾后冒泡（第二耗尽分支）
                    Err(_) => return Err(finalize_fallback_error(state, e, &excluded).await),
                }
            }
        }
    }
}

/// 透传非流式 / 两条流式路径的换 vendor fallback 循环（仅
/// cover_all_paths 开启时选用），语义镜像 execute_request_with_fallback；
/// 载荷类型异构，以派发闭包泛型化。流式 Err 均发生在响应头前，Ok 即流已
/// 建立、流中错误不换路；same_protocol_only 时跨协议候选计入排除集跳过。
/// 返回最终成功的 RoutingDecision
#[cfg(feature = "resilience")]
pub(crate) async fn execute_dispatch_with_fallback<T, F, Fut>(
    state: &std::sync::Arc<crate::api::state::ApiState>,
    model: &str,
    initial: crate::router::RoutingDecision,
    same_protocol_only: bool,
    dispatch: F,
    trail: &mut crate::vendor::resilience::AttemptTrail,
) -> Result<(T, crate::router::RoutingDecision), GateError>
where
    // 派发闭包持有决策所有权并返回不借用环境的 future：稳定 Rust 的
    // Fn->Fut 无法表达借用闭包参数的高阶生命周期（async-book 限制）。
    F: Fn(crate::router::RoutingDecision) -> Fut,
    Fut: std::future::Future<Output = Result<T, GateError>>,
{
    let expected_type = initial.vendor.type_.clone();
    let mut decision = initial;
    trail.record(&decision.vendor.id);
    let mut excluded: Vec<String> = Vec::new();
    let FallbackBudget {
        chain,
        max_attempts,
        budget,
        budget_anchor,
    } = build_fallback_budget(state, model);

    loop {
        match dispatch(decision.clone()).await {
            Ok(response) => return Ok((response, decision)),
            Err(e) => {
                let triggered = chain
                    .as_ref()
                    .map(|c| c.should_trigger(&e))
                    .unwrap_or(false);
                excluded.push(decision.vendor.id.clone());
                if !triggered || excluded.len() as u32 >= max_attempts {
                    return Err(e);
                }
                // 跨 vendor 累计等待超预算 → 放弃换路，返回最后错误（保留 5xx/429 语义）
                if let Some(b) = budget {
                    if crate::utils::retry::budget_exceeded(budget_anchor, b, Duration::ZERO) {
                        return Err(e);
                    }
                }
                tracing::warn!(
                    vendor = %decision.vendor.id,
                    error = %e,
                    excluded = ?excluded,
                    "{}",
                    t("api-vendor-failed-rerouting-pre-stream")
                );
                let next = match next_compatible_route(
                    state,
                    model,
                    &expected_type,
                    same_protocol_only,
                    max_attempts,
                    &mut excluded,
                )
                .await
                {
                    Some(n) => n,
                    // 无可用候选 / 排除达上限：以原错误冒泡（保留 5xx/429 语义）
                    None => return Err(e),
                };
                trail.record(&next.vendor.id);
                decision = next;
            }
        }
    }
}

// ============ tests：fallback 预算与守门器 ============
//
// 两个 fallback 循环是 pub(crate)——集成面只能走 HTTP 全链路，无法对
// 注入计数/时钟做确定性断言；本模块以桩派发闭包直接驱动循环本体。用
// 真实时钟（start_paused 与本模块 sqlite 连接池冲突，PoolTimedOut）。
#[cfg(all(test, feature = "resilience", feature = "database"))]
mod fallback_guard_tests {
    use super::*;
    use crate::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, RetrySection, RoutingSection, ServerConfig, VendorConfig,
    };
    use crate::database::{
        AsyncLogger, AsyncLoggerHandle, Database, Repository, TokenPreAggregator,
    };
    use crate::protocol::{AnthropicConverter, OpenAIConverter};
    use crate::router::Router as NexusRouter;
    use crate::storage::{RequestLogSink, UsageSink};
    use crate::vendor::VendorManager;
    use dbnexus::{DbConfig, DbPool};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn make_vendor(id: &str, priority: u8, cap: Option<usize>) -> VendorConfig {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            name: format!("Vendor {id}"),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled: true,
            priority,
            models: vec![ModelConfig {
                model_id: "shared-model".into(),
                abstract_id: Some("shared-model".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: cap,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
            supports_batch: false,
        }
    }

    async fn make_state(vendors: Vec<VendorConfig>, retry: RetrySection) -> Arc<ApiState> {
        let config = Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors,
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry,
            routing: RoutingSection::default(),
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
        };
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let logger = Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
            repository.clone(),
            1024,
            Duration::from_millis(200),
        )));
        let pa = Arc::new(TokenPreAggregator::new(
            repository.clone(),
            Duration::from_secs(60),
        ));
        let database = Arc::new(Database {
            pool,
            repository: repository.clone(),
        });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));
        let db_sink: Arc<dyn RequestLogSink> = database;
        let log_front: Option<Arc<dyn RequestLogSink>> = Some(logger);
        let usage_front: Option<Arc<dyn UsageSink>> = Some(pa);
        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: db_sink,
            logger: log_front,
            pre_aggregator: usage_front,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        })
    }

    /// 灌超阈观测：6/6 全失败（f=1.0）、r=1.0、C=20、λ=0.1 →
    /// f_c≈0.995，f=1.0 > 0.8·f_c → 收紧。
    fn flood_superthreshold_metrics(state: &ApiState) {
        let m = state.vendor_manager.metrics();
        for _ in 0..6 {
            m.record_request("v-a", false, Duration::from_millis(1));
        }
        for _ in 0..4 {
            m.record_retry_attempt();
            m.record_failed_request_before_retry();
        }
    }

    /// 总是失败的桩派发闭包：记调用次数，可选先等待（预算测试用）。输出
    /// future 装箱为 trait 对象（嵌套 RPIT 不上稳定 Rust，E0782）。
    fn failing_dispatcher(
        calls: Arc<AtomicUsize>,
        pre_sleep: Duration,
    ) -> impl Fn(
        crate::router::RoutingDecision,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GateError>>>> {
        move |_d: crate::router::RoutingDecision| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if pre_sleep > Duration::ZERO {
                    tokio::time::sleep(pre_sleep).await;
                }
                Err::<(), GateError>(GateError::VendorUnavailable {
                    vendor: "stub".into(),
                })
            })
        }
    }

    /// gate=true + 超阈计数 → max_total_attempts 收紧到 1：首次失败即放弃，v-b 不被触达。
    #[tokio::test]
    async fn gate_on_tightens_total_attempts_to_one() {
        let state = make_state(
            vec![
                make_vendor("v-a", 1, Some(10)),
                make_vendor("v-b", 2, Some(10)),
            ],
            RetrySection {
                amplification_gate: true,
                ..Default::default()
            },
        )
        .await;
        flood_superthreshold_metrics(&state);

        let initial = state
            .router
            .route_excluding("shared-model", &[])
            .await
            .expect("initial route");
        let calls = Arc::new(AtomicUsize::new(0));
        let result = execute_dispatch_with_fallback(
            &state,
            "shared-model",
            initial,
            false,
            failing_dispatcher(calls.clone(), Duration::ZERO),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        assert!(result.is_err(), "桩恒失败，最终必须冒泡错误");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "守门器收紧到 1：首次失败即放弃，v-b 不得被触达"
        );
    }

    /// 双开关默认（gate=false、budget=0）：同样超阈数据 → 换路 v-b 再失败才放弃（2 次派发）。
    #[tokio::test]
    async fn defaults_keep_current_behavior_two_dispatches() {
        let state = make_state(
            vec![
                make_vendor("v-a", 1, Some(10)),
                make_vendor("v-b", 2, Some(10)),
            ],
            RetrySection::default(),
        )
        .await;
        flood_superthreshold_metrics(&state);

        let initial = state
            .router
            .route_excluding("shared-model", &[])
            .await
            .expect("initial route");
        let calls = Arc::new(AtomicUsize::new(0));
        let result = execute_dispatch_with_fallback(
            &state,
            "shared-model",
            initial,
            false,
            failing_dispatcher(calls.clone(), Duration::ZERO),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "默认关闭：gate/budget 必须都被无视，a→b 换路耗尽（现状）"
        );
    }

    /// budget_secs=1 + 每次派发先真实等待 2s → 跨 vendor 累计耗时超预算：
    /// 第一次失败后即放弃，不重派 v-b（2s/1s 余量对调度抖动稳健）。
    #[tokio::test]
    async fn budget_secs_abandons_after_first_wait() {
        let state = make_state(
            vec![
                make_vendor("v-a", 1, Some(10)),
                make_vendor("v-b", 2, Some(10)),
            ],
            RetrySection {
                retry_budget_secs: 1,
                ..Default::default()
            },
        )
        .await;

        let initial = state
            .router
            .route_excluding("shared-model", &[])
            .await
            .expect("initial route");
        let calls = Arc::new(AtomicUsize::new(0));
        let result = execute_dispatch_with_fallback(
            &state,
            "shared-model",
            initial,
            false,
            failing_dispatcher(calls.clone(), Duration::from_secs(2)),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "锚点已耗时 2s > 1s 预算：重派前放弃（tokio Instant 随 paused clock 推进）"
        );
    }
}
// ============ tests：Retry-After 提示与耗尽收尾 ============
//
// 全 Throttled 场景经配置注入（Token max_tokens=0 → worker 限流早退
// RateLimited，无真实 HTTP）；初始决策用 direct 形式手工构造
//（try_direct_route 不查状态），否则首路由即被 ==Available 过滤、
// 循环无法进入。
#[cfg(all(
    test,
    feature = "resilience",
    feature = "database",
    feature = "web-server"
))]
mod cooldown_retry_after {
    use super::*;
    use crate::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, RetrySection, RoutingSection, ServerConfig, VendorConfig,
    };
    use crate::database::{
        AsyncLogger, AsyncLoggerHandle, Database, Repository, TokenPreAggregator,
    };
    use crate::protocol::{AnthropicConverter, OpenAIConverter, UnifiedRequest};
    use crate::router::{RoutingDecision, RoutingStrategy};
    use crate::storage::{RequestLogSink, UsageSink};
    use crate::vendor::VendorManager;
    use axum::response::IntoResponse;
    use dbnexus::{DbConfig, DbPool};
    use std::sync::Arc;
    use std::time::Duration;

    /// 本模块内 NexusRouter 别名（镜像 fallback_guard_tests 的 use 形式）
    use crate::router::Router as NexusRouter;

    fn make_vendor(id: &str, priority: u8, max_tokens: u64, api_base: &str) -> VendorConfig {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.to_string(),
            name: format!("Vendor {id}"),
            type_: "openai".to_string(),
            api_key_env: "NEXUSFLOW_TEST_KEY".to_string(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: api_base.to_string(),
            enabled: true,
            priority,
            models: vec![ModelConfig {
                model_id: "shared-model".to_string(),
                abstract_id: Some("shared-model".to_string()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens,
                window_hours: Some(1),
                sliding: false,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
            supports_batch: false,
        }
    }

    async fn make_state(
        vendors: Vec<VendorConfig>,
        routing: RoutingSection,
        retry: RetrySection,
    ) -> Arc<ApiState> {
        let config = Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors,
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry,
            routing,
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
        };
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let logger = Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
            repository.clone(),
            1024,
            Duration::from_millis(200),
        )));
        let pa = Arc::new(TokenPreAggregator::new(
            repository.clone(),
            Duration::from_secs(60),
        ));
        let database = Arc::new(Database {
            pool,
            repository: repository.clone(),
        });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));
        let db_sink: Arc<dyn RequestLogSink> = database;
        let log_front: Option<Arc<dyn RequestLogSink>> = Some(logger);
        let usage_front: Option<Arc<dyn UsageSink>> = Some(pa);
        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: db_sink,
            logger: log_front,
            pre_aggregator: usage_front,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        })
    }

    fn chat_request() -> UnifiedRequest {
        serde_json::from_value(serde_json::json!({
            "model": "shared-model",
            "messages": []
        }))
        .unwrap()
    }

    /// direct 形式初始决策（不查 vendor 状态，绕过 ==Available 过滤使循环可达）。
    fn direct_decision(vendor: &VendorConfig) -> RoutingDecision {
        RoutingDecision {
            vendor: vendor.clone(),
            model: vendor.models[0].clone(),
            strategy: RoutingStrategy::Direct,
        }
    }

    // ---- 纯判定（脱离 state）----

    /// 收尾纯判定：min 重置秒数、混合原因/hint 关/重置不可得/空集 → 原错误。
    #[test]
    fn finalize_all_throttled_min_reset_and_gates() {
        let original = || GateError::VendorUnavailable {
            vendor: "v-a (500)".into(),
        };
        // 全 Throttled + hint 开 → min(30, 60) = 30
        let out = finalize_all_throttled(
            true,
            &[("v-a".to_string(), Some(30)), ("v-b".to_string(), Some(60))],
            true,
            original(),
        );
        match out {
            GateError::AllVendorsThrottled { retry_after_secs } => {
                assert_eq!(retry_after_secs, 30, "min 重置秒数");
            }
            other => panic!("期望 AllVendorsThrottled，实际 {other}"),
        }
        // 单 vendor 可得、另一 vendor 不可得 → 取可得者
        let out = finalize_all_throttled(
            true,
            &[("v-a".to_string(), None), ("v-b".to_string(), Some(45))],
            true,
            original(),
        );
        assert!(matches!(
            out,
            GateError::AllVendorsThrottled {
                retry_after_secs: 45
            }
        ));
        // 混合原因（非全 Throttled）→ 原错误
        assert!(matches!(
            finalize_all_throttled(false, &[("v-a".to_string(), Some(30))], true, original()),
            GateError::VendorUnavailable { .. }
        ));
        // hint 关 → 原错误
        assert!(matches!(
            finalize_all_throttled(true, &[("v-a".to_string(), Some(30))], false, original()),
            GateError::VendorUnavailable { .. }
        ));
        // 重置全部不可得 → 原错误
        assert!(matches!(
            finalize_all_throttled(true, &[("v-a".to_string(), None)], true, original()),
            GateError::VendorUnavailable { .. }
        ));
        // 空排除集 → 原错误
        assert!(matches!(
            finalize_all_throttled(true, &[], true, original()),
            GateError::VendorUnavailable { .. }
        ));
    }

    // ---- E2E：execute_request_with_fallback 两处耗尽分支 ----

    /// 分支一（trigger/预算耗尽）：max_total_attempts=1，v-a 首失败即耗尽
    /// → 全 Throttled + hint 开 → AllVendorsThrottled，429 + Retry-After 头。
    #[tokio::test]
    async fn trigger_exhausted_all_throttled_yields_429_with_retry_after() {
        let state = make_state(
            vec![make_vendor("v-a", 1, 0, ""), make_vendor("v-b", 2, 0, "")],
            RoutingSection {
                retry_after_hint: true,
                ..Default::default()
            },
            RetrySection {
                max_total_attempts: 1,
                ..Default::default()
            },
        )
        .await;
        let v_a = make_vendor("v-a", 1, 0, "");
        let result = execute_request_with_fallback(
            &state,
            "shared-model",
            direct_decision(&v_a),
            chat_request(),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        let err = NexusFlowError::from(result.expect_err("两 vendor 限流必须失败"));
        let secs = match &err {
            NexusFlowError::Gate(GateError::AllVendorsThrottled { retry_after_secs }) => {
                *retry_after_secs
            }
            other => panic!("期望 AllVendorsThrottled，实际 {other}"),
        };
        assert!(secs >= 1, "重置秒数取自窗口快照: {secs}");
        let response = err.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .is_some(),
            "Retry-After 头必须存在"
        );
    }

    /// 分支二（route_excluding 无候选）：fail-open 使重派可达 v-b，双 vendor
    /// 均入排除集后重路由无候选 → 收尾 AllVendorsThrottled。
    #[tokio::test]
    async fn route_exhausted_both_throttled_yields_retry_after() {
        let state = make_state(
            vec![make_vendor("v-a", 1, 0, ""), make_vendor("v-b", 2, 0, "")],
            RoutingSection {
                retry_after_hint: true,
                fail_open_when_all_cooling: true,
                ..Default::default()
            },
            RetrySection {
                max_total_attempts: 3,
                ..Default::default()
            },
        )
        .await;
        let v_a = make_vendor("v-a", 1, 0, "");
        let result = execute_request_with_fallback(
            &state,
            "shared-model",
            direct_decision(&v_a),
            chat_request(),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        let err = result.expect_err("全部候选限流必须失败");
        assert!(
            matches!(err, GateError::AllVendorsThrottled { .. }),
            "第二耗尽分支（重路由无候选）也应收敛 AllVendorsThrottled: {err}"
        );
    }

    /// 混合原因（v-a 限流 + v-b 真实失败）→ 原错误冒泡（503 语义不变）。
    /// 「原错误」是最后一次失败的错误，故 Throttled 放前、真实失败放后。
    #[tokio::test]
    async fn mixed_reasons_keep_original_error() {
        let state = make_state(
            vec![
                make_vendor("v-a", 1, 0, ""),
                make_vendor("v-b", 2, 1_000_000, "http://127.0.0.1:1"),
            ],
            RoutingSection {
                retry_after_hint: true,
                ..Default::default()
            },
            RetrySection {
                max_total_attempts: 3,
                ..Default::default()
            },
        )
        .await;
        let v_a = make_vendor("v-a", 1, 0, "");
        let result = execute_request_with_fallback(
            &state,
            "shared-model",
            direct_decision(&v_a),
            chat_request(),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        let err = result.expect_err("混合原因最终必须失败");
        match &err {
            GateError::VendorUnavailable { .. } => {
                assert_eq!(err.status_code() as i32, 503);
            }
            other => panic!("混合原因必须保持原错误冒泡，实际 {other}"),
        }
    }

    /// 默认关（hint=false）：同样的全 Throttled 数据 → 原错误冒泡
    ///（RateLimited 语义保留，不产生 AllVendorsThrottled）。
    #[tokio::test]
    async fn hint_off_keeps_original_error() {
        let state = make_state(
            vec![make_vendor("v-a", 1, 0, ""), make_vendor("v-b", 2, 0, "")],
            RoutingSection::default(),
            RetrySection {
                max_total_attempts: 1,
                ..Default::default()
            },
        )
        .await;
        let v_a = make_vendor("v-a", 1, 0, "");
        let result = execute_request_with_fallback(
            &state,
            "shared-model",
            direct_decision(&v_a),
            chat_request(),
            &mut crate::vendor::resilience::AttemptTrail::new(true),
        )
        .await;
        let err = result.expect_err("全部候选限流必须失败");
        assert!(
            matches!(err, GateError::RateLimited { .. }),
            "默认关必须保持原错误（RateLimited），实际 {err}"
        );
    }
}
