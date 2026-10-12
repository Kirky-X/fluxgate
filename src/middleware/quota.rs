// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 成本与 token 维度配额（WP8 预扣-回补）。
//!
//! 入口按 `预估输入 token×输入单价 + max_tokens×输出单价` 预扣，响应后/流结束后按
//! 实际 usage 回补（可负向返还、超预估追扣），超限 429 + Retry-After，与 rate_limit/
//! admission 独立叠加。账本以 std 原语实现固定窗口（limiteron 1.x 无 keyed
//! `(key, cost)` 原子消费/回补 API）；配置热读。透传流断流或无 usage 时不回补、保持预扣。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::config::{Config, ConfigManager, ModelCatalogEntry, PricingConfig};
use crate::i18n::t;
use crate::protocol::sse_frame::{
    reframe_passthrough_stream, reframe_passthrough_stream_with_usage, ReframeObservation,
};
use crate::utils::tenant::api_key_digest;
use crate::utils::tokens::estimate_messages_tokens_with;

/// 1 millicent = 1e-5 USD（整数计价避浮点误差）。
pub const MILLICENTS_PER_USD: f64 = 100_000.0;

/// 配额路径体上限（字节）：仅受限租户的适用路径缓冲时执行，超限 413。
const MAX_QUOTA_BODY_BYTES: usize = 4 * 1024 * 1024;

/// USD → millicents（向上取整，保守预扣方向；非有限/负值按 0）。
pub fn usd_to_millicents(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * MILLICENTS_PER_USD)
        .ceil()
        .clamp(0.0, u64::MAX as f64) as u64
}

/// 同模型跨 vendor 最高单价（每 1k tokens；入口 vendor 未定，保守取高；
/// 无登记返回 (0,0)，预扣零成本）。
pub fn max_unit_prices(pricing: &[PricingConfig], model: &str) -> (f64, f64) {
    let mut input = 0.0f64;
    let mut output = 0.0f64;
    for entry in pricing.iter().filter(|p| p.model_id == model) {
        if entry.input_per_1k_tokens.is_finite() && entry.input_per_1k_tokens > 0.0 {
            input = input.max(entry.input_per_1k_tokens);
        }
        if entry.output_per_1k_tokens.is_finite() && entry.output_per_1k_tokens > 0.0 {
            output = output.max(entry.output_per_1k_tokens);
        }
    }
    (input, output)
}

/// 档源解析单点：目录四档优先，未登记回退 `[[pricing]]` 两档；成本计算
/// 不允许第二套价目表。
enum PriceSource {
    /// 目录条目四档价（每 1M tokens，cache 两档可有值）
    Catalog { input: f64, output: f64 },
    /// `[[pricing]]` 回退两档价（每 1k tokens，无 cache 档）
    Pricing { input: f64, output: f64 },
}

fn resolve_price_source(
    catalog: &[ModelCatalogEntry],
    pricing: &[PricingConfig],
    model: &str,
) -> PriceSource {
    match crate::vendor::cost::catalog_unit_prices(catalog, model) {
        Some((input, output, _, _)) => PriceSource::Catalog { input, output },
        None => {
            let (input, output) = max_unit_prices(pricing, model);
            PriceSource::Pricing { input, output }
        }
    }
}

/// 预扣估算（入口与回补共用同一口径）：返回（预估总 token，预估成本 millicents）。
/// 模型缺失 → (0,0)；cache 两档不参与预估，只按回补实际计量。分档保留原始
/// 表达式（目录每 1M、回退每 1k），不来回换算。
pub fn precharge_for(
    body: &Value,
    catalog: &[ModelCatalogEntry],
    pricing: &[PricingConfig],
    chars_per_token: u64,
) -> (u64, u64) {
    let model = match body.get("model").and_then(Value::as_str) {
        Some(model) => model,
        None => return (0, 0),
    };
    let est_input = estimate_messages_tokens_with(body, chars_per_token);
    let max_tokens = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
    let usd = match resolve_price_source(catalog, pricing, model) {
        PriceSource::Catalog { input, output } => {
            est_input as f64 / 1_000_000.0 * input + max_tokens as f64 / 1_000_000.0 * output
        }
        PriceSource::Pricing { input, output } => {
            est_input as f64 / 1000.0 * input + max_tokens as f64 / 1000.0 * output
        }
    };
    (est_input.saturating_add(max_tokens), usd_to_millicents(usd))
}

/// 实际成本输入单束（usage 四档 + 档源 + 协议）：`model` 为档源命中键，
/// `vendor_type` 决定 cache 包含语义。
pub struct UsageCostInput<'a> {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub catalog: &'a [ModelCatalogEntry],
    pub pricing: &'a [PricingConfig],
    pub model: &'a str,
    pub vendor_type: &'a str,
}

/// 实际成本（回补用，与预扣同单价源）：四档全计，cache 语义与分账侧同一
/// 判定，仅余 ≤1 millicent 取整尾差。
pub fn actual_cost_millicents(input: &UsageCostInput<'_>) -> u64 {
    let UsageCostInput {
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        catalog,
        pricing,
        model,
        vendor_type,
    } = *input;
    let clamp = |v: u64| v.min(u32::MAX as u64) as u32;
    if let PriceSource::Catalog { .. } = resolve_price_source(catalog, pricing, model) {
        // 目录命中 → 走分账同一函数（Inclusive/Exclusive 基数折算）。
        let usage = crate::protocol::TokenUsage {
            prompt_tokens: clamp(prompt_tokens),
            completion_tokens: clamp(completion_tokens),
            cache_read_tokens: clamp(cache_read_tokens) as u64,
            cache_creation_tokens: clamp(cache_creation_tokens) as u64,
            ..Default::default()
        };
        if let Some(estimate) = crate::vendor::cost::estimate_cost_from_catalog(
            catalog,
            model,
            &usage,
            crate::vendor::cost::prompt_cache_semantics(vendor_type),
        ) {
            return usd_to_millicents(estimate.total_cost);
        }
    }
    // 回退档：`[[pricing]]` 无 cache 档可计，只计两档。
    let (in_price, out_price) = max_unit_prices(pricing, model);
    usd_to_millicents(
        prompt_tokens as f64 / 1000.0 * in_price + completion_tokens as f64 / 1000.0 * out_price,
    )
}

/// 响应 usage 提取（OpenAI/Anthropic 双形态；无 usage → None，调用方保持预扣），
/// 提数单源 `parse_usage_detailed`。
pub fn extract_usage_tokens(body: &Value) -> Option<(u64, u64)> {
    let (prompt, completion, _, _) = crate::protocol::parse_usage_detailed(body)?;
    Some((prompt as u64, completion as u64))
}

/// per-key 固定窗口账本（token/成本双维度；翻转惰性，读-判-写同一临界区）。
#[derive(Debug, Default)]
struct KeyWindow {
    token_used: u64,
    cost_used: u64,
    window_start: Option<Instant>,
}

/// 配额账本（`ApiState.quota` 承载，中间件与回补钩共享同一实例）。
#[derive(Debug, Default)]
pub struct QuotaState {
    ledgers: Mutex<HashMap<String, KeyWindow>>,
}

/// 高基数 key 内存约束（与 limiteron `QuotaLimiter` 10k 同级）。
const QUOTA_MAX_TRACKED_KEYS: usize = 10_000;

impl QuotaState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 窗口是否翻转（`None` 起点恒翻转；`window_secs` 由调用方热读传入）。
    fn flipped(window_start: Option<Instant>, now: Instant, window_secs: u64) -> bool {
        match window_start {
            None => true,
            Some(start) => now.duration_since(start) >= Duration::from_secs(window_secs.max(1)),
        }
    }

    /// 账本淘汰（consume/recompense 共用谓词）：超上限时仅保留未翻转窗口。
    fn evict_if_over_cap(guard: &mut HashMap<String, KeyWindow>, now: Instant, window_secs: u64) {
        if guard.len() > QUOTA_MAX_TRACKED_KEYS {
            guard.retain(|_, w| !Self::flipped(w.window_start, now, window_secs));
        }
    }

    /// 预扣（入口）：任一维度超限即拒（不落账），返回窗口剩余秒（≥1）；
    /// 两限额均为 0 → 直接放行（不建账）。
    pub fn consume(
        &self,
        key: &str,
        token_quota: u64,
        cost_quota: u64,
        pre_tokens: u64,
        pre_cost: u64,
        window_secs: u64,
    ) -> Result<(), u64> {
        if token_quota == 0 && cost_quota == 0 {
            return Ok(());
        }
        if pre_tokens == 0 && pre_cost == 0 {
            return Ok(());
        }
        let mut guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        Self::evict_if_over_cap(&mut guard, now, window_secs);
        let window = guard.entry(key.to_string()).or_default();
        if Self::flipped(window.window_start, now, window_secs) {
            window.token_used = 0;
            window.cost_used = 0;
            window.window_start = Some(now);
        }
        let remaining_secs = (Duration::from_secs(window_secs.max(1))
            .saturating_sub(now.duration_since(window.window_start.unwrap_or(now))))
        .as_secs()
        .max(1);
        let token_over =
            token_quota > 0 && pre_tokens > token_quota.saturating_sub(window.token_used);
        let cost_over = cost_quota > 0 && pre_cost > cost_quota.saturating_sub(window.cost_used);
        if token_over || cost_over {
            return Err(remaining_secs);
        }
        window.token_used = window.token_used.saturating_add(pre_tokens);
        window.cost_used = window.cost_used.saturating_add(pre_cost);
        Ok(())
    }

    /// 回补（响应后/流结束后）：按实际结算差额（可负向返还、超预估追扣）。
    /// **无账不建**：未落账时无差额可回，新建账只会把正向差额虚记进干净窗口。
    /// 饱和钳 0（回补不跨窗口追溯）。淘汰与 [`Self::consume`] 同谓词。
    pub fn recompense(
        &self,
        key: &str,
        pre_tokens: u64,
        pre_cost: u64,
        actual_tokens: u64,
        actual_cost: u64,
        window_secs: u64,
    ) {
        let mut guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        Self::evict_if_over_cap(&mut guard, now, window_secs);
        let Some(window) = guard.get_mut(key) else {
            return;
        };
        if Self::flipped(window.window_start, now, window_secs) {
            window.token_used = 0;
            window.cost_used = 0;
            window.window_start = Some(now);
        }
        let token_delta = actual_tokens as i128 - pre_tokens as i128;
        let cost_delta = actual_cost as i128 - pre_cost as i128;
        window.token_used = (window.token_used as i128 + token_delta).max(0) as u64;
        window.cost_used = (window.cost_used as i128 + cost_delta).max(0) as u64;
    }

    /// 窗口用量快照（单测断言用）。
    #[cfg(test)]
    pub fn snapshot(&self, key: &str) -> (u64, u64) {
        let guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        match guard.get(key) {
            Some(w) => (w.token_used, w.cost_used),
            None => (0, 0),
        }
    }

    /// 在册 key 数（单测断言淘汰用）。
    #[cfg(test)]
    pub fn tracked_keys(&self) -> usize {
        self.ledgers.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// per-key 限额解析（热配置现读）：digest 匹配 `[[auth.keys]]` 条目 →
/// (token_quota, cost_quota)；未声明 → (0,0) 不限。
fn quotas_for(config_manager: &ConfigManager, tenant: &str) -> (u64, u64) {
    match config_manager.read().auth.as_ref() {
        Some(auth) => match auth.keys.iter().find(|k| k.hash == tenant) {
            Some(entry) => (entry.token_quota, entry.cost_quota_millicents),
            None => (0, 0),
        },
        None => (0, 0),
    }
}

/// 配额中间件注入面（最小接口）：只携带判限所需两项数据，经 `from_fn_with_state` 注入。
#[derive(Clone)]
pub struct QuotaGate {
    pub config: Arc<ConfigManager>,
    pub quota: Option<Arc<QuotaState>>,
}

/// 配额适用性判定产物：账本句柄 + 配置快照（`Arc<Config>` 共享，零深拷贝）+ 双限额。
struct QuotaLease {
    quota: Arc<QuotaState>,
    config: Arc<Config>,
    token_quota: u64,
    cost_quota: u64,
}

/// 配额适用性三级判定（入口/透传流/转换流共用单点）：未接线 /
/// `[quota].enabled = false` / 该租户双限额为 0 → `None`（调用方零行为直通）。
fn quota_applies(
    config_manager: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
) -> Option<QuotaLease> {
    let quota = quota?;
    let snapshot = config_manager.read();
    if !snapshot.quota.enabled {
        return None;
    }
    let (token_quota, cost_quota) = quotas_for(config_manager, tenant);
    if token_quota == 0 && cost_quota == 0 {
        return None;
    }
    Some(QuotaLease {
        quota: quota.clone(),
        config: snapshot,
        token_quota,
        cost_quota,
    })
}

/// 配额中间件（WP8 入口裁决）：仅 `/v1/chat/completions` 与 `/v1/messages`；
/// 关闭/无账本/双限额不限时零行为透传（限额判定先于体读取）。超限 429 +
/// Retry-After + i18n 文案。
pub async fn quota_middleware(
    State(gate): State<QuotaGate>,
    request: Request,
    next: Next,
) -> Response {
    // 零分配路径判定（matches! 不产生 String）。
    let applicable = matches!(
        request.uri().path(),
        "/v1/chat/completions" | "/v1/messages"
    );
    let (parts, body) = request.into_parts();
    // 非适用路径：body 未触碰，原样透传（零拷贝）。
    if !applicable {
        return next.run(Request::from_parts(parts, body)).await;
    }
    // 租户摘要：认证中间件在外层已放行，现场计算。
    let tenant = api_key_digest(&parts.headers);
    let Some(lease) = quota_applies(&gate.config, gate.quota.as_ref(), &tenant) else {
        return next.run(Request::from_parts(parts, body)).await;
    };
    let QuotaLease {
        quota,
        config: snapshot,
        token_quota,
        cost_quota,
    } = lease;
    // `snapshot` 为 Arc<Config>（非锁守卫）：持至本请求结束，判限全程零深拷贝。
    let cpt = snapshot.quota.est_chars_per_token.max(1);
    let window_secs = snapshot.quota.window_secs.max(1);

    // 受限租户才缓冲与解析（handler 经 Json 提取器自行解析，不跨层传递已解析体）。
    let body_bytes = match axum::body::to_bytes(body, MAX_QUOTA_BODY_BYTES).await {
        Ok(bytes) => bytes,
        // 读体失败（超限/断流）：显性 413，不吞错转发空体。
        Err(_) => {
            tracing::warn!(
                path = %parts.uri.path(),
                cap_bytes = MAX_QUOTA_BODY_BYTES,
                "{}",
                t("middleware-quota-body-too-large")
            );
            return (
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                t("middleware-quota-body-too-large"),
            )
                .into_response();
        }
    };
    let parsed: Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        // 非法体：判限跳过，下游以 4xx 校验（fail-open）。
        Err(_) => {
            return next
                .run(Request::from_parts(
                    parts,
                    axum::body::Body::from(body_bytes),
                ))
                .await;
        }
    };
    let (pre_tokens, pre_cost) =
        precharge_for(&parsed, &snapshot.model_catalog, &snapshot.pricing, cpt);
    match quota.consume(
        &tenant,
        token_quota,
        cost_quota,
        pre_tokens,
        pre_cost,
        window_secs,
    ) {
        // 预扣成功：body 原样重装交 handler。
        Ok(()) => {
            next.run(Request::from_parts(
                parts,
                axum::body::Body::from(body_bytes),
            ))
            .await
        }
        Err(retry_secs) => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry_secs.max(1).to_string())],
            t("middleware-quota-exceeded"),
        )
            .into_response(),
    }
}

/// 非流式回补钩（响应后）：有 usage 才结算（无 usage 保持预扣）。
/// `quota` 未接线/未启用/无限额时 no-op。
pub fn settle_nonstream(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
    response_body: &Value,
    vendor_type: &str,
) {
    let Some(lease) = quota_applies(config, quota, tenant) else {
        return;
    };
    let quota = lease.quota;
    let snapshot = lease.config;
    let Some((prompt, completion, cache_read, cache_creation)) =
        crate::protocol::parse_usage_detailed(response_body)
    else {
        return;
    };
    let cpt = snapshot.quota.est_chars_per_token.max(1);
    let window_secs = snapshot.quota.window_secs.max(1);
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let (pre_tokens, pre_cost) =
        precharge_for(body, &snapshot.model_catalog, &snapshot.pricing, cpt);
    let actual_tokens = (prompt as u64).saturating_add(completion as u64);
    let actual_cost = actual_cost_millicents(&UsageCostInput {
        prompt_tokens: prompt as u64,
        completion_tokens: completion as u64,
        cache_read_tokens: cache_read as u64,
        cache_creation_tokens: cache_creation as u64,
        catalog: &snapshot.model_catalog,
        pricing: &snapshot.pricing,
        model,
        vendor_type,
    });
    quota.recompense(
        tenant,
        pre_tokens,
        pre_cost,
        actual_tokens,
        actual_cost,
        window_secs,
    );
}

/// 缓存 HIT 结算：命中无上游往返、实际用量为 0，以 0 结算回补预扣；
/// 门控与入口一致（未接线/未启用/无限额时 no-op）。
pub fn settle_nonstream_cached(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
) {
    let Some(lease) = quota_applies(config, quota, tenant) else {
        return;
    };
    let snapshot = lease.config;
    let cpt = snapshot.quota.est_chars_per_token.max(1);
    let window_secs = snapshot.quota.window_secs.max(1);
    let (pre_tokens, pre_cost) =
        precharge_for(body, &snapshot.model_catalog, &snapshot.pricing, cpt);
    lease
        .quota
        .recompense(tenant, pre_tokens, pre_cost, 0, 0, window_secs);
}

/// 结算上下文（转换流）：账本句柄、租户与预扣口径一次构造，上下文不持有请求体
///（整段对话不在流存活期常驻内存）。`for_request` 内置三级早退——不适用返回
/// `None`，调用方零包装直通。
pub struct SettlementCtx {
    quota: Arc<QuotaState>,
    tenant: String,
    model: String,
    vendor_type: String,
    /// 配置快照共享（`Arc<Config>` 引用，零深拷贝；热更换新快照不影响
    /// 已建立流的结算口径）。
    config: Arc<Config>,
    pre_tokens: u64,
    pre_cost: u64,
}

impl SettlementCtx {
    pub fn for_request(
        config: &Arc<ConfigManager>,
        quota: Option<&Arc<QuotaState>>,
        tenant: &str,
        body: &Value,
        vendor_type: &str,
    ) -> Option<Self> {
        let QuotaLease {
            quota,
            config: snapshot,
            ..
        } = quota_applies(config, quota, tenant)?;
        let (pre_tokens, pre_cost) = precharge_for(
            body,
            &snapshot.model_catalog,
            &snapshot.pricing,
            snapshot.quota.est_chars_per_token.max(1),
        );
        Some(Self {
            quota,
            tenant: tenant.to_string(),
            model: body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            vendor_type: vendor_type.to_string(),
            config: snapshot,
            pre_tokens,
            pre_cost,
        })
    }
}

/// 转换流结算包装（流结束后）：捕获末个 `Usage` chunk（含 cache 两档），流正常
/// 结束时回补；`ctx = None`（三级早退不适用）时仅原样转发。透传流结算见
/// [`passthrough_stream_body`]。
pub fn settle_stream<S>(
    stream: S,
    ctx: Option<SettlementCtx>,
) -> impl futures::Stream<Item = Result<crate::protocol::StreamChunk, crate::error::GateError>>
where
    S: futures::Stream<Item = Result<crate::protocol::StreamChunk, crate::error::GateError>>
        + Send
        + 'static,
{
    async_stream::stream! {
        use futures::StreamExt as _;
        let mut stream = Box::pin(stream);
        // 流式用量结算（取最后一次出现的值；cache 两档随 chunk 携带）。
        let mut last_usage: Option<(u32, u32, u32, u32)> = None;
        while let Some(item) = stream.next().await {
            if let Ok(crate::protocol::StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                ..
            }) = &item
            {
                last_usage = Some((
                    *prompt_tokens,
                    *completion_tokens,
                    *cache_read_tokens,
                    *cache_creation_tokens,
                ));
            }
            yield item;
        }
        // 流结束：按实际回补（无 Usage 帧 → 保持预扣）；预扣值构造期已算好，此处不重算。
        if let (Some(ctx), Some((prompt, completion, cache_read, cache_creation))) =
            (ctx, last_usage)
        {
            let actual_cost = actual_cost_millicents(&UsageCostInput {
                prompt_tokens: prompt as u64,
                completion_tokens: completion as u64,
                cache_read_tokens: cache_read as u64,
                cache_creation_tokens: cache_creation as u64,
                catalog: &ctx.config.model_catalog,
                pricing: &ctx.config.pricing,
                model: &ctx.model,
                vendor_type: &ctx.vendor_type,
            });
            ctx.quota.recompense(
                &ctx.tenant,
                ctx.pre_tokens,
                ctx.pre_cost,
                (prompt as u64).saturating_add(completion as u64),
                actual_cost,
                ctx.config.quota.window_secs.max(1),
            );
        }
    }
}

/// 透传流帧内 usage 提取：Anthropic `message_start` 把 usage 嵌在 `message` 下，
/// 其余在顶层；提数一律交给 `parse_usage_detailed`（包成 `{"usage": …}` 复用同一分支）。
fn frame_usage(data: &str) -> Option<(u64, u64, u64, u64)> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return None;
    }
    // 廉价预筛：usage 只存在于含 "usage" 子串的帧——内容帧绝大多数不含，
    // 直接跳过全量 JSON 解析。
    if !trimmed.contains("usage") {
        return None;
    }
    let value = serde_json::from_str::<Value>(trimmed).ok()?;
    let usage = value
        .get("usage")
        .or_else(|| value.get("message")?.get("usage"))?;
    // 流尾帧（Anthropic `message_delta`）只带 `output_tokens`：补
    // `input_tokens = 0` 后仍交同一提数实现，配 `merge_usage` 取最大值得终值。
    let mut usage_obj = usage.clone();
    if usage_obj.get("prompt_tokens").is_none() && usage_obj.get("input_tokens").is_none() {
        // 既无 prompt/input 也无 output 的帧不含可用用量 → 跳过。
        usage_obj.get("output_tokens")?;
        usage_obj["input_tokens"] = Value::from(0);
    }
    let wrapped = serde_json::json!({ "usage": usage_obj });
    let (prompt, completion, cache_read, cache_creation) =
        crate::protocol::parse_usage_detailed(&wrapped)?;
    Some((
        prompt as u64,
        completion as u64,
        cache_read as u64,
        cache_creation as u64,
    ))
}

/// 观测字段合并：各档取最大值。Anthropic 的 `message_start`（input/cache 两档）与
/// `message_delta`（output）分帧上报且 output 单调递增——取最大即末值，无需按协议分支。
fn merge_usage(prev: (u64, u64, u64, u64), next: (u64, u64, u64, u64)) -> (u64, u64, u64, u64) {
    (
        prev.0.max(next.0),
        prev.1.max(next.1),
        prev.2.max(next.2),
        prev.3.max(next.3),
    )
}

/// 透传流结算器：累计观测到的 usage，正常 EOF 时按实际回补。预扣口径与非流式/
/// 转换流共用 [`precharge_for`]，成本口径共用 [`actual_cost_millicents`]。
struct PassthroughSettler {
    quota: Arc<QuotaState>,
    tenant: String,
    model: String,
    vendor_type: String,
    config: Arc<Config>,
    pre_tokens: u64,
    pre_cost: u64,
    usage: Option<(u64, u64, u64, u64)>,
}

impl PassthroughSettler {
    fn observe(&mut self, observation: ReframeObservation) {
        match observation {
            ReframeObservation::Frame { data, .. } => {
                let Some(data) = data else { return };
                if let Some(usage) = frame_usage(&data) {
                    self.usage = Some(match self.usage.take() {
                        Some(prev) => merge_usage(prev, usage),
                        None => usage,
                    });
                }
            }
            // 正常收束才回补；断流/错误帧保持预扣。
            ReframeObservation::End { completed: true } => self.settle(),
            ReframeObservation::End { completed: false } => {}
        }
    }

    fn settle(&self) {
        // 全程无 usage（如 OpenAI 透传不带 include_usage）→ 不回补。
        let Some((prompt, completion, cache_read, cache_creation)) = self.usage else {
            return;
        };
        let actual_cost = actual_cost_millicents(&UsageCostInput {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            catalog: &self.config.model_catalog,
            pricing: &self.config.pricing,
            model: &self.model,
            vendor_type: &self.vendor_type,
        });
        // `actual_tokens` 与转换流同口径 = prompt + completion（cache 两档不
        // 并入 token 窗基数）。
        self.quota.recompense(
            &self.tenant,
            self.pre_tokens,
            self.pre_cost,
            prompt.saturating_add(completion),
            actual_cost,
            self.config.quota.window_secs.max(1),
        );
    }
}

/// 透传流响应体装配：仅当配额适用（已接线、`[quota].enabled` 且该 tenant
/// 任一限额 > 0，判定单点 [`quota_applies`]）时挂帧观测包装；其余形态走无观测
/// reframe（零额外分配）。
///
/// 上游错误类型仅需 `Display`（与 reframe 同一约束），输出统一为
/// `Result<Bytes, std::io::Error>` 供 `Body::from_stream` 使用。
pub fn passthrough_stream_body<S, E>(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
    vendor_type: &str,
    stream: S,
) -> Body
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    // 三级早退单点：未接线 / 关闭 / 无限额 → 无观测直通。
    let Some(lease) = quota_applies(config, quota, tenant) else {
        return Body::from_stream(reframe_passthrough_stream(stream));
    };
    let quota = lease.quota;
    let snapshot = lease.config;
    let (pre_tokens, pre_cost) = precharge_for(
        body,
        &snapshot.model_catalog,
        &snapshot.pricing,
        snapshot.quota.est_chars_per_token.max(1),
    );
    let settler = Arc::new(Mutex::new(PassthroughSettler {
        quota,
        tenant: tenant.to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        vendor_type: vendor_type.to_string(),
        config: snapshot,
        pre_tokens,
        pre_cost,
        usage: None,
    }));
    Body::from_stream(reframe_passthrough_stream_with_usage(
        stream,
        move |observation| {
            settler
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .observe(observation);
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Vec<PricingConfig> {
        vec![crate::config::PricingConfig {
            vendor_id: "v".into(),
            model_id: "m".into(),
            input_per_1k_tokens: 1.0,
            output_per_1k_tokens: 3.0,
        }]
    }

    fn chat_body(messages: &str, max_tokens: u64) -> Value {
        serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": messages}],
            "max_tokens": max_tokens,
        })
    }

    /// 预扣公式：est 输入×输入单价 + max_tokens×输出单价（向上取整）；
    /// 目录未登记时回退 `[[pricing]]`（每 1k）。
    #[test]
    fn precharge_formula_matches_spec() {
        // "hello!" 6 字符 → 2 token + 8 消息开销 = 10；max 10 → 总 20；
        // 成本 0.04 USD = 4000 millicents。
        let (tokens, cost) = precharge_for(&chat_body("hello!", 10), &[], &pricing(), 4);
        assert_eq!(tokens, 20);
        assert_eq!(cost, 4000);
    }

    fn catalog(
        input: f64,
        output: f64,
        cache_read: f64,
        cache_creation: f64,
    ) -> Vec<ModelCatalogEntry> {
        vec![ModelCatalogEntry {
            model_id: "m".into(),
            context_window: 1000,
            input_price: input,
            output_price: output,
            cache_read_price: cache_read,
            cache_creation_price: cache_creation,
            ..Default::default()
        }]
    }

    /// 档源优先：目录命中时预扣取目录价（每 1M），不再看 `[[pricing]]`。
    #[test]
    fn precharge_prefers_catalog_over_pricing() {
        // 目录 input/output = 2000/6000 每 1M（即 2.0/6.0 每 1k），而
        // `[[pricing]]` 为 1.0/3.0；预估 10/10 token → 0.08 USD = 8000 millicents。
        let (tokens, cost) = precharge_for(
            &chat_body("hello!", 10),
            &catalog(2000.0, 6000.0, 0.0, 0.0),
            &pricing(),
            4,
        );
        assert_eq!(tokens, 20);
        assert_eq!(cost, 8000, "目录价必须优先于 [[pricing]]");
    }

    /// 实际成本输入束（model 固定 "m"，与 [`chat_body`] 同源）。
    fn cost_input<'a>(
        prompt: u64,
        completion: u64,
        cache_read: u64,
        cache_creation: u64,
        cat: &'a [ModelCatalogEntry],
        pricing_ref: &'a [PricingConfig],
        vendor_type: &'a str,
    ) -> UsageCostInput<'a> {
        UsageCostInput {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            catalog: cat,
            pricing: pricing_ref,
            model: "m",
            vendor_type,
        }
    }

    /// 四档全计：cache 两档进入成本口径（OpenAI Inclusive 基数不重复计费），
    /// 且结果与分账侧 `estimate_cost_from_catalog` 同一数值（单源）。
    #[test]
    fn actual_cost_includes_cache_tiers_matching_catalog_semantics() {
        let cat = catalog(1000.0, 2000.0, 300.0, 3750.0);
        // prompt 1000（含 cached 600）+ completion 100 + cache_creation 200：
        // Inclusive 基数 = 1000-600-200 = 200。
        let cost =
            actual_cost_millicents(&cost_input(1000, 100, 600, 200, &cat, &pricing(), "openai"));
        let expected_usd = 200.0 / 1e6 * 1000.0
            + 100.0 / 1e6 * 2000.0
            + 600.0 / 1e6 * 300.0
            + 200.0 / 1e6 * 3750.0;
        assert_eq!(cost, usd_to_millicents(expected_usd), "与目录四档式一致");
        // 旧两档口径（忽略 cache）必然更低。
        let legacy =
            actual_cost_millicents(&cost_input(1000, 100, 0, 0, &cat, &pricing(), "openai"));
        assert!(
            cost > legacy,
            "cache 两档必须进入成本口径：{cost} vs {legacy}"
        );
        // Anthropic Exclusive：prompt 不含 cache 档，基数不缩减（价更高）。
        let exclusive = actual_cost_millicents(&cost_input(
            1000,
            100,
            600,
            200,
            &cat,
            &pricing(),
            "anthropic",
        ));
        assert!(
            exclusive > cost,
            "Exclusive 不扣非缓存基数，应比 Inclusive 贵：{exclusive} vs {cost}"
        );
    }

    /// 目录未登记 → 回退 `[[pricing]]` 两档，cache 参数不参与计价。
    #[test]
    fn actual_cost_falls_back_to_pricing_when_model_absent() {
        let cost = actual_cost_millicents(&cost_input(10, 10, 600, 200, &[], &pricing(), "openai"));
        // 10/1000*1.0 + 10/1000*3.0 = 0.04 USD
        assert_eq!(cost, 4000);
    }

    /// 帧内 usage 提取三形态：Anthropic `message_start`（嵌套 message.usage，
    /// 含 cache 两档）、`message_delta`（仅 output_tokens）、OpenAI 尾帧（含
    /// prompt_tokens_details.cached_tokens）；`[DONE]`/非 JSON/无 usage → None。
    #[test]
    fn frame_usage_covers_stream_shapes() {
        let start = r#"{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":120,"cache_read_input_tokens":60,"cache_creation_input_tokens":30,"output_tokens":1}}}"#;
        assert_eq!(frame_usage(start), Some((120, 1, 60, 30)));
        let delta = r#"{"type":"message_delta","usage":{"output_tokens":44}}"#;
        assert_eq!(
            frame_usage(delta),
            Some((0, 44, 0, 0)),
            "尾帧仅 output_tokens 时 prompt 档按 0 交同一提数实现"
        );
        let openai = r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":20}}}"#;
        assert_eq!(frame_usage(openai), Some((50, 7, 20, 0)));
        assert_eq!(frame_usage("[DONE]"), None);
        assert_eq!(frame_usage("keepalive"), None);
        assert_eq!(frame_usage(r#"{"choices":[]}"#), None);
        // 预筛钉住：含 "usage" 字样但无 usage 字段的内容帧 → None。
        assert_eq!(
            frame_usage(r#"{"choices":[{"delta":{"content":"usage quota"}}]}"#),
            None
        );
    }

    /// 分帧合并取各档最大：start 与 delta 合并得全量终值（output 不回退为 0）。
    #[test]
    fn merge_usage_keeps_max_per_tier() {
        let merged = merge_usage(
            (120, 1, 60, 30),
            frame_usage(&String::from(r#"{"usage":{"output_tokens":44}}"#)).unwrap(),
        );
        assert_eq!(merged, (120, 44, 60, 30), "output 取末值且其余档不回退");
    }

    /// 回补返还：实际低于预估时余额返还。
    #[test]
    fn recompense_refunds_difference() {
        let quota = QuotaState::new();
        // 限额 100 token：预扣 60 通过。
        quota.consume("k", 100, 0, 60, 0, 3600).expect("首次预扣");
        assert_eq!(quota.snapshot("k"), (60, 0));
        // 实际仅 10 → 回补返还 50。
        quota.recompense("k", 60, 0, 10, 0, 3600);
        assert_eq!(quota.snapshot("k"), (10, 0));
        // 实际超预估时追扣。
        quota.recompense("k", 10, 0, 30, 0, 3600);
        assert_eq!(quota.snapshot("k"), (30, 0));
    }

    /// 回补不建账：无账可回时保持账本为空，正向差额不得虚记进干净窗口。
    #[test]
    fn recompense_without_ledger_does_not_create_account() {
        let quota = QuotaState::new();
        quota.recompense("ghost", 60, 0, 10, 0, 3600);
        assert_eq!(quota.tracked_keys(), 0, "无账不得新建");
        assert_eq!(quota.snapshot("ghost"), (0, 0));
    }

    /// 回补触发淘汰：超上限时惰性清理已翻转窗口（与 consume 同谓词）。
    #[test]
    fn recompense_triggers_window_eviction() {
        let quota = QuotaState::new();
        // 灌满上限 + 1（淘汰谓词严格大于）——窗口 1s，随后全部翻转。
        for i in 0..=QUOTA_MAX_TRACKED_KEYS {
            quota
                .consume(&format!("k{i}"), 100, 0, 1, 0, 1)
                .expect("fill");
        }
        assert_eq!(quota.tracked_keys(), QUOTA_MAX_TRACKED_KEYS + 1);
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        // 无账 key 上的回补也要先执行淘汰（谓词同一），且不建账。
        quota.recompense("overflow", 0, 0, 0, 0, 1);
        assert!(
            quota.tracked_keys() <= QUOTA_MAX_TRACKED_KEYS,
            "超上限必须淘汰已翻转窗口"
        );
        assert_eq!(quota.tracked_keys(), 0, "全部窗口已翻转 → 全清");
    }

    /// 超限拒收且不落账。
    #[test]
    fn over_quota_rejected_without_ledger_pollution() {
        let quota = QuotaState::new();
        quota.consume("k", 100, 0, 60, 0, 3600).expect("首次");
        assert!(quota.consume("k", 100, 0, 60, 0, 3600).is_err());
        assert_eq!(quota.snapshot("k"), (60, 0), "拒收不得落账");
    }

    /// usage 双形态提取；无 usage → None（保持预扣）。提数委派
    /// `parse_usage_detailed`，单侧字段缺失按 0 计。
    #[test]
    fn usage_extraction_covers_both_shapes() {
        let openai = serde_json::json!({"usage": {"prompt_tokens": 5, "completion_tokens": 7}});
        assert_eq!(extract_usage_tokens(&openai), Some((5, 7)));
        let anthropic = serde_json::json!({"usage": {"input_tokens": 5, "output_tokens": 7}});
        assert_eq!(extract_usage_tokens(&anthropic), Some((5, 7)));
        assert_eq!(extract_usage_tokens(&serde_json::json!({})), None);
        assert_eq!(
            extract_usage_tokens(&serde_json::json!({"usage": {"prompt_tokens": 5}})),
            Some((5, 0)),
            "单侧缺失按 0 计（与分账侧同一口径）"
        );
    }

    /// usd 取整方向保守（向上），非法输入归零。
    #[test]
    fn millicents_conversion_is_conservative() {
        assert_eq!(usd_to_millicents(0.032), 3200);
        assert_eq!(usd_to_millicents(0.0), 0);
        assert_eq!(usd_to_millicents(-1.0), 0);
        assert_eq!(usd_to_millicents(f64::NAN), 0);
    }

    // ==================== 结算门控与 Arc<Config> 快照 ====================

    /// quota 开启 + 已知租户限额的配置（结算门控单测共用）。
    fn quota_enabled_config(enabled: bool) -> crate::config::Config {
        crate::config::Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: pricing(),
            model_catalog: Vec::new(),
            auth: Some(crate::config::AuthSection {
                enabled: true,
                api_keys: vec![],
                header: Default::default(),
                keys: vec![crate::config::AuthKeyEntry {
                    hash: "tenant-digest".into(),
                    tier: crate::config::AuthTier::Elastic,
                    token_quota: 1_000_000,
                    cost_quota_millicents: 0,
                }],
            }),
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: crate::config::QuotaSection {
                enabled,
                ..Default::default()
            },
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
        }
    }

    /// 三级早退单点：适用（Some）/ 双限额 0 / enabled=false / 未接线（None）。
    #[test]
    fn quota_applies_three_level_gate() {
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let quota = Arc::new(QuotaState::new());

        let applied = quota_applies(&config, Some(&quota), "tenant-digest");
        assert!(
            applied.is_some_and(|lease| lease.token_quota == 1_000_000 && lease.cost_quota == 0),
            "已启用 + 租户有限额 → 适用，限额随快照返回"
        );

        assert!(
            quota_applies(&config, Some(&quota), "unknown-tenant").is_none(),
            "无限额租户不适用（H1 零解析路径）"
        );
        let off = Arc::new(ConfigManager::new(quota_enabled_config(false)));
        assert!(
            quota_applies(&off, Some(&quota), "tenant-digest").is_none(),
            "enabled=false 不适用"
        );
        assert!(
            quota_applies(&config, None, "tenant-digest").is_none(),
            "账本未接线不适用"
        );
    }

    /// SettlementCtx 同一三级早退（转换流装配入口）。
    #[test]
    fn settlement_ctx_gated_by_quota_applies() {
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let quota = Arc::new(QuotaState::new());
        let body = chat_body("hello!", 10);
        assert!(SettlementCtx::for_request(
            &config,
            Some(&quota),
            "tenant-digest",
            &body,
            "openai"
        )
        .is_some());
        assert!(
            SettlementCtx::for_request(&config, Some(&quota), "unknown", &body, "openai").is_none(),
            "无限额租户不构建 ctx（H2：默认配置零克隆）"
        );
    }

    /// 缓存 HIT 结算：入口预扣后以 0 结算 → 预扣全额回补，重复命中不虚耗配额。
    #[tokio::test]
    async fn cached_settlement_refunds_full_precharge() {
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let quota = Arc::new(QuotaState::new());
        let body = chat_body("hello!", 10);
        let (pre_tokens, pre_cost) = precharge_for(&body, &[], &pricing(), 4);
        quota
            .consume("tenant-digest", 1_000_000, 0, pre_tokens, pre_cost, 3600)
            .expect("入口预扣");
        assert_eq!(quota.snapshot("tenant-digest"), (pre_tokens, pre_cost));

        settle_nonstream_cached(&config, Some(&quota), "tenant-digest", &body);
        assert_eq!(
            quota.snapshot("tenant-digest"),
            (0, 0),
            "HIT 以实际用量 0 结算，预扣全额回补"
        );

        // 门控形态：未接线 → no-op（不 panic、不建账）。
        let other = QuotaState::new();
        settle_nonstream_cached(&config, None, "tenant-digest", &body);
        assert_eq!(other.tracked_keys(), 0);
    }
}
