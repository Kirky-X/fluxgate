// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 逐请求成本（`request_logs.cost_usd` 单一来源）。
//!
//! 计价单源与分账/配额一致：`[[model_catalog]]` 四档单价经
//! `estimate_cost_from_catalog`（cache 语义单源 `prompt_cache_semantics`）。
//! 无 usage（失败/空响应）→ `None`（不写成本）；未登记单价 → `Some(0.0)`
//!（与 token_usage 分账、rerank 既有口径一致：写 0 而非 None）。

use crate::config::ModelCatalogEntry;

/// 逐请求成本（USD 道语义，币种随目录条目，与 `/admin/cost` 同口径）：
/// `response.usage` 四档 token 求和 × 目录单价。未登记单价 → `Some(0.0)`
///（登记性检查单源 `lookup_pricing_entry`）。
pub(crate) fn request_cost_usd(
    catalog: &[ModelCatalogEntry],
    vendor_type: &str,
    model_id: &str,
    response: &serde_json::Value,
) -> Option<f64> {
    let (prompt, completion, cache_read, cache_creation) =
        crate::protocol::parse_usage_detailed(response)?;
    if crate::vendor::cost::lookup_pricing_entry(catalog, model_id).is_none() {
        return Some(0.0);
    }
    let usage = crate::protocol::TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_tokens: cache_read as u64,
        cache_creation_tokens: cache_creation as u64,
        cache_creation_1h_tokens: crate::protocol::parse_usage_1h_creation(response),
        ..Default::default()
    };
    let semantics = crate::vendor::cost::prompt_cache_semantics(vendor_type);
    crate::vendor::cost::estimate_cost_from_catalog(catalog, model_id, &usage, semantics)
        .map(|estimate| estimate.total_cost)
}

/// 会话/上下文观测列（`session_key` + usage 派生的三列）。
///
/// 与 [`request_cost_usd`] 同源取 usage（`parse_usage_detailed`）与目录条目
///（`lookup_pricing_entry`），保证「成本列非空」与「缓存列非空」口径一致。
/// 一次解析、一次查找，避免热路径重复遍历与整目录克隆。
///
/// `enabled=false`（`[usage_insights]` 关）时全部返回 `None`——采集门与
/// 查询门同源，关闭后不写库（与文档承诺一致）。
pub(crate) fn request_context_columns(
    enabled: bool,
    catalog: &[ModelCatalogEntry],
    model_id: &str,
    response: Option<&serde_json::Value>,
    session_key: Option<&str>,
) -> ContextColumns {
    if !enabled {
        return ContextColumns::default();
    }
    let window = crate::vendor::cost::lookup_pricing_entry(catalog, model_id)
        .map(|entry| entry.context_window.min(i64::MAX as u64) as i64);
    // 缓存列保留上游原值，不在此处归一：归零会永久丢失 Inclusive 协议的
    // 命中量（`cache_read_share` 对其恒 0，且无法回溯）。占用率口径的
    // Inclusive/Exclusive 折中见 `repository::INPUT_TOKENS_EXPR`。
    let (cache_read, cache_write) = response
        .and_then(crate::protocol::parse_usage_detailed)
        .map(|(_, _, read, write)| (Some(read as i64), Some(write as i64)))
        .unwrap_or((None, None));
    ContextColumns {
        session_key: session_key.map(str::to_string),
        context_window_tokens: window,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

/// 缓存经济学亲和结算（`[routing.affinity]`）：响应 usage 结算时按模式
/// 决定值不值得粘（R-affinity-002）。
///
/// 与 [`request_cost_usd`] 同源取 usage（同一 `parse_usage_detailed` 调用，
/// 一次解析复用四档），保证「成本口径」与「粘性与否」看到同一份 usage。
/// 绑定动作与 `legacy` 现行为互斥：本函数写亲和时不再走
/// `bind_session_affinity`（后者是 legacy 的 1h 无条件 set）。
pub(crate) async fn settle_api_affinity(
    state: &crate::api::state::ApiState,
    api_key_hash: &str,
    session_key: Option<&str>,
    vendor_id: &str,
    response: Option<&serde_json::Value>,
) -> Option<crate::router::affinity::AffinityDecision> {
    let writer = state.affinity_writer.as_ref()?;
    // 一次解析复用四档（与 request_cost_usd 同源，避免热路径重复遍历）
    let (prompt, _completion, cache_read, cache_creation) = response
        .and_then(crate::protocol::parse_usage_detailed)
        .unwrap_or((0, 0, 0, 0));
    let usage = crate::router::affinity::CacheUsage {
        prompt_tokens: u64::from(prompt),
        cache_read_tokens: u64::from(cache_read),
        cache_creation_tokens: u64::from(cache_creation),
    };
    let tenant = state
        .config
        .read()
        .routing
        .affinity_tenant_scoped
        .then_some(api_key_hash);
    let vendor = state
        .config
        .read()
        .vendors
        .iter()
        .find(|v| v.id == vendor_id)
        .cloned();
    let decision = match vendor {
        Some(vendor) => writer.settle(tenant, session_key, &vendor, usage).await,
        // 厂商配置已热删（竞态）：按缺省 vendor 级参数判定，不跳过粘性
        None => {
            let fallback = crate::config::VendorConfig {
                id: vendor_id.to_string(),
                name: vendor_id.to_string(),
                type_: String::new(),
                api_key_env: String::new(),
                api_key_envs: Vec::new(),
                extra_headers: Default::default(),
                api_base: String::new(),
                enabled: true,
                priority: 1,
                models: Vec::new(),
                rate_limit: crate::config::RateLimitConfig::Interval {
                    hours: 1,
                    sliding: false,
                },
                concurrency: crate::config::ConcurrencyConfig::default(),
                timeout_ms: 0,
                stream_ttft_ms: 0,
                stream_idle_ms: 0,
                supports_batch: false,
                chat_path: None,
                api_style: None,
                anthropic_version: None,
                forward_beta_headers: false,
                send_nonstandard_params: false,
                cache_ttl_secs: crate::config::loader::default_vendor_cache_ttl_secs(),
                silent_cache: false,
            };
            writer.settle(tenant, session_key, &fallback, usage).await
        }
    };
    Some(decision)
}

/// [`request_context_columns`] 的输出：直接展开进 `RequestLog` 的四列。
#[derive(Debug, Clone, Default)]
pub(crate) struct ContextColumns {
    pub session_key: Option<String>,
    pub context_window_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
}
