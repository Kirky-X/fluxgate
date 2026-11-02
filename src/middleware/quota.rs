// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 成本与 token 维度配额（预扣-回补）。
//!
//! 入口按 `预估输入 token×输入单价 + max_tokens×输出单价` 预扣，响应后/流结束后按
//! 实际 usage 回补（可负向返还、超预估追扣），超限 429 + Retry-After，与 rate_limit/
//! admission 独立叠加。账本以 std 原语实现固定窗口（limiteron 1.x 无 keyed
//! `(key, cost)` 原子消费/回补 API）；配置热读。透传流断流或无 usage 时不回补、保持预扣。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::config::{
    effective_quota_windows, Config, ConfigManager, ModelCatalogEntry, PricingConfig,
};
use crate::i18n::{t, t_args};
use crate::protocol::sse_frame::{reframe_passthrough_stream_with_usage, ReframeObservation};
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
#[derive(Clone, Copy)]
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
    // rerank 形态（documents 数组且无 messages）：按 query+documents 字符数
    // 折算 token，成本按目录每 1k 文档价（与 chat 的 token 口径分离）
    if body.get("messages").is_none() && body.get("documents").is_some() {
        return precharge_rerank(body, catalog, chars_per_token);
    }
    // Responses 形态（无 messages 但有 input/instructions）：按 items/
    // instructions/tools 字符折算 token，成本按 catalog/pricing 单价
    if body.get("messages").is_none()
        && (body.get("input").is_some() || body.get("instructions").is_some())
    {
        return precharge_responses(body, catalog, pricing, chars_per_token);
    }
    // Gemini 形态（contents / content / requests）：按 parts[].text +
    // systemInstruction + tools 字符折算，成本按 catalog/pricing 单价
    if body.get("messages").is_none()
        && (body.get("contents").is_some()
            || body.get("content").is_some()
            || body.get("requests").is_some())
    {
        return precharge_gemini(body, catalog, pricing, chars_per_token);
    }
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

/// Responses 预扣：token = ceil((instructions + input JSON + tools JSON)
/// 字符 / chars_per_token) + `max_output_tokens`；成本按 catalog/pricing
/// 单价（与 chat 同口径）。
pub fn precharge_responses(
    body: &Value,
    catalog: &[ModelCatalogEntry],
    pricing: &[PricingConfig],
    chars_per_token: u64,
) -> (u64, u64) {
    let cpt = chars_per_token.max(1) as usize;
    let instructions_chars = body
        .get("instructions")
        .and_then(Value::as_str)
        .map(|text| text.chars().count())
        .unwrap_or(0);
    let input_chars = body
        .get("input")
        .and_then(|input| serde_json::to_string(input).ok())
        .map(|text| text.chars().count())
        .unwrap_or(0);
    let tools_chars = body
        .get("tools")
        .and_then(|tools| serde_json::to_string(tools).ok())
        .map(|text| text.chars().count())
        .unwrap_or(0);
    let total_chars = instructions_chars
        .saturating_add(input_chars)
        .saturating_add(tools_chars);
    let est_input = (total_chars.div_ceil(cpt)) as u64;
    let max_tokens = body
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
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

/// Gemini 预扣：token = ceil((Σ parts[].text + systemInstruction + tools JSON)
/// 字符 / chars_per_token) + `generationConfig.maxOutputTokens`；embedding
/// 形态无输出预算，只计输入。成本按 catalog/pricing 单价（与 chat 同口径）。
pub fn precharge_gemini(
    body: &Value,
    catalog: &[ModelCatalogEntry],
    pricing: &[PricingConfig],
    chars_per_token: u64,
) -> (u64, u64) {
    let cpt = chars_per_token.max(1) as usize;
    let mut chars = 0usize;
    // chat：contents[] 与 systemInstruction
    if let Some(contents) = body.get("contents").and_then(Value::as_array) {
        for content in contents {
            chars += parts_text_chars(content.get("parts"));
        }
    }
    chars += parts_text_chars(
        body.get("systemInstruction")
            .or_else(|| body.get("system_instruction"))
            .and_then(|system| system.get("parts")),
    );
    // embedding：单条 content / 批量 requests[].content
    if let Some(content) = body.get("content") {
        chars += parts_text_chars(content.get("parts"));
    }
    if let Some(requests) = body.get("requests").and_then(Value::as_array) {
        for request in requests {
            chars += parts_text_chars(
                request
                    .get("content")
                    .and_then(|content| content.get("parts")),
            );
        }
    }
    if let Some(tools) = body.get("tools") {
        chars += serde_json::to_string(tools)
            .map(|json| json.chars().count())
            .unwrap_or(0);
    }
    let est_input = chars.div_ceil(cpt) as u64;
    let max_tokens = body
        .get("generationConfig")
        .or_else(|| body.get("generation_config"))
        .and_then(|config| config.get("maxOutputTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
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

/// parts[].text 字符数（非文本 part 不计）。
fn parts_text_chars(parts: Option<&Value>) -> usize {
    parts
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .map(|text| text.chars().count())
                .sum()
        })
        .unwrap_or(0)
}

/// 从 Gemini 路径提取 model：`/v1beta/models/<model>:<action>` → `<model>`。
pub fn gemini_model_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    let model = rest.split(':').next()?.trim();
    (!model.is_empty()).then(|| model.strip_prefix("models/").unwrap_or(model).to_string())
}

/// rerank 预扣：token = ceil((query 字符 + Σ 文档字符) / chars_per_token)；
/// 成本 = 目录每 1k 文档价 × 文档数 / 1000（USD → millicents，未登记价 = 0）。
pub fn precharge_rerank(
    body: &Value,
    catalog: &[ModelCatalogEntry],
    chars_per_token: u64,
) -> (u64, u64) {
    let cpt = chars_per_token.max(1) as usize;
    let query_chars = body
        .get("query")
        .and_then(Value::as_str)
        .map(|query| query.chars().count())
        .unwrap_or(0);
    let documents = body.get("documents").and_then(Value::as_array);
    let doc_chars: usize = documents
        .map(|docs| {
            docs.iter()
                .map(|doc| match doc {
                    Value::String(text) => text.chars().count(),
                    Value::Object(map) => map
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| text.chars().count())
                        .unwrap_or(0),
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or(0);
    let total_chars = query_chars.saturating_add(doc_chars);
    let tokens = total_chars.div_ceil(cpt) as u64;
    let doc_count = documents.map(|docs| docs.len()).unwrap_or(0);
    let usd = catalog
        .iter()
        .find(|entry| Some(entry.model_id.as_str()) == body.get("model").and_then(Value::as_str))
        .map(|entry| entry.rerank_price_per_1k_docs * doc_count as f64 / 1000.0)
        .unwrap_or(0.0);
    (tokens, usd_to_millicents(usd))
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

/// 窗口周期数学单源（[`crate::middleware::quota_period`]）：Period 编译、
/// 自然边界对齐与翻转判定拆为纯函数文件（日历数学可定向测试）。本模块
/// re-export `Period` 保持历史引用路径（`middleware::quota::Period`）。
pub use crate::middleware::quota_period::Period;
use crate::middleware::quota_period::{DAY_SECS, MAX_MONTH_SECS};

/// 单窗运行时判定面（热配置逐请求解析产物）：账本按 (账本键, window id) 存态。
/// `tokens_limit`/`cost_limit_millicents` 为已回填的生效限额（0 = 该维度不判）。
#[derive(Debug, Clone)]
pub struct RuntimeWindow {
    pub id: String,
    /// scope=global：全体请求共享 [`GLOBAL_LEDGER_KEY`] 账本键
    pub global: bool,
    pub period: Period,
    pub tz_offset_secs: i64,
    pub tokens_limit: u64,
    pub cost_limit_millicents: u64,
}

/// scope=global 窗的共享账本键（design 决策 3：全局窗行以固定键落账本）
pub const GLOBAL_LEDGER_KEY: &str = "__global__";

/// vendor 预算行的账本键（design 决策 4：key=`__vendor__:{vendor_id}`）
pub fn vendor_ledger_key(vendor_id: &str) -> String {
    format!("__vendor__:{vendor_id}")
}

/// vendor 预算行的窗口 id（design 决策 4：window_id=`vendor:{vendor_id}`）
pub fn vendor_window_id(vendor_id: &str) -> String {
    format!("vendor:{vendor_id}")
}

impl RuntimeWindow {
    /// 账本键：global 窗共享固定键，api_key 窗随租户摘要。
    fn ledger_key<'a>(&self, tenant: &'a str) -> &'a str {
        if self.global {
            GLOBAL_LEDGER_KEY
        } else {
            tenant
        }
    }
}

/// 单窗状态：used 为已预扣+已结算的窗口占用（预扣即预留，结算按差额
/// 修正——与历史 KeyWindow 同一 delta 语义，多窗只是按 id 分桶）；
/// `window_start` 为 Unix 秒（rolling=窗内首次用量；calendar=对齐边界）。
/// period/tz 随写入定格，热更换新窗集时同 id 窗在下次访问自然接管。
#[derive(Debug, Clone)]
struct WindowState {
    period: Period,
    tz_offset_secs: i64,
    token_used: u64,
    cost_used: u64,
    window_start: Option<i64>,
}

impl WindowState {
    fn new(window: &RuntimeWindow) -> Self {
        Self {
            period: window.period,
            tz_offset_secs: window.tz_offset_secs,
            token_used: 0,
            cost_used: 0,
            window_start: None,
        }
    }
}

/// 429 拒绝详情：`retry_after_secs` = 受拒窗中最晚重置秒数（所有窗须同时
/// 有余，最早可成功重试时刻 = 最晚重置的受拒窗——proposal「取最远重置窗」）；
/// `windows` 列全部生效窗（脱敏：仅 window id 与重置秒数，R-quota-001 约束）。
#[derive(Debug, Clone)]
pub struct QuotaRefusal {
    pub retry_after_secs: u64,
    pub windows: Vec<WindowRemainder>,
    /// 受拒窗 id 集（`quota_window_refused_total{window_id}` 打分标签）
    pub refused_ids: Vec<String>,
}

/// 单窗重置余量（429 响应体逐窗行；不含 key 信息）
#[derive(Debug, Clone)]
pub struct WindowRemainder {
    pub id: String,
    pub reset_secs: u64,
}

/// 429 多窗拒绝响应体：摘要行 + 逐窗重置秒数（i18n；脱敏 = 仅 id 与秒数）。
pub(crate) fn refusal_body(refusal: &QuotaRefusal) -> String {
    let mut body = t("middleware-quota-exceeded").to_string();
    for window in &refusal.windows {
        body.push('\n');
        body.push_str(&t_args(
            "middleware-quota-window-reset",
            &[
                ("id", window.id.to_string()),
                ("secs", window.reset_secs.to_string()),
            ],
        ));
    }
    body
}

/// 配额账本（`ApiState.quota` 承载，中间件与回补钩共享同一实例）：
/// 账本键（租户摘要 / `__global__`）→ 窗口 id → 状态。一 key 多窗并存，
/// 每窗独立判定（R-quota-001）。另承载 vendor 预算记账态（T006，与租户
/// 窗口同构）与落盘句柄（T005，结算 UPSERT + 周期 flush 的生产侧）。
#[derive(Debug, Default)]
pub struct QuotaState {
    ledgers: Mutex<HashMap<String, HashMap<String, WindowState>>>,
    /// vendor 预算记账态：vendor_id → window_id → 状态（usage 结算点累计，
    /// 无预扣预留语义——预算按实际用量记账）
    vendor_budgets: Mutex<HashMap<String, HashMap<String, WindowState>>>,
    /// 网关键成本预算账本（`[key_budgets]`，在飞预留-结算回冲；段缺省时
    /// reserve 早退 NoBudget——零配置零行为）
    pub key_budgets: crate::middleware::key_budget::KeyBudgetLedger,
    /// 落盘句柄（bootstrap 装配；未接线 = 纯内存态，行为与历史一致）
    ledger: Mutex<Option<Arc<crate::middleware::quota_persist::QuotaLedgerPersister>>>,
    /// 淘汰迟滞水位（见 [`QUOTA_EVICT_HYSTERESIS`]；0 = 未越过上限）
    evict_watermark: std::sync::atomic::AtomicUsize,
    /// 生效窗集单代缓存（入口与结算共用，见 [`ResolvedWindowCache`]）
    resolved: Mutex<ResolvedWindowCache>,
}

/// 手工 Debug（Repository 无 Debug）：只暴露计数面，不暴露内部结构。
impl std::fmt::Debug for crate::middleware::quota_persist::QuotaLedgerPersister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaLedgerPersister")
            .field("flush_failures", &self.flush_failures())
            .finish()
    }
}

/// 高基数 key 内存约束（与 limiteron `QuotaLimiter` 10k 同级；按账本键数计，
/// 每 key 窗口数由配置面决定、不受请求输入影响）。
const QUOTA_MAX_TRACKED_KEYS: usize = 10_000;

/// 淘汰迟滞水位：一次全表淘汰后，账本键数须再增长过该值才再次扫描——
/// 全活跃（淘汰零命中）时避免每请求 O(n) 全表扫描（均匀摊还 O(1)）。
const QUOTA_EVICT_HYSTERESIS: usize = 1_024;

/// 生效窗集缓存容量上限（按租户数计；超限租户直解析不缓存，工作有界）。
const QUOTA_RESOLVED_CACHE_MAX: usize = 4_096;

/// 生效窗集单代缓存（配置快照即代锚）：入口判定与结算路径对同一租户
/// 的两次 `resolve_windows` 收敛为一次真解析。配置热更换（`Arc<Config>`
/// 指针变更）整代作废；持代锚 `Arc<Config>` 使指针等价判定无地址复用
/// 误判（旧代存活期间其地址不会被新分配复用）。
#[derive(Debug, Default)]
struct ResolvedWindowCache {
    config: Option<Arc<Config>>,
    by_tenant: HashMap<String, Arc<Vec<RuntimeWindow>>>,
}

impl QuotaState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 生效窗集解析缓存读侧（入口判定与结算路径共用单点）：同配置快照下
    /// 同租户命中缓存零解析零分配（`Arc` 克隆）；未命中解析一回填。配置
    /// 热更换（快照指针变更）整代作废重建；容量超限直解析不缓存（见
    /// [`QUOTA_RESOLVED_CACHE_MAX`]，工作有界）。
    fn resolved_windows(&self, snapshot: &Arc<Config>, tenant: &str) -> Arc<Vec<RuntimeWindow>> {
        let mut guard = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
        if !guard
            .config
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(cached, snapshot))
        {
            *guard = ResolvedWindowCache {
                config: Some(snapshot.clone()),
                by_tenant: HashMap::new(),
            };
        }
        if let Some(hit) = guard.by_tenant.get(tenant) {
            return hit.clone();
        }
        let windows = Arc::new(resolve_windows(snapshot, tenant));
        if guard.by_tenant.len() < QUOTA_RESOLVED_CACHE_MAX {
            guard.by_tenant.insert(tenant.to_string(), windows.clone());
        }
        windows
    }

    /// 单窗在 `now` 的生效起点：calendar 由 now 对齐（确定性边界）；rolling
    /// 惰性起算——窗内首次用量即起点，未过期保持原起点（与历史
    /// `flipped(None) → 以 now 起算` 同口径）。
    fn effective_start(window: &RuntimeWindow, state_start: Option<i64>, now: i64) -> i64 {
        match window.period {
            Period::Rolling(_) => match state_start {
                Some(start) if !window.period.is_expired(start, now, window.tz_offset_secs) => {
                    start
                }
                _ => now,
            },
            _ => window
                .period
                .window_start(now, window.tz_offset_secs)
                .unwrap_or(now),
        }
    }

    /// 翻转惰性收敛：起点不一致（首触 / 已过期 / 热更换周期）即重置占用。
    /// 返回生效起点。
    fn flip_if_needed(window: &RuntimeWindow, state: &mut WindowState, now: i64) -> i64 {
        let start = Self::effective_start(window, state.window_start, now);
        if state.window_start != Some(start) {
            state.token_used = 0;
            state.cost_used = 0;
            state.window_start = Some(start);
            state.period = window.period;
            state.tz_offset_secs = window.tz_offset_secs;
        }
        start
    }

    /// 账本淘汰（consume/recompense 共用谓词 + 迟滞水位）：超上限时仅保留
    /// 存在未翻转窗的账本键（翻转判定用状态定格的 period/tz，与写入期同一
    /// 口径）。迟滞：淘汰后记水位 = 存活键数 + [`QUOTA_EVICT_HYSTERESIS`]，
    /// 未再越过水位前不重复全表扫描——全活跃（淘汰零命中）时避免每请求
    /// O(n)，摊还每请求 O(1)。
    fn evict_if_over_cap(
        &self,
        guard: &mut HashMap<String, HashMap<String, WindowState>>,
        now: i64,
    ) {
        let len = guard.len();
        if len <= QUOTA_MAX_TRACKED_KEYS {
            self.evict_watermark
                .store(0, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if len
            < self
                .evict_watermark
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        guard.retain(|_, windows| {
            windows.values().any(|s| {
                s.window_start
                    .is_none_or(|start| !s.period.is_expired(start, now, s.tz_offset_secs))
            })
        });
        self.evict_watermark.store(
            guard.len() + QUOTA_EVICT_HYSTERESIS,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// 预扣（入口）：任一窗任一维度超限即拒（不落账），拒绝详情含逐窗重置
    /// 秒数与 Retry-After；无生效窗或零预扣直接放行（不建账）。
    ///
    /// 已知语义边界：零预扣请求（body 无 model / 双限额 0）不进窗，其响应
    /// usage 也无从结算入窗（`recompense` 无账不建）——缺 model 的请求通常
    /// 被下游 4xx，该边界在 USER_GUIDE 显性披露。
    pub fn consume(
        &self,
        tenant: &str,
        windows: &[RuntimeWindow],
        pre_tokens: u64,
        pre_cost: u64,
    ) -> Result<(), QuotaRefusal> {
        if windows.is_empty() || (pre_tokens == 0 && pre_cost == 0) {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let mut guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        self.evict_if_over_cap(&mut guard, now);
        // 第一遍：翻转收敛 + 逐窗判定（拒绝路径零预留写入）。成功路径只留
        // reset 秒数（零 String 分配）；拒绝时才物化逐窗 remainder 行。
        let mut reset_secs: Vec<u64> = Vec::with_capacity(windows.len());
        let mut refused_max_reset = 0u64;
        let mut refused = false;
        let mut refused_ids = Vec::new();
        for window in windows {
            let state = guard
                .entry(window.ledger_key(tenant).to_string())
                .or_default()
                .entry(window.id.clone())
                .or_insert_with(|| WindowState::new(window));
            let start = Self::flip_if_needed(window, state, now);
            let secs = (window
                .period
                .reset_at(start, window.tz_offset_secs)
                .saturating_sub(now))
            .max(1) as u64;
            let token_over = window.tokens_limit > 0
                && pre_tokens > window.tokens_limit.saturating_sub(state.token_used);
            let cost_over = window.cost_limit_millicents > 0
                && pre_cost > window.cost_limit_millicents.saturating_sub(state.cost_used);
            if token_over || cost_over {
                refused = true;
                refused_ids.push(window.id.clone());
                refused_max_reset = refused_max_reset.max(secs);
            }
            reset_secs.push(secs);
        }
        if refused {
            let remainders = windows
                .iter()
                .zip(reset_secs)
                .map(|(window, secs)| WindowRemainder {
                    id: window.id.clone(),
                    reset_secs: secs,
                })
                .collect();
            return Err(QuotaRefusal {
                retry_after_secs: refused_max_reset,
                windows: remainders,
                refused_ids,
            });
        }
        // 第二遍：全部通过后统一落预扣（同一临界区内，判定与写入原子）
        for window in windows {
            let state = guard
                .get_mut(window.ledger_key(tenant))
                .and_then(|map| map.get_mut(&window.id));
            if let Some(state) = state {
                state.token_used = state.token_used.saturating_add(pre_tokens);
                state.cost_used = state.cost_used.saturating_add(pre_cost);
            }
        }
        Ok(())
    }

    /// 回补（响应后/流结束后）：按实际结算差额修正各窗占用（可负向返还、
    /// 超预估追扣）。**无账不建**：窗无状态时跳过（未落账无差额可回）。
    /// 翻转后差额照历史口径落入新窗（饱和钳 0）。淘汰与 [`Self::consume`]
    /// 同谓词。落盘句柄在位时逐结算窗 UPSERT 入队（设计决策 3：结算即落库）。
    pub fn recompense(
        &self,
        tenant: &str,
        windows: &[RuntimeWindow],
        pre_tokens: u64,
        pre_cost: u64,
        actual_tokens: u64,
        actual_cost: u64,
    ) {
        if windows.is_empty() {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let mut guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        self.evict_if_over_cap(&mut guard, now);
        let token_delta = actual_tokens as i128 - pre_tokens as i128;
        let cost_delta = actual_cost as i128 - pre_cost as i128;
        let mut settled: Vec<(&str, &str, i64, u64, u64)> = Vec::new();
        for window in windows {
            let Some(map) = guard.get_mut(window.ledger_key(tenant)) else {
                continue;
            };
            let Some(state) = map.get_mut(&window.id) else {
                continue;
            };
            Self::flip_if_needed(window, state, now);
            state.token_used = (state.token_used as i128 + token_delta).max(0) as u64;
            state.cost_used = (state.cost_used as i128 + cost_delta).max(0) as u64;
            if let Some(start) = state.window_start {
                settled.push((
                    window.ledger_key(tenant),
                    window.id.as_str(),
                    start,
                    state.token_used,
                    state.cost_used,
                ));
            }
        }
        drop(guard);
        self.enqueue_ledger_rows(settled, now);
    }

    /// vendor 预算记账（usage 结算点，T006）：实际用量累计进该 vendor 的
    /// 各预算窗（无预扣预留——按实际记账）；无预算 vendor 零开销早退。
    /// 落盘句柄在位时同路入队（window_id=`vendor:{id}`）。
    pub fn record_vendor_usage(&self, config: &Config, vendor_id: &str, tokens: u64, cost: u64) {
        if config.vendor_budgets.is_empty() {
            return;
        }
        let windows = resolve_vendor_budget_windows(config, vendor_id);
        if windows.is_empty() {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let mut guard = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut recorded: Vec<(&str, &str, i64, u64, u64)> = Vec::new();
        for window in &windows {
            let state = guard
                .entry(vendor_id.to_string())
                .or_default()
                .entry(vendor_window_id(vendor_id))
                .or_insert_with(|| WindowState::new(window));
            Self::flip_if_needed(window, state, now);
            state.token_used = state.token_used.saturating_add(tokens);
            state.cost_used = state.cost_used.saturating_add(cost);
            if let Some(start) = state.window_start {
                recorded.push((
                    vendor_id,
                    vendor_id,
                    start,
                    state.token_used,
                    state.cost_used,
                ));
            }
        }
        drop(guard);
        // 账本行键特殊：key=__vendor__:{id}、window_id=vendor:{id}
        let persister = self
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(persister) = persister {
            let rows = recorded
                .into_iter()
                .map(
                    |(_, _, start, token_used, cost_used)| crate::database::QuotaLedgerRow {
                        api_key_hash: vendor_ledger_key(vendor_id),
                        window_id: vendor_window_id(vendor_id),
                        window_start: start,
                        tokens_used: token_used as i64,
                        cost_used_millicents: cost_used as i64,
                        updated_at: now,
                    },
                )
                .collect();
            persister.write(rows);
        }
    }

    /// 启动恢复（design 决策 3）：仅恢复仍处活动期的窗（window_start + 周期
    /// ≥ now），过期行跳过；周期语义按当前配置解释（账本不存周期事实）。
    /// 返回恢复窗数。租户 / `__global__` / `__vendor__:{id}` 三类作用域行同路。
    /// 租户账本灌回套 [`QUOTA_MAX_TRACKED_KEYS`] 上限：超限键跳过并显性 warn
    ///（读侧 SQL 下界已滤过期行，此处防高基数残留撑爆内存——被跳过键的窗
    /// 后续请求自然重建，fail-open 方向与 flush 误差一致）。
    pub fn restore_from_ledger(
        &self,
        rows: &[crate::database::QuotaLedgerRow],
        config: &Config,
    ) -> usize {
        let now = chrono::Utc::now().timestamp();
        let windows = effective_quota_windows(&config.quota);
        // 索引化：restore 行数多时逐行线性扫 windows/vendor_budgets 是 O(rows×N)，
        // 一次性建 HashMap 后恢复路径 O(rows+N)
        let window_index: std::collections::HashMap<&str, &_> =
            windows.iter().map(|w| (w.id.as_str(), w)).collect();
        let budget_index: std::collections::HashMap<&str, &_> = config
            .vendor_budgets
            .iter()
            .map(|b| (b.vendor_id.trim(), b))
            .collect();
        let mut restored = 0usize;
        let mut restored_keys = 0usize;
        let mut skipped_keys = 0usize;
        {
            let mut guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
            for row in rows {
                if row.tokens_used < 0 || row.cost_used_millicents < 0 {
                    continue;
                }
                if row.api_key_hash.starts_with("__vendor__:") {
                    continue;
                }
                let Some(window) = window_index.get(row.window_id.as_str()) else {
                    continue;
                };
                let Some(period) =
                    Period::from_config(&window.period, &window.calendar, window.rolling_secs)
                else {
                    continue;
                };
                if period.is_expired(row.window_start, now, window.tz_offset_secs) {
                    continue;
                }
                let known_key = guard.contains_key(&row.api_key_hash);
                if !known_key && restored_keys >= QUOTA_MAX_TRACKED_KEYS {
                    skipped_keys += 1;
                    continue;
                }
                let state = guard
                    .entry(row.api_key_hash.clone())
                    .or_default()
                    .entry(row.window_id.clone())
                    .or_insert_with(|| WindowState {
                        period,
                        tz_offset_secs: window.tz_offset_secs,
                        token_used: 0,
                        cost_used: 0,
                        window_start: None,
                    });
                state.token_used = row.tokens_used as u64;
                state.cost_used = row.cost_used_millicents as u64;
                state.window_start = Some(row.window_start);
                restored += 1;
                if !known_key {
                    restored_keys += 1;
                }
            }
        }
        if skipped_keys > 0 {
            tracing::warn!(
                skipped = skipped_keys,
                cap = QUOTA_MAX_TRACKED_KEYS,
                "{}",
                t_args(
                    "main-quota-ledger-restore-capped",
                    &[
                        ("skipped", skipped_keys.to_string()),
                        ("cap", QUOTA_MAX_TRACKED_KEYS.to_string()),
                    ]
                )
            );
        }
        let mut vendors = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for row in rows {
            let Some(vendor_id) = row.api_key_hash.strip_prefix("__vendor__:") else {
                continue;
            };
            let Some(budget) = budget_index.get(vendor_id.trim()) else {
                continue;
            };
            let Some(period) =
                Period::from_config(&budget.period, &budget.calendar, budget.rolling_secs)
            else {
                continue;
            };
            if period.is_expired(row.window_start, now, budget.tz_offset_secs) {
                continue;
            }
            let state = vendors
                .entry(vendor_id.to_string())
                .or_default()
                .entry(vendor_window_id(vendor_id))
                .or_insert_with(|| WindowState {
                    period,
                    tz_offset_secs: budget.tz_offset_secs,
                    token_used: 0,
                    cost_used: 0,
                    window_start: None,
                });
            state.token_used = row.tokens_used.max(0) as u64;
            state.cost_used = row.cost_used_millicents.max(0) as u64;
            state.window_start = Some(row.window_start);
            restored += 1;
        }
        restored
    }

    /// 账本全量快照（30s 周期 flush 的生产侧；design 决策 3）：租户/global
    /// 窗 + vendor 预算窗的当前占用。`token_used`/`cost_used` 含入口预扣
    /// （预扣即预留）——快照携带预扣是保守方向：恢复后多扣不漏扣，预算
    /// 约束不放松。
    pub fn ledger_snapshot(&self) -> Vec<crate::database::QuotaLedgerRow> {
        let now = chrono::Utc::now().timestamp();
        let mut rows = Vec::new();
        {
            let guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
            for (key, windows) in guard.iter() {
                for (id, state) in windows {
                    if let Some(start) = state.window_start {
                        rows.push(crate::database::QuotaLedgerRow {
                            api_key_hash: key.clone(),
                            window_id: id.clone(),
                            window_start: start,
                            tokens_used: state.token_used as i64,
                            cost_used_millicents: state.cost_used as i64,
                            updated_at: now,
                        });
                    }
                }
            }
        }
        let vendors = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for (vendor_id, windows) in vendors.iter() {
            for state in windows.values() {
                if let Some(start) = state.window_start {
                    rows.push(crate::database::QuotaLedgerRow {
                        api_key_hash: vendor_ledger_key(vendor_id),
                        window_id: vendor_window_id(vendor_id),
                        window_start: start,
                        tokens_used: state.token_used as i64,
                        cost_used_millicents: state.cost_used as i64,
                        updated_at: now,
                    });
                }
            }
        }
        rows
    }

    /// admin 账本读侧（`GET /admin/quota/ledger` 单点）：内存态逐窗快照，
    /// 含作用域分类与重置时刻。未起算窗不列（无事实可读）。
    pub fn ledger_entries(&self) -> Vec<QuotaLedgerEntry> {
        let mut entries = Vec::new();
        {
            let guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
            for (key, windows) in guard.iter() {
                for (id, state) in windows {
                    let Some(start) = state.window_start else {
                        continue;
                    };
                    let scope = if key == GLOBAL_LEDGER_KEY {
                        "global"
                    } else {
                        "tenant"
                    };
                    entries.push(QuotaLedgerEntry {
                        api_key_hash: key.clone(),
                        window_id: id.clone(),
                        scope: scope.to_string(),
                        window_start: start,
                        tokens_used: state.token_used,
                        cost_used_millicents: state.cost_used,
                        reset_at: state.period.reset_at(start, state.tz_offset_secs),
                    });
                }
            }
        }
        let vendors = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for (vendor_id, windows) in vendors.iter() {
            for state in windows.values() {
                let Some(start) = state.window_start else {
                    continue;
                };
                entries.push(QuotaLedgerEntry {
                    api_key_hash: vendor_ledger_key(vendor_id),
                    window_id: vendor_window_id(vendor_id),
                    scope: "vendor".to_string(),
                    window_start: start,
                    tokens_used: state.token_used,
                    cost_used_millicents: state.cost_used,
                    reset_at: state.period.reset_at(start, state.tz_offset_secs),
                });
            }
        }
        entries
    }

    /// 接线落盘句柄（bootstrap 装配点；幂等——后写覆盖）。
    pub fn set_ledger_persister(
        &self,
        persister: Arc<crate::middleware::quota_persist::QuotaLedgerPersister>,
    ) {
        *self.ledger.lock().unwrap_or_else(|p| p.into_inner()) = Some(persister);
    }

    /// 结算窗行入队落盘（recompense 专用：租户/global 键语义）。
    fn enqueue_ledger_rows(&self, settled: Vec<(&str, &str, i64, u64, u64)>, now: i64) {
        let persister = self
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(persister) = persister {
            let rows = settled
                .into_iter()
                .map(|(key, window_id, start, token_used, cost_used)| {
                    crate::database::QuotaLedgerRow {
                        api_key_hash: key.to_string(),
                        window_id: window_id.to_string(),
                        window_start: start,
                        tokens_used: token_used as i64,
                        cost_used_millicents: cost_used as i64,
                        updated_at: now,
                    }
                })
                .collect();
            persister.write(rows);
        }
    }

    /// 窗口用量快照（单测断言用）：指定账本键下单窗的 (token, cost) 占用。
    #[cfg(test)]
    pub fn snapshot(&self, key: &str, window_id: &str) -> (u64, u64) {
        let guard = self.ledgers.lock().unwrap_or_else(|p| p.into_inner());
        match guard.get(key).and_then(|map| map.get(window_id)) {
            Some(w) => (w.token_used, w.cost_used),
            None => (0, 0),
        }
    }

    /// vendor 预算窗占用快照（单测断言用）。
    #[cfg(test)]
    pub fn vendor_snapshot(&self, vendor_id: &str, window_id: &str) -> (u64, u64) {
        let guard = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match guard.get(vendor_id).and_then(|map| map.get(window_id)) {
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

/// vendor 预算运行时窗解析（热配置逐结算单点）：`[[vendor_budgets]]` 中
/// 该 vendor 的条目 → RuntimeWindow（id=`vendor:{id}`；无预算 → 空集）。
fn resolve_vendor_budget_windows(config: &Config, vendor_id: &str) -> Vec<RuntimeWindow> {
    config
        .vendor_budgets
        .iter()
        // 与 config 校验同口径（eq_ignore_ascii_case）：校验层允许 vendor id 大小写
        // 不同但视为同一 vendor，运行时要匹配得上，否则预算配置静默失效
        .filter(|b| b.vendor_id.trim().eq_ignore_ascii_case(vendor_id))
        .filter_map(|b| {
            Period::from_config(&b.period, &b.calendar, b.rolling_secs).map(|period| {
                RuntimeWindow {
                    id: vendor_window_id(vendor_id),
                    global: false,
                    period,
                    tz_offset_secs: b.tz_offset_secs,
                    tokens_limit: b.tokens_limit,
                    cost_limit_millicents: b.cost_limit_millicents,
                }
            })
        })
        .collect()
}

/// admin 账本读侧条目（`GET /admin/quota/ledger` 的 rows 形态）：内存态
/// 快照 + 周期语义（reset_at 由状态定格的 period 计算，账本表不存周期）。
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaLedgerEntry {
    /// 租户摘要 / `__global__` / `__vendor__:{id}`
    pub api_key_hash: String,
    pub window_id: String,
    /// 作用域：`tenant` / `global` / `vendor`
    pub scope: String,
    pub window_start: i64,
    pub tokens_used: u64,
    pub cost_used_millicents: u64,
    /// 重置时刻（Unix 秒）
    pub reset_at: i64,
}

/// 预算读模型与读侧端口的定义在同名根级契约模块 [`crate::quota_contract`]
/// （无 feature 门控，与 [`crate::storage`] 缝同构）。此处**只为本文件实现使用**
/// 而引入类型，不做 re-export——对外公开面只有 `VendorBudgetSource` 的 trait
/// 方法，避免出现「同一类型两条公开路径」。
use crate::quota_contract::{VendorBudgetSource, VendorBudgetView};

impl VendorBudgetSource for QuotaState {
    fn vendor_budget_view(&self, config: &Config, vendor_id: &str) -> Option<VendorBudgetView> {
        self.vendor_budget_view_impl(config, vendor_id)
    }
}

impl QuotaState {
    /// 端口 [`VendorBudgetSource::vendor_budget_view`] 的唯一落点（私有）：
    /// 端口契约下 router 与 admin 均经 trait 方法调用，本函数不对外暴露。
    /// 未配置预算返回 None（vendor 不参与该因子）。
    fn vendor_budget_view_impl(
        &self,
        config: &Config,
        vendor_id: &str,
    ) -> Option<VendorBudgetView> {
        let windows = resolve_vendor_budget_windows(config, vendor_id);
        let window = windows.first()?;
        let now = chrono::Utc::now().timestamp();
        let guard = self
            .vendor_budgets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let state = guard.get(vendor_id).and_then(|map| map.get(&window.id));
        // 读侧惰性翻转（与写侧 `flip_if_needed` 同源 `effective_start`）：账本态
        // 停在上一窗时不得把上一窗占用暴露给读侧。否则窗过期且此后该 vendor
        // 零流量时，`depleted()` 恒 true → smart 永久摘除候选，且因无结算再无
        // 翻转机会（只能重启或用直连路由解开）。
        let start = Self::effective_start(window, state.and_then(|s| s.window_start), now);
        let live = state.filter(|s| s.window_start == Some(start));
        let (tokens_used, cost_used, start) = match live {
            Some(st) => (st.token_used, st.cost_used, Some(start)),
            None => (0, 0, None),
        };
        let (reset_secs, period_secs, elapsed_secs, not_started) = match start {
            Some(start) => (
                window
                    .period
                    .reset_at(start, window.tz_offset_secs)
                    .saturating_sub(now)
                    .max(0) as u64,
                window
                    .period
                    .period_secs(start, window.tz_offset_secs)
                    .max(1) as u64,
                (now - start).max(0) as u64,
                false,
            ),
            None => match window.period {
                // calendar 窗起点与用量无关——对齐边界即窗起点
                Period::Daily | Period::Weekly | Period::Monthly => {
                    let start = window
                        .period
                        .window_start(now, window.tz_offset_secs)
                        .unwrap_or(now);
                    (
                        window
                            .period
                            .reset_at(start, window.tz_offset_secs)
                            .saturating_sub(now)
                            .max(0) as u64,
                        window
                            .period
                            .period_secs(start, window.tz_offset_secs)
                            .max(1) as u64,
                        (now - start).max(0) as u64,
                        false,
                    )
                }
                Period::Rolling(secs) => (secs.max(1), secs.max(1), 0, true),
            },
        };
        Some(VendorBudgetView {
            tokens_used,
            tokens_limit: window.tokens_limit,
            cost_used,
            cost_limit: window.cost_limit_millicents,
            reset_secs,
            period_secs,
            elapsed_secs,
            not_started,
        })
    }
}

/// 生效窗口解析（热配置逐请求单点）：`[[quota.windows]]` 直读编译（热路径
/// 零中间拷贝，仅旧单窗字段兜底路径物化临时配置）→ 每窗限额回填 → 双零窗
/// 剔除。回填只对 `scope=api_key`：0 = 落回 [[auth.keys]] 的 per-key 限额；
/// `scope=global` 窗不回填——全局窗是舰队级约束，限额必须配置面显式声明
/// （validate 拒双零），按请求租户回填会让同一窗的判定随请求者漂移、无限
/// 额租户整体绕过。全部窗被剔除返回空 vec（调用方三级早退）。
pub fn resolve_windows(snapshot: &Config, tenant: &str) -> Vec<RuntimeWindow> {
    let (tenant_tokens, tenant_cost) = quotas_for(snapshot, tenant);
    let legacy;
    let windows: &[crate::config::QuotaWindowConfig] = if snapshot.quota.windows.is_empty() {
        legacy = effective_quota_windows(&snapshot.quota);
        &legacy
    } else {
        &snapshot.quota.windows
    };
    let mut resolved = Vec::new();
    for window in windows {
        let global = window.scope.trim() == "global";
        let (tokens_limit, cost_limit_millicents) = if global {
            (window.tokens_limit, window.cost_limit_millicents)
        } else {
            (
                if window.tokens_limit > 0 {
                    window.tokens_limit
                } else {
                    tenant_tokens
                },
                if window.cost_limit_millicents > 0 {
                    window.cost_limit_millicents
                } else {
                    tenant_cost
                },
            )
        };
        if tokens_limit == 0 && cost_limit_millicents == 0 {
            continue;
        }
        let Some(period) =
            Period::from_config(&window.period, &window.calendar, window.rolling_secs)
        else {
            continue;
        };
        resolved.push(RuntimeWindow {
            id: window.id.clone(),
            global,
            period,
            tz_offset_secs: window.tz_offset_secs,
            tokens_limit,
            cost_limit_millicents,
        });
    }
    resolved
}

/// per-key 限额解析（配置快照现读）：digest 匹配 `[[auth.keys]]` 条目 →
/// (token_quota, cost_quota)；未声明 → (0,0) 不限。
fn quotas_for(config: &Config, tenant: &str) -> (u64, u64) {
    match config.auth.as_ref() {
        Some(auth) => match auth.keys.iter().find(|k| k.hash == tenant) {
            Some(entry) => (entry.token_quota, entry.cost_quota_millicents),
            None => (0, 0),
        },
        None => (0, 0),
    }
}

/// 账本保留下界推导（保留清理与恢复读侧共用单点）：当前配置下可能仍处
/// 活动期的最长窗长——rolling = `rolling_secs`、daily/weekly 固定、monthly
/// 取上界 [`MAX_MONTH_SECS`]（月长依起点月份，宁取上界不误删活动行）。
/// `[[quota.windows]]` 为空且启用时计入旧单窗字段；完全无窗（配额关/未配）
/// 兜底月窗上界，防空配置短保留误删切换窗口期的活动行。
pub fn ledger_retention_secs(config: &Config) -> i64 {
    fn period_secs_upper(period: Period) -> i64 {
        match period {
            Period::Rolling(secs) => secs.max(1) as i64,
            Period::Daily => DAY_SECS,
            Period::Weekly => 7 * DAY_SECS,
            Period::Monthly => MAX_MONTH_SECS,
        }
    }
    let mut max_secs = 0i64;
    if config.quota.windows.is_empty() && config.quota.enabled {
        max_secs = max_secs.max((config.quota.window_secs.max(1)) as i64);
    }
    for window in &config.quota.windows {
        if let Some(period) =
            Period::from_config(&window.period, &window.calendar, window.rolling_secs)
        {
            max_secs = max_secs.max(period_secs_upper(period));
        }
    }
    for budget in &config.vendor_budgets {
        if let Some(period) =
            Period::from_config(&budget.period, &budget.calendar, budget.rolling_secs)
        {
            max_secs = max_secs.max(period_secs_upper(period));
        }
    }
    if max_secs == 0 {
        MAX_MONTH_SECS
    } else {
        max_secs
    }
}

/// 配额中间件注入面（最小接口）：只携带判限所需数据，经 `from_fn_with_state` 注入。
#[derive(Clone)]
pub struct QuotaGate {
    pub config: Arc<ConfigManager>,
    pub quota: Option<Arc<QuotaState>>,
    /// 指标句柄（429 多窗拒绝逐窗打分；None = 无观测，判限不受影响）
    pub metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
}

/// 配额适用性判定产物：账本句柄 + 配置快照（`Arc<Config>` 共享，零深拷贝）
/// + 生效窗口集（限额已回填；缓存产物，`Arc` 共享）。
struct QuotaLease {
    quota: Arc<QuotaState>,
    config: Arc<Config>,
    windows: Arc<Vec<RuntimeWindow>>,
}

/// 配额适用性三级判定（入口/透传流/转换流共用单点）：未接线 /
/// `[quota].enabled = false` / 生效窗集为空（全部窗不判或该租户无限额）
/// → `None`（调用方零行为直通）。
fn quota_applies(
    config_manager: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
) -> Option<QuotaLease> {
    let quota = quota?;
    let snapshot = config_manager.read();
    quota_applies_config(&snapshot, quota, tenant)
}

/// 同 [`quota_applies`]，快照已持有（结算路径复用同代快照，免二次读锁）。
fn quota_applies_config(
    snapshot: &Arc<Config>,
    quota: &Arc<QuotaState>,
    tenant: &str,
) -> Option<QuotaLease> {
    if !snapshot.quota.enabled {
        return None;
    }
    let windows = quota.resolved_windows(snapshot, tenant);
    if windows.is_empty() {
        return None;
    }
    Some(QuotaLease {
        quota: quota.clone(),
        config: snapshot.clone(),
        windows,
    })
}

/// key 预算估成本（R-budget-001 入口单点）：输入腿 = 请求体字节 / cpt ×
/// 输入单价（档源与租户配额同一 [`resolve_price_source`]，不允许第二套
/// 价目表）；输出腿 = 完成 token 滑动均值 × 输出单价，冷启动（无均值）
/// 取 `cold_start_output_millicents`（调用方传规则窗限额一半——保守方向：
/// 首请求预留不超半窗输出）。
pub fn key_budget_estimate(
    catalog: &[ModelCatalogEntry],
    pricing: &[PricingConfig],
    model: &str,
    est_input_tokens: u64,
    completion_mean: Option<u64>,
    cold_start_output_millicents: u64,
) -> u64 {
    let source = resolve_price_source(catalog, pricing, model);
    let input = match source {
        PriceSource::Catalog { input, .. } => {
            usd_to_millicents(est_input_tokens as f64 / 1_000_000.0 * input)
        }
        PriceSource::Pricing { input, .. } => {
            usd_to_millicents(est_input_tokens as f64 / 1000.0 * input)
        }
    };
    let output = match completion_mean {
        Some(mean) => match source {
            PriceSource::Catalog { output, .. } => {
                usd_to_millicents(mean as f64 / 1_000_000.0 * output)
            }
            PriceSource::Pricing { output, .. } => usd_to_millicents(mean as f64 / 1000.0 * output),
        },
        None => cold_start_output_millicents,
    };
    input.saturating_add(output)
}

/// key 预算适用性产物（入口判定）：fail_open 开关（同 key 多条规则时取
/// 首条——同 key 多窗属罕见形态，首条即声明语义）与冷启动输出预留基准
///（规则窗限额最小值的 1/2，多窗取最紧——保守方向）。
struct KeyBudgetLease {
    fail_open: bool,
    cold_start_output_millicents: u64,
}

/// key 预算适用性判定（入口单点）：该 key 有预算规则即适用（与租户配额
/// 独立叠加，不受 `[quota].enabled` 门控）；无规则 → None（零行为直通）。
fn key_budget_applies(snapshot: &Config, key_hash: &str) -> Option<KeyBudgetLease> {
    let mut rules = snapshot
        .key_budgets
        .rules
        .iter()
        .filter(|rule| rule.api_key_hash.trim() == key_hash);
    let first = rules.next()?;
    let min_limit = rules
        .map(|rule| rule.cost_limit_millicents)
        .chain(std::iter::once(first.cost_limit_millicents))
        .min()?;
    Some(KeyBudgetLease {
        fail_open: first.fail_open,
        cold_start_output_millicents: min_limit / 2,
    })
}

/// key 预算 429 拒绝响应（超限/源不可用共形）：Retry-After（窗余量换算）
/// + i18n 文案，形状与租户配额 429 一致。
fn key_budget_refusal(retry_after_secs: u64, window_name: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        [
            ("retry-after", retry_after_secs.max(1).to_string()),
            ("content-type", "text/plain; charset=utf-8".to_string()),
        ],
        format!(
            "{}\n{}",
            t("middleware-key-budget-exceeded"),
            t_args(
                "middleware-key-budget-window-reset",
                &[
                    ("window", window_name.to_string()),
                    ("secs", retry_after_secs.max(1).to_string()),
                ],
            )
        ),
    )
        .into_response()
}

/// 配额中间件（入口裁决）：`/v1/chat/completions`、`/v1/messages`、
/// `/v1/rerank` 与 `/v1/responses`；关闭/无账本/双限额不限时零行为透传
/// （限额判定先于体读取）。超限 429 + Retry-After + i18n 文案。
/// 租户配额窗与网关键预算（`[key_budgets]`）两道独立叠加，任一触发即拒。
pub async fn quota_middleware(
    State(gate): State<QuotaGate>,
    request: Request,
    next: Next,
) -> Response {
    // 零分配路径判定（matches! 不产生 String）；path 在 into_parts 前结束借用
    let (gemini, applicable) = {
        let path = request.uri().path();
        let gemini = request.method() == axum::http::Method::POST
            && path.starts_with("/v1beta/models/")
            && !path.ends_with(":countTokens");
        let applicable = gemini
            || matches!(
                path,
                "/v1/chat/completions" | "/v1/messages" | "/v1/rerank" | "/v1/responses"
            );
        (gemini, applicable)
    };
    let (parts, body) = request.into_parts();
    // 非适用路径：body 未触碰，原样透传（零拷贝）。
    if !applicable {
        return next.run(Request::from_parts(parts, body)).await;
    }
    // 租户摘要：认证中间件在外层已放行，现场计算。
    let tenant = api_key_digest(&parts.headers);
    // 两道适用性判定（独立叠加）：租户配额窗 / 网关键预算。二者皆无 →
    // 零行为直通（段缺省整体关闭，零配置逐字节不变）。
    let quota_lease = quota_applies(&gate.config, gate.quota.as_ref(), &tenant);
    let budget_lease = gate.quota.as_ref().and_then(|quota| {
        let snapshot = gate.config.read();
        key_budget_applies(&snapshot, &tenant).map(|lease| (quota.clone(), lease, snapshot))
    });
    if quota_lease.is_none() && budget_lease.is_none() {
        return next.run(Request::from_parts(parts, body)).await;
    }
    // 受限租户/预算 key 才缓冲与解析（handler 经 Json 提取器自行解析，
    // 不跨层传递已解析体）。
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
    // Gemini body 无 model（在路径上）：为预扣注入本地副本，不改变转发体
    let mut parsed = parsed;
    if gemini {
        if let Some(model) = gemini_model_from_path(parts.uri.path()) {
            parsed["model"] = Value::String(model);
        }
    }
    // 第一道：租户配额窗预扣（快照为 Arc<Config>：持至请求结束零深拷贝）
    if let Some(QuotaLease {
        quota,
        config: snapshot,
        windows,
    }) = &quota_lease
    {
        let cpt = snapshot.quota.est_chars_per_token.max(1);
        let (pre_tokens, pre_cost) =
            precharge_for(&parsed, &snapshot.model_catalog, &snapshot.pricing, cpt);
        if let Err(refusal) = quota.consume(&tenant, windows, pre_tokens, pre_cost) {
            if let Some(metrics) = &gate.metrics {
                for id in &refusal.refused_ids {
                    metrics.record_quota_window_refused(id);
                }
            }
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [
                    ("retry-after", refusal.retry_after_secs.max(1).to_string()),
                    ("content-type", "text/plain; charset=utf-8".to_string()),
                ],
                refusal_body(&refusal),
            )
                .into_response();
        }
    }
    // 第二道：网关键预算预扣（auth 识别 key 后、路由前；超限 429 +
    // Retry-After；预算源不可用按 fail_open 语义处置）
    if let Some((quota_state, budget_lease, snapshot)) = &budget_lease {
        let cpt = snapshot.quota.est_chars_per_token.max(1);
        let model = parsed.get("model").and_then(Value::as_str).unwrap_or("");
        let est = key_budget_estimate(
            &snapshot.model_catalog,
            &snapshot.pricing,
            model,
            body_bytes.len() as u64 / cpt,
            quota_state.key_budgets.completion_mean(&tenant),
            budget_lease.cold_start_output_millicents,
        );
        match quota_state.key_budgets.reserve(snapshot, &tenant, est) {
            crate::quota_contract::ReserveOutcome::Admitted(_)
            | crate::quota_contract::ReserveOutcome::NoBudget => {}
            crate::quota_contract::ReserveOutcome::Exceeded { retry_after_secs } => {
                let window = quota_state
                    .key_budgets
                    .view(snapshot, &tenant)
                    .map(|view| view.window)
                    .unwrap_or_default();
                return key_budget_refusal(retry_after_secs, &window);
            }
            crate::quota_contract::ReserveOutcome::Unavailable => {
                if budget_lease.fail_open {
                    tracing::warn!(
                        key_hash = %tenant,
                        "{}",
                        t("middleware-key-budget-source-unavailable")
                    );
                } else {
                    // fail-closed：拒绝并带窗重置余量（best-effort）
                    let view = quota_state.key_budgets.view(snapshot, &tenant);
                    return key_budget_refusal(
                        view.as_ref().map(|view| view.reset_secs).unwrap_or(60),
                        &view.map(|view| view.window).unwrap_or_default(),
                    );
                }
            }
        }
    }
    // 预扣成功：body 原样重装交 handler。
    next.run(Request::from_parts(
        parts,
        axum::body::Body::from(body_bytes),
    ))
    .await
}

/// 非流式回补钩（响应后）：有 usage 才结算（无 usage 保持预扣）。
/// `quota` 未接线/未启用/无限额时 no-op。
#[allow(clippy::too_many_arguments)] // 结算参数面：与本变更内 route_ctx 化重构解耦
pub fn settle_nonstream(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
    response_body: &Value,
    vendor_type: &str,
    vendor_id: &str,
    metrics: Option<&std::sync::Arc<crate::utils::metrics::GlobalMetrics>>,
) {
    // O2 命中率观测：厂商维度缓存 token 记账（先于 quota 门控——无限额
    // 租户同样计入，避免 cache_hit_ratio 系统性失真）
    if let Some(metrics) = metrics {
        if let Some((prompt, _, cache_read, _)) =
            crate::protocol::parse_usage_detailed(response_body)
        {
            metrics.record_vendor_cache(vendor_id, cache_read as u64, prompt as u64);
        }
    }
    let Some(state) = quota else {
        return;
    };
    // 结算适用性（快路径保持零解析）：租户配额窗非空 / vendor 预算在位 /
    // key 预算在位才需要解析 usage 与计价；三者皆无 → 零账面直通。窗集走
    // 缓存（入口已解析同租户同快照 → 本次零解析）。
    let snapshot = config.read();
    let has_vendor_budgets = !snapshot.vendor_budgets.is_empty();
    let has_key_budgets = !snapshot.key_budgets.rules.is_empty();
    let tenant_windows = if snapshot.quota.enabled {
        state.resolved_windows(&snapshot, tenant)
    } else {
        Arc::new(Vec::new())
    };
    if tenant_windows.is_empty() && !has_vendor_budgets && !has_key_budgets {
        return;
    }
    let Some((prompt, completion, cache_read, cache_creation)) =
        crate::protocol::parse_usage_detailed(response_body)
    else {
        return;
    };
    let cpt = snapshot.quota.est_chars_per_token.max(1);
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
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
    // vendor 预算记账（与租户回补同点；vendor 维度约束独立于租户限额——
    // 无预算 vendor 在 record_vendor_usage 内零开销早退）
    if has_vendor_budgets {
        state.record_vendor_usage(&snapshot, vendor_id, actual_tokens, actual_cost);
    }
    // 网关键预算结算回冲（R-budget-001：FIFO 配对在飞预留，实际用量入窗；
    // 无预留不丢账）
    if has_key_budgets {
        state
            .key_budgets
            .settle_fifo(&snapshot, tenant, actual_cost, completion as u64);
    }
    if !tenant_windows.is_empty() {
        let (pre_tokens, pre_cost) =
            precharge_for(body, &snapshot.model_catalog, &snapshot.pricing, cpt);
        state.recompense(
            tenant,
            &tenant_windows,
            pre_tokens,
            pre_cost,
            actual_tokens,
            actual_cost,
        );
    }
}

/// 缓存 HIT 结算：命中无上游往返、实际用量为 0，以 0 结算回补预扣；
/// 门控与入口一致（未接线/未启用/无限额时 no-op）。key 预算在位时同样
/// 以 0 结算回冲预留。
pub fn settle_nonstream_cached(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
) {
    let Some(state) = quota else {
        return;
    };
    let snapshot = config.read();
    if !snapshot.key_budgets.rules.is_empty() {
        state.key_budgets.settle_fifo(&snapshot, tenant, 0, 0);
    }
    let Some(lease) = quota_applies_config(&snapshot, state, tenant) else {
        return;
    };
    let snapshot = lease.config;
    let cpt = snapshot.quota.est_chars_per_token.max(1);
    let (pre_tokens, pre_cost) =
        precharge_for(body, &snapshot.model_catalog, &snapshot.pricing, cpt);
    lease
        .quota
        .recompense(tenant, &lease.windows, pre_tokens, pre_cost, 0, 0);
}

/// 结算上下文（转换流）：账本句柄、租户与预扣口径一次构造，上下文不持有请求体
///（整段对话不在流存活期常驻内存）。`for_request` 内置早退——不适用返回
/// `None`，调用方零包装直通。适用性 = 租户配额窗非空 / vendor 预算在位 /
/// key 预算在位（后两者让无限额租户的流同样构建 ctx，预算记账不漏流式用量）。
pub struct SettlementCtx {
    quota: Arc<QuotaState>,
    tenant: String,
    model: String,
    vendor_type: String,
    vendor_id: String,
    metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
    /// 配置快照共享（`Arc<Config>` 引用，零深拷贝；热更换新快照不影响
    /// 已建立流的结算口径）。
    config: Arc<Config>,
    /// 生效窗口集（构造期解析产物，缓存共享 `Arc`；空 = 租户配额不适用，
    /// 仅 vendor 预算记账）
    windows: Arc<Vec<RuntimeWindow>>,
    /// vendor 预算在位（流结束记账开关）
    record_vendor: bool,
    /// key 预算在位（流结束结算回冲开关）
    settle_key_budget: bool,
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
        vendor_id: &str,
        metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
    ) -> Option<Self> {
        let quota = quota?;
        let snapshot = config.read();
        let has_vendor_budgets = !snapshot.vendor_budgets.is_empty();
        let has_key_budgets = !snapshot.key_budgets.rules.is_empty();
        let windows = if snapshot.quota.enabled {
            quota.resolved_windows(&snapshot, tenant)
        } else {
            Arc::new(Vec::new())
        };
        if windows.is_empty() && !has_vendor_budgets && !has_key_budgets {
            return None;
        }
        let (pre_tokens, pre_cost) = precharge_for(
            body,
            &snapshot.model_catalog,
            &snapshot.pricing,
            snapshot.quota.est_chars_per_token.max(1),
        );
        Some(Self {
            quota: quota.clone(),
            vendor_id: vendor_id.to_string(),
            metrics,
            tenant: tenant.to_string(),
            model: body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            vendor_type: vendor_type.to_string(),
            record_vendor: has_vendor_budgets,
            settle_key_budget: has_key_budgets,
            config: snapshot,
            windows,
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
        let ctx_vendor = ctx.as_ref().map(|c| c.vendor_id.clone()).unwrap_or_default();
        let ctx_metrics = ctx.as_ref().and_then(|c| c.metrics.clone());
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
                // O2：流式记账按末次 usage 收敛（多 chunk 不重复计数）
                if let Some(metrics) = &ctx_metrics {
                    metrics.record_vendor_cache(
                        &ctx_vendor,
                        *cache_read_tokens as u64,
                        *prompt_tokens as u64,
                    );
                }
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
            let actual_tokens = (prompt as u64).saturating_add(completion as u64);
            if ctx.record_vendor {
                ctx.quota.record_vendor_usage(
                    &ctx.config,
                    &ctx.vendor_id,
                    actual_tokens,
                    actual_cost,
                );
            }
            if ctx.settle_key_budget {
                ctx.quota.key_budgets.settle_fifo(
                    &ctx.config,
                    &ctx.tenant,
                    actual_cost,
                    completion as u64,
                );
            }
            if !ctx.windows.is_empty() {
                ctx.quota.recompense(
                    &ctx.tenant,
                    &ctx.windows,
                    ctx.pre_tokens,
                    ctx.pre_cost,
                    actual_tokens,
                    actual_cost,
                );
            }
        }
    }
}

/// 透传流帧内 usage 提取：Anthropic `message_start` 把 usage 嵌在 `message` 下，
/// 其余在顶层；提数一律交给 `parse_usage_detailed`（包成 `{"usage": …}` 复用同一分支）。
pub(crate) fn frame_usage(data: &str) -> Option<(u64, u64, u64, u64)> {
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
    // Gemini 原生直通：`usageMetadata` 顶层累积值，直接复用三形态提数
    if value.get("usageMetadata").is_some() {
        let (prompt, completion, cache_read, cache_creation) =
            crate::protocol::parse_usage_detailed(&value)?;
        return Some((
            prompt as u64,
            completion as u64,
            cache_read as u64,
            cache_creation as u64,
        ));
    }
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
pub(crate) fn merge_usage(
    prev: (u64, u64, u64, u64),
    next: (u64, u64, u64, u64),
) -> (u64, u64, u64, u64) {
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
    vendor_id: String,
    config: Arc<Config>,
    windows: Arc<Vec<RuntimeWindow>>,
    record_vendor: bool,
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
        let actual_tokens = prompt.saturating_add(completion);
        if self.record_vendor {
            self.quota.record_vendor_usage(
                &self.config,
                &self.vendor_id,
                actual_tokens,
                actual_cost,
            );
        }
        if !self.windows.is_empty() {
            self.quota.recompense(
                &self.tenant,
                &self.windows,
                self.pre_tokens,
                self.pre_cost,
                actual_tokens,
                actual_cost,
            );
        }
    }
}

/// 透传流响应体装配：仅当结算适用（已接线，且「租户配额窗非空」或「vendor
/// 预算在位」，判定与 [`SettlementCtx::for_request`] 同口径）时挂配额/预算
/// 结算；其余形态零结算。两种形态都用带观测的 reframe（逐字节同输出）——
/// 调用方观测回调 `observe` 在两种形态下都按帧收到事件（用量记账与配额
/// 结算共用同一趟分帧）。
///
/// 上游错误类型仅需 `Display`（与 reframe 同一约束），输出统一为
/// `Result<Bytes, std::io::Error>` 供 `Body::from_stream` 使用。
#[allow(clippy::too_many_arguments)] // 结算参数面：config/quota/tenant/body/vendor_type/vendor_id 与观测回调缺一不可
pub fn passthrough_stream_body<S, E, O>(
    config: &Arc<ConfigManager>,
    quota: Option<&Arc<QuotaState>>,
    tenant: &str,
    body: &Value,
    vendor_type: &str,
    vendor_id: &str,
    stream: S,
    observe: O,
) -> Body
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
    O: FnMut(&ReframeObservation) + Send + 'static,
{
    let mut observe = observe;
    // 早退单点：未接线 / 结算不适用（无租户窗且无预算）→ 无结算，仅调用方观测。
    let Some(state) = quota else {
        return Body::from_stream(reframe_passthrough_stream_with_usage(
            stream,
            move |observation| observe(&observation),
        ));
    };
    let snapshot = config.read();
    let has_vendor_budgets = !snapshot.vendor_budgets.is_empty();
    let windows = if snapshot.quota.enabled {
        state.resolved_windows(&snapshot, tenant)
    } else {
        Arc::new(Vec::new())
    };
    if windows.is_empty() && !has_vendor_budgets {
        return Body::from_stream(reframe_passthrough_stream_with_usage(
            stream,
            move |observation| observe(&observation),
        ));
    }
    let (pre_tokens, pre_cost) = precharge_for(
        body,
        &snapshot.model_catalog,
        &snapshot.pricing,
        snapshot.quota.est_chars_per_token.max(1),
    );
    let settler = Arc::new(Mutex::new(PassthroughSettler {
        quota: state.clone(),
        tenant: tenant.to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        vendor_type: vendor_type.to_string(),
        vendor_id: vendor_id.to_string(),
        config: snapshot,
        windows,
        record_vendor: has_vendor_budgets,
        pre_tokens,
        pre_cost,
        usage: None,
    }));
    Body::from_stream(reframe_passthrough_stream_with_usage(
        stream,
        move |observation| {
            // 调用方观测先借看同一帧（用量记账），再 move 进配额结算
            observe(&observation);
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

    /// Responses 预扣：instructions+input+tools 字符折算 token，叠加
    /// `max_output_tokens`；成本按 catalog/pricing 单价。
    #[test]
    fn responses_precharge_estimates_tokens_and_cost() {
        let catalog = vec![ModelCatalogEntry {
            model_id: "gpt-4".into(),
            context_window: 128_000,
            input_price: 1.0,
            output_price: 2.0,
            ..Default::default()
        }];
        let body = serde_json::json!({
            "model": "gpt-4",
            "instructions": "1234",
            "input": "12345678",
            "max_output_tokens": 10,
        });
        let (tokens, cost) = precharge_for(&body, &catalog, &[], 4);
        // instructions 4 + JSON 字符串 10 字符 = 14 → ceil(14/4)=4；+10 = 14
        assert_eq!(tokens, 14);
        // 4/1e6*1 + 10/1e6*2 = 0.000024 USD = 2.4 millicents → ceil 3
        assert_eq!(cost, 3);
    }

    /// rerank 预扣：token 按 query+documents 字符折算，成本按目录每 1k 文档价。
    #[test]
    fn rerank_precharge_estimates_tokens_and_cost() {
        let catalog = vec![ModelCatalogEntry {
            model_id: "rerank-v3".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 2.0,
            ..Default::default()
        }];
        let body = serde_json::json!({
            "model": "rerank-v3",
            "query": "12345678",
            "documents": ["1234", {"text": "12345678"}],
        });
        let (tokens, cost) = precharge_for(&body, &catalog, &[], 4);
        assert_eq!(tokens, 5, "(8+12)/4 向上取整");
        // 2 docs × $2/1k docs = $0.004 → 400 millicents
        assert_eq!(cost, 400);

        // 未登记价：成本 0，token 照算
        let (tokens, cost) = precharge_for(&body, &[], &[], 4);
        assert_eq!(tokens, 5);
        assert_eq!(cost, 0);
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
            rerank_price_per_1k_docs: 0.0,
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

    /// 旧单窗 API 形状的测试辅助：单 rolling 窗（与旧 window_secs 热读等价）。
    fn legacy_windows(token_quota: u64, cost_quota: u64, window_secs: u64) -> Vec<RuntimeWindow> {
        vec![RuntimeWindow {
            id: "default".into(),
            global: false,
            period: Period::Rolling(window_secs),
            tz_offset_secs: 0,
            tokens_limit: token_quota,
            cost_limit_millicents: cost_quota,
        }]
    }

    /// 回补返还：实际低于预估时余额返还。
    #[test]
    fn recompense_refunds_difference() {
        let quota = QuotaState::new();
        let windows = legacy_windows(100, 0, 3600);
        // 限额 100 token：预扣 60 通过。
        quota.consume("k", &windows, 60, 0).expect("首次预扣");
        assert_eq!(quota.snapshot("k", "default"), (60, 0));
        // 实际仅 10 → 回补返还 50。
        quota.recompense("k", &windows, 60, 0, 10, 0);
        assert_eq!(quota.snapshot("k", "default"), (10, 0));
        // 实际超预估时追扣。
        quota.recompense("k", &windows, 10, 0, 30, 0);
        assert_eq!(quota.snapshot("k", "default"), (30, 0));
    }

    /// 回补不建账：无账可回时保持账本为空，正向差额不得虚记进干净窗口。
    #[test]
    fn recompense_without_ledger_does_not_create_account() {
        let quota = QuotaState::new();
        let windows = legacy_windows(60, 0, 3600);
        quota.recompense("ghost", &windows, 60, 0, 10, 0);
        assert_eq!(quota.tracked_keys(), 0, "无账不得新建");
        assert_eq!(quota.snapshot("ghost", "default"), (0, 0));
    }

    /// 回补触发淘汰：超上限时惰性清理已翻转窗口（与 consume 同谓词）。
    #[test]
    fn recompense_triggers_window_eviction() {
        let quota = QuotaState::new();
        // 灌满上限 + 1（淘汰谓词严格大于）——窗口 1s，随后全部翻转。
        for i in 0..=QUOTA_MAX_TRACKED_KEYS {
            quota
                .consume(&format!("k{i}"), &legacy_windows(100, 0, 1), 1, 0)
                .expect("fill");
        }
        assert_eq!(quota.tracked_keys(), QUOTA_MAX_TRACKED_KEYS + 1);
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        // 无账 key 上的回补也要先执行淘汰（谓词同一），且不建账。
        quota.recompense("overflow", &legacy_windows(0, 0, 1), 0, 0, 0, 0);
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
        let windows = legacy_windows(100, 0, 3600);
        quota.consume("k", &windows, 60, 0).expect("首次");
        assert!(quota.consume("k", &windows, 60, 0).is_err());
        assert_eq!(quota.snapshot("k", "default"), (60, 0), "拒收不得落账");
    }

    // ---- 多窗准入（quota-aware-routing T003 / R-quota-001）----

    /// 双 calendar 窗运行时面：daily 限额 tokens_limit_a、weekly 限额 b。
    fn calendar_windows(daily_limit: u64, weekly_limit: u64) -> Vec<RuntimeWindow> {
        vec![
            RuntimeWindow {
                id: "daily".into(),
                global: false,
                period: Period::Daily,
                tz_offset_secs: 0,
                tokens_limit: daily_limit,
                cost_limit_millicents: 0,
            },
            RuntimeWindow {
                id: "weekly".into(),
                global: false,
                period: Period::Weekly,
                tz_offset_secs: 0,
                tokens_limit: weekly_limit,
                cost_limit_millicents: 0,
            },
        ]
    }

    /// 日窗有余 / 周窗尽 → 拒；Retry-After = 周窗重置秒数（受拒窗最晚重置
    /// = 最早可成功重试时刻）；响应体列两窗剩余；受拒请求零落账。
    #[test]
    fn any_window_short_refuses_with_min_retry_after() {
        let quota = QuotaState::new();
        let windows = calendar_windows(5_000, 2_000);
        // 首扣 1500：日窗余 3500、周窗余 500，两窗均放行。
        quota
            .consume("k", &windows, 1500, 0)
            .expect("首扣两窗均有余");
        assert_eq!(quota.snapshot("k", "daily"), (1500, 0));
        assert_eq!(quota.snapshot("k", "weekly"), (1500, 0));
        // 二扣 800：日窗余 3500 有余、周窗余 500 不足 → 拒。
        let refusal = quota.consume("k", &windows, 800, 0).expect_err("周窗尽");
        assert_eq!(
            refusal.retry_after_secs,
            refusal
                .windows
                .iter()
                .find(|w| w.id == "weekly")
                .expect("响应必须列周窗")
                .reset_secs,
            "Retry-After = 受拒窗（周窗）重置秒数"
        );
        assert!(
            refusal.retry_after_secs > 0 && refusal.retry_after_secs <= 7 * DAY_SECS as u64,
            "周窗重置秒数在 (0, 7d] 内"
        );
        let ids: Vec<&str> = refusal.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["daily", "weekly"],
            "响应列两窗剩余（脱敏仅 id+秒数）"
        );
        // 受拒零落账：周窗占用保持 1500。
        assert_eq!(quota.snapshot("k", "weekly"), (1500, 0));
        assert_eq!(quota.snapshot("k", "daily"), (1500, 0));
        // 429 响应体（i18n 文案）列两窗 id 与秒数。
        let body = refusal_body(&refusal);
        assert!(body.contains("daily"), "响应体列日窗: {body}");
        assert!(body.contains("weekly"), "响应体列周窗: {body}");
    }

    /// 全窗有余 → 放行，预扣落各窗。
    #[test]
    fn all_windows_pass_admits() {
        let quota = QuotaState::new();
        let windows = calendar_windows(5_000, 2_000);
        quota.consume("k", &windows, 1200, 0).expect("两窗均有余");
        assert_eq!(quota.snapshot("k", "daily"), (1200, 0));
        assert_eq!(quota.snapshot("k", "weekly"), (1200, 0));
    }

    /// 结算逐窗差额修正：实际 300 → 两窗同降至 300（预扣 1500 中 1200 返还）。
    #[test]
    fn recompense_settles_every_window_independently() {
        let quota = QuotaState::new();
        let windows = calendar_windows(5_000, 2_000);
        quota.consume("k", &windows, 1500, 0).expect("预扣");
        quota.recompense("k", &windows, 1500, 0, 300, 0);
        assert_eq!(quota.snapshot("k", "daily"), (300, 0), "日窗逐窗结算");
        assert_eq!(quota.snapshot("k", "weekly"), (300, 0), "周窗逐窗结算");
    }

    /// scope=global 窗全体请求共享同一账本键（`__global__`），与 api_key 窗
    /// 互不串账（R-quota-001 / design 决策 3）。
    #[test]
    fn global_window_shares_state_across_tenants() {
        let quota = QuotaState::new();
        let windows = vec![RuntimeWindow {
            id: "fleet".into(),
            global: true,
            period: Period::Rolling(3600),
            tz_offset_secs: 0,
            tokens_limit: 1000,
            cost_limit_millicents: 0,
        }];
        quota.consume("tenant-a", &windows, 600, 0).expect("a 扣");
        quota
            .consume("tenant-b", &windows, 600, 0)
            .expect_err("b 撞同一全局窗");
        assert_eq!(
            quota.snapshot(GLOBAL_LEDGER_KEY, "fleet"),
            (600, 0),
            "全局窗共享占用，b 的受拒不落账"
        );
        assert_eq!(quota.tracked_keys(), 1, "全局窗只建一个账本键");
    }

    // ---- vendor 预算记账（quota-aware-routing T006 / R-vbr-001）----

    /// vendor 预算配置（单 vendor rolling token 预算）。
    fn vendor_budget_config() -> crate::config::Config {
        let mut config = quota_enabled_config(true);
        config.vendor_budgets = vec![crate::config::VendorBudgetConfig {
            vendor_id: "budgeted".into(),
            tokens_limit: 10_000,
            cost_limit_millicents: 0,
            period: "rolling".into(),
            rolling_secs: 3600,
            ..Default::default()
        }];
        config
    }

    /// 结算累计进 VendorBudgetState；无预算 vendor 不参与（R-vbr-001）。
    #[test]
    fn usage_settles_into_vendor_budget_state() {
        let config = Arc::new(ConfigManager::new(vendor_budget_config()));
        let quota = Arc::new(QuotaState::new());
        let snapshot = config.read();

        // 有预算 vendor：usage 结算点累计
        quota.record_vendor_usage(&snapshot, "budgeted", 1200, 30);
        assert_eq!(
            quota.vendor_snapshot("budgeted", &vendor_window_id("budgeted")),
            (1200, 30),
            "结算累计进预算态"
        );
        quota.record_vendor_usage(&snapshot, "budgeted", 300, 0);
        assert_eq!(
            quota.vendor_snapshot("budgeted", &vendor_window_id("budgeted")),
            (1500, 30),
            "多次结算累加"
        );

        // 无预算 vendor：零参与（不建账）
        quota.record_vendor_usage(&snapshot, "plain", 500, 5);
        assert_eq!(
            quota.vendor_snapshot("plain", &vendor_window_id("plain")),
            (0, 0),
            "无预算 vendor 不参与"
        );
    }

    /// 读侧惰性翻转：账本态停在上一窗时，读侧必须归零而非暴露上一窗占用。
    ///
    /// 缺陷场景（回归钉）：窗过期 + 此后该 vendor 零流量 → 写侧 `flip_if_needed`
    /// 不会被触发 → 若读侧照搬 `token_used`，`depleted()` 恒 true，smart 模式把
    /// 该 vendor **永久**摘出候选（无结算即无翻转机会）。
    #[test]
    fn vendor_budget_view_zeroes_expired_window() {
        use crate::quota_contract::VendorBudgetSource as _;

        let config = Arc::new(ConfigManager::new(vendor_budget_config()));
        let quota = Arc::new(QuotaState::new());
        let snapshot = config.read();

        // 灌满预算（limit=10_000，用 10_000 即耗尽）
        quota.record_vendor_usage(&snapshot, "budgeted", 10_000, 0);
        let view = quota
            .vendor_budget_view(&snapshot, "budgeted")
            .expect("有预算 vendor 必有视图");
        assert!(view.depleted(), "窗内足额应判耗尽");

        // 把账本态回拨到上一窗（rolling 1h，回拨 2h 必然过期）
        {
            let mut guard = quota
                .vendor_budgets
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let state = guard
                .get_mut("budgeted")
                .and_then(|m| m.get_mut(&vendor_window_id("budgeted")))
                .expect("预算态已建立");
            let stale = chrono::Utc::now().timestamp() - 7_200;
            state.window_start = Some(stale);
        }

        // 读侧必须按新窗归零：不再耗尽、占用为 0
        let view = quota
            .vendor_budget_view(&snapshot, "budgeted")
            .expect("有预算 vendor 必有视图");
        assert_eq!(view.tokens_used, 0, "过期窗占用不得暴露给读侧");
        assert!(
            !view.depleted(),
            "过期窗不得判耗尽（否则 vendor 被永久摘除）"
        );
        // 账本态本身不被读侧改写（读不产生副作用）
        assert_eq!(
            quota.vendor_snapshot("budgeted", &vendor_window_id("budgeted")),
            (10_000, 0),
            "读侧不得改写账本态"
        );
    }

    /// settle_nonstream 在租户配额不适用（无限额租户）时仍记 vendor 预算——
    /// 预算是 vendor 维度约束，独立于租户限额（快路径仅在两者皆无时保持零解析）。
    #[test]
    fn settle_nonstream_records_vendor_budget_without_tenant_quota() {
        // 租户无限额（auth.keys 未命中）→ tenant_windows 空；vendor 有预算。
        let config = Arc::new(ConfigManager::new(vendor_budget_config()));
        let quota = Arc::new(QuotaState::new());
        let body = chat_body("hello!", 10);
        let response = serde_json::json!({
            "usage": {"prompt_tokens": 50, "completion_tokens": 20}
        });
        settle_nonstream(
            &config,
            Some(&quota),
            "unknown-tenant",
            &body,
            &response,
            "openai",
            "budgeted",
            None,
        );
        let (tokens, cost) = quota.vendor_snapshot("budgeted", &vendor_window_id("budgeted"));
        assert_eq!(tokens, 70, "实际 usage（50+20）进预算态");
        assert!(cost > 0, "成本维度按实际计价累计: {cost}");
        // 租户窗不建账（无限额租户）
        assert_eq!(quota.tracked_keys(), 0, "无限额租户零账面");
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

    // 结算门控与 Arc<Config> 快照

    /// quota 开启 + 已知租户限额的配置（结算门控单测共用）。
    fn quota_enabled_config(enabled: bool) -> crate::config::Config {
        crate::config::Config {
            masking: Default::default(),
            web_search: Default::default(),
            rate_limit: Default::default(),
            otlp: Default::default(),
            vendor_budgets: Vec::new(),
            key_budgets: Default::default(),
            usage_insights: Default::default(),
            catalog_sync: Default::default(),
            subscription: Default::default(),
            subscription_accounts: Vec::new(),
            metrics: Default::default(),
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
            lexcompress: Default::default(),
            responses: Default::default(),
        }
    }

    /// 三级早退单点：适用（Some）/ 生效窗集空 / enabled=false / 未接线（None）。
    #[test]
    fn quota_applies_three_level_gate() {
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let quota = Arc::new(QuotaState::new());

        let applied = quota_applies(&config, Some(&quota), "tenant-digest");
        assert!(
            applied.is_some_and(|lease| {
                lease.windows.len() == 1
                    && lease.windows[0].id == "default"
                    && lease.windows[0].tokens_limit == 1_000_000
                    && lease.windows[0].cost_limit_millicents == 0
            }),
            "已启用 + 租户有限额 → 适用，限额回填进生效窗"
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
            "openai",
            "vendor-x",
            None
        )
        .is_some());
        assert!(
            SettlementCtx::for_request(
                &config,
                Some(&quota),
                "unknown",
                &body,
                "openai",
                "vendor-x",
                None
            )
            .is_none(),
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
            .consume(
                "tenant-digest",
                &legacy_windows(1_000_000, 0, 3600),
                pre_tokens,
                pre_cost,
            )
            .expect("入口预扣");
        assert_eq!(
            quota.snapshot("tenant-digest", "default"),
            (pre_tokens, pre_cost)
        );

        settle_nonstream_cached(&config, Some(&quota), "tenant-digest", &body);
        assert_eq!(
            quota.snapshot("tenant-digest", "default"),
            (0, 0),
            "HIT 以实际用量 0 结算，预扣全额回补"
        );

        // 门控形态：未接线 → no-op（不 panic、不建账）。
        let other = QuotaState::new();
        settle_nonstream_cached(&config, None, "tenant-digest", &body);
        assert_eq!(other.tracked_keys(), 0);
    }

    /// Gemini 预扣：parts 文本 + systemInstruction + tools 字符 + maxOutputTokens。
    #[test]
    fn gemini_precharge_counts_text_and_output_budget() {
        let body = serde_json::json!({
            "model": "m",
            "systemInstruction": {"parts": [{"text": "sys"}]},
            "contents": [{"role": "user", "parts": [{"text": "hello!"}]}],
            "tools": [{"functionDeclarations": [{"name": "f"}]}],
            "generationConfig": {"maxOutputTokens": 10}
        });
        let (tokens, cost) = precharge_for(&body, &[], &pricing(), 4);
        assert!(tokens > 10, "预估 = 输入 token + maxOutputTokens: {tokens}");
        assert!(cost > 0, "单价命中后成本必须非零: {cost}");

        let embed = serde_json::json!({
            "model": "m",
            "content": {"parts": [{"text": "hello!"}]}
        });
        let (embed_tokens, _) = precharge_for(&embed, &[], &pricing(), 4);
        assert_eq!(embed_tokens, 2, "embedding 无输出预算，只计文本");
    }

    #[tokio::test]
    async fn gemini_settlement_refunds_to_actual_usage() {
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let quota = Arc::new(QuotaState::new());
        let body = serde_json::json!({
            "model": "m",
            "contents": [{"role": "user", "parts": [{"text": "hello!"}]}],
            "generationConfig": {"maxOutputTokens": 10}
        });
        let (pre_tokens, pre_cost) = precharge_for(&body, &[], &pricing(), 4);
        quota
            .consume(
                "tenant-digest",
                &legacy_windows(1_000_000, 0, 3600),
                pre_tokens,
                pre_cost,
            )
            .expect("入口预扣");
        assert_eq!(
            quota.snapshot("tenant-digest", "default"),
            (pre_tokens, pre_cost)
        );

        let response = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 2,
                "candidatesTokenCount": 1,
                "totalTokenCount": 3
            }
        });
        settle_nonstream(
            &config,
            Some(&quota),
            "tenant-digest",
            &body,
            &response,
            "gemini",
            "v",
            None,
        );
        let (tokens, cost) = quota.snapshot("tenant-digest", "default");
        assert_eq!(tokens, 3, "按实际 prompt+completion 结算");
        assert!(
            cost <= pre_cost,
            "回补后成本不得高于预扣: {cost} > {pre_cost}"
        );
    }

    #[test]
    fn gemini_model_from_path_parses_action_suffix() {
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-2.5-pro:generateContent").as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(
            gemini_model_from_path("/v1beta/models/models/gemini-2.5-pro:embedContent").as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(gemini_model_from_path("/v1beta/models"), None);
        assert_eq!(gemini_model_from_path("/v1/chat/completions"), None);
    }

    #[test]
    fn frame_usage_reads_gemini_usage_metadata() {
        let data = r#"{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":2,"cachedContentTokenCount":3}}"#;
        assert_eq!(frame_usage(data), Some((10, 7, 3, 0)));
        assert_eq!(frame_usage("[DONE]"), None);
        assert_eq!(frame_usage(r#"{"candidates":[]}"#), None);
    }

    // ---- 修复回归（quota-aware-routing 审查意见）----

    /// scope=global 窗不回填 per-key 限额：限额只取配置显式值（token 维度
    /// 0 = 不判）；无限额租户下全局窗仍然生效（不双零剔除、不整体绕过）。
    #[test]
    fn global_window_limits_skip_tenant_backfill() {
        let mut config = quota_enabled_config(true);
        config.quota.windows = vec![crate::config::QuotaWindowConfig {
            id: "fleet".into(),
            scope: "global".into(),
            period: "rolling".into(),
            rolling_secs: 86_400,
            tokens_limit: 0,
            cost_limit_millicents: 500_000,
            ..Default::default()
        }];
        let resolved = resolve_windows(&config, "tenant-digest");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].tokens_limit, 0,
            "global 窗 token 维度不落回 per-key（判定随请求者漂移即租户 A 定舰队限额）"
        );
        assert_eq!(resolved[0].cost_limit_millicents, 500_000);
        assert!(resolved[0].global);
        // 无限额租户：窗不被剔除（全局约束对全体一致）
        let resolved = resolve_windows(&config, "unknown-tenant");
        assert_eq!(resolved.len(), 1, "global 窗对无限额租户同样生效");

        // api_key 窗行为不变：0 落回 per-key、双零剔除
        config.quota.windows = vec![crate::config::QuotaWindowConfig {
            id: "perkey".into(),
            period: "rolling".into(),
            rolling_secs: 3600,
            tokens_limit: 0,
            cost_limit_millicents: 0,
            ..Default::default()
        }];
        let resolved = resolve_windows(&config, "tenant-digest");
        assert_eq!(resolved.len(), 1, "有限额租户回填后生效");
        assert_eq!(
            resolved[0].tokens_limit, 1_000_000,
            "回填 per-key token_quota"
        );
        assert!(
            resolve_windows(&config, "unknown-tenant").is_empty(),
            "无限额租户双零剔除"
        );
    }

    /// 生效窗集缓存：同配置快照同租户第二趟命中（零重解析），配置更换
    /// （新快照）整代作废重建。
    #[test]
    fn resolved_windows_cache_hits_and_invalidates_on_config_swap() {
        let quota = QuotaState::new();
        let config = Arc::new(ConfigManager::new(quota_enabled_config(true)));
        let snapshot = config.read();
        let first = quota.resolved_windows(&snapshot, "tenant-digest");
        assert_eq!(first.len(), 1);
        let hit = quota.resolved_windows(&snapshot, "tenant-digest");
        assert!(
            Arc::ptr_eq(&first, &hit),
            "同快照同租户必须命中同一缓存产物"
        );
        // 配置热更换：新快照指针 → 缓存重建（产物为新解析）。手构管理器
        // 无落盘路径，内存切换走 swap_memory（与热载桥接同入口）；校验要求
        // key hash 为合法摘要，换用 64 位 hex 摘要租户。
        let digest = "a".repeat(64);
        let mut updated = quota_enabled_config(true);
        updated.quota.est_chars_per_token = 8;
        updated.auth.as_mut().unwrap().keys[0].hash = digest.clone();
        config.swap_memory(updated).expect("swap");
        let fresh = quota.resolved_windows(&config.read(), &digest);
        assert!(!Arc::ptr_eq(&first, &fresh), "配置更换后代锚失效，产物重建");
        assert_eq!(fresh.len(), 1, "重建后解析结果等价");
    }

    /// 账本保留下界推导：取配置最长窗（monthly 取 31 天上界）；无窗兜底
    /// 月窗上界。
    #[test]
    fn ledger_retention_tracks_longest_configured_window() {
        let mut config = quota_enabled_config(false);
        assert_eq!(
            ledger_retention_secs(&config),
            MAX_MONTH_SECS,
            "无窗配置兜底月窗上界（防短保留误删活动行）"
        );
        config.quota.enabled = true;
        config.quota.windows = vec![crate::config::QuotaWindowConfig {
            id: "d".into(),
            period: "calendar".into(),
            calendar: "daily".into(),
            tokens_limit: 100,
            ..Default::default()
        }];
        assert_eq!(ledger_retention_secs(&config), DAY_SECS);
        config.quota.windows.push(crate::config::QuotaWindowConfig {
            id: "r".into(),
            period: "rolling".into(),
            rolling_secs: 90 * 86_400,
            tokens_limit: 100,
            ..Default::default()
        });
        assert_eq!(ledger_retention_secs(&config), 90 * DAY_SECS);
        config.quota.windows = vec![crate::config::QuotaWindowConfig {
            id: "m".into(),
            period: "calendar".into(),
            calendar: "monthly".into(),
            tokens_limit: 100,
            ..Default::default()
        }];
        assert_eq!(
            ledger_retention_secs(&config),
            MAX_MONTH_SECS,
            "monthly 取上界 31 天（宁多留不误删）"
        );
    }

    /// 恢复灌回套内存上限：超限键跳过（活动键保持），不无界灌回。
    #[test]
    fn restore_caps_ledger_keys_at_memory_cap() {
        let quota = QuotaState::new();
        let config = quota_enabled_config(true);
        let now = chrono::Utc::now().timestamp();
        // 旧单窗编译产物为 rolling 窗（起点 = 窗内首次用量 = now）
        let rows: Vec<crate::database::QuotaLedgerRow> = (0..=QUOTA_MAX_TRACKED_KEYS)
            .map(|i| crate::database::QuotaLedgerRow {
                api_key_hash: format!("k{i}"),
                window_id: "default".into(),
                window_start: now,
                tokens_used: 100,
                cost_used_millicents: 0,
                updated_at: now,
            })
            .collect();
        let restored = quota.restore_from_ledger(&rows, &config);
        assert_eq!(
            quota.tracked_keys(),
            QUOTA_MAX_TRACKED_KEYS,
            "灌回必须受 cap 约束"
        );
        assert_eq!(restored, QUOTA_MAX_TRACKED_KEYS, "超限键整行跳过");
    }

    /// 淘汰迟滞：零命中淘汰（全活跃）后水位内不再扫描（过期键暂留）；
    /// 越过水位（增长 ≥ 迟滞值）恢复淘汰并清掉全部过期键。
    #[test]
    fn evict_hysteresis_defers_rescan_until_watermark() {
        let quota = QuotaState::new();
        let active = legacy_windows(100, 0, 3600);
        // 超上限 + 1：淘汰执行一次，零命中（全活跃）→ 水位 = len + 迟滞
        for i in 0..=QUOTA_MAX_TRACKED_KEYS {
            quota
                .consume(&format!("k{i}"), &active, 1, 0)
                .expect("fill");
        }
        quota.consume("probe", &active, 1, 0).expect("probe");
        assert_eq!(quota.tracked_keys(), QUOTA_MAX_TRACKED_KEYS + 2);
        // 1s 窗键随后过期，但 len < 水位 → 本趟 consume 跳过淘汰（过期键暂留）
        let flash = legacy_windows(100, 0, 1);
        quota.consume("flash-a", &flash, 1, 0).expect("flash");
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        quota
            .consume("probe2", &active, 1, 0)
            .expect("within watermark");
        assert_eq!(
            quota.tracked_keys(),
            QUOTA_MAX_TRACKED_KEYS + 4,
            "水位内不得重复全表淘汰（过期 flash-a 暂留）"
        );
        // 越过水位（再灌迟滞值个活跃键）→ 淘汰恢复，唯一过期键 flash-a 被清
        for i in 0..QUOTA_EVICT_HYSTERESIS {
            quota
                .consume(&format!("g{i}"), &active, 1, 0)
                .expect("cross watermark");
        }
        assert_eq!(
            quota.tracked_keys(),
            QUOTA_MAX_TRACKED_KEYS + 4 + QUOTA_EVICT_HYSTERESIS - 1,
            "越过水位后淘汰恢复且恰好清掉过期键"
        );
    }
}
