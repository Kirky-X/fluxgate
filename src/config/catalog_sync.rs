// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 模型目录自动同步（catalog-sync feature）：外部源拉取 → `CatalogEntryDraft`
//! 映射 → 只增不改合并。映射依据 2026-10-28 实测 models.dev `api.json`
//! 形态：顶层 provider slug → `{models: {<model_id>: {cost, limit, ...}}}`。

use serde::Deserialize;

use super::model_catalog::{ModelCatalogEntry, PricingCurrency};
use super::CatalogSyncSection;

/// 单模型草稿 — 源模型映射出的 `[[model_catalog]]` 候选条目（尚未落目录；
/// `vendor_id` 来自 alias 表或源 provider slug）。
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntryDraft {
    pub vendor_id: String,
    pub model_id: String,
    pub context_window: u64,
    pub input_price: f64,
    pub output_price: f64,
    pub cache_read_price: f64,
    pub cache_creation_price: f64,
    /// 1h TTL 写档价：catalog 外部源（models.dev）无此档 → 恒 0.0（结算侧
    /// 拆分缺失时整体按基础写价，见 billing 1h 档语义）
    pub cache_creation_1h_price: f64,
    /// 价格来源：`Source` = 四档全部来自源；`Fallback` = 至少一档源缺失、
    /// 以 catalog 缺省 0 价回退（diff 响应据此标注 `price_source`）
    pub price_source: PriceSource,
    pub supports_tools: bool,
    pub supports_thinking: bool,
    pub max_output_tokens: Option<u64>,
}

/// 草稿价格来源标记（design Consequences：单源字段缺口 → fallback 档，
/// diff 响应显式标注，不伪装成源价）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Source,
    Fallback,
}

/// 解析统计：跳过计数（规则 4：跳过必须显性可数，不静默丢弃）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// 缺 `cost`（或四档全缺）跳过的模型数
    pub skipped_missing_pricing: usize,
    /// 缺 `limit.context`（或为 0）跳过的模型数（catalog 登记
    /// context_window 必须 > 0，validate_config 硬约束）
    pub skipped_missing_context: usize,
}

/// models.dev 源 provider 层（未知字段忽略）
#[derive(Debug, Deserialize)]
struct SourceProvider {
    #[serde(default)]
    models: serde_json::Map<String, serde_json::Value>,
}

/// 解析外部目录源 → 草稿列表（alias 表之外的裸 slug 形态；alias 由
/// [`apply_aliases`] 单独一步应用）。解析失败由调用方按 fail-open 处理。
pub fn parse_source(raw: &[u8]) -> Vec<CatalogEntryDraft> {
    parse_source_with_stats(raw, &mut ParseStats::default()).unwrap_or_default()
}

/// 同 [`parse_source`]，附带跳过计数（测试与管理面 skipped 字段消费）；
/// JSON 非法时返回 Err（编排层计入 fail 指标并在 admin 响应带错误详情）。
pub fn parse_source_with_stats(
    raw: &[u8],
    stats: &mut ParseStats,
) -> Result<Vec<CatalogEntryDraft>, String> {
    let providers: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(raw)
        .map_err(|error| {
            tracing::warn!(
                "{}",
                crate::i18n::t_args("catalog-sync-parse-failed", &[("error", error.to_string())])
            );
            error.to_string()
        })?;
    let mut drafts = Vec::new();
    for (slug, provider) in &providers {
        let Ok(provider) = serde_json::from_value::<SourceProvider>(provider.clone()) else {
            continue;
        };
        for (model_key, model_value) in &provider.models {
            let Ok(model) = serde_json::from_value::<SourceModel>(model_value.clone()) else {
                continue;
            };
            let Some(cost) = model.cost else {
                stats.skipped_missing_pricing += 1;
                continue;
            };
            let context_window = model.limit.as_ref().map(|l| l.context).unwrap_or(0);
            if context_window == 0 {
                stats.skipped_missing_context += 1;
                continue;
            }
            let mut price_source = PriceSource::Source;
            let mut tier = |value: Option<f64>| -> f64 {
                match value {
                    Some(v) => super::unit::source_price_to_catalog_price(v),
                    None => {
                        price_source = PriceSource::Fallback;
                        0.0
                    }
                }
            };
            let input_price = tier(cost.input);
            let output_price = tier(cost.output);
            let cache_read_price = tier(cost.cache_read);
            let cache_creation_price = tier(cost.cache_write);
            drafts.push(CatalogEntryDraft {
                vendor_id: slug.clone(),
                model_id: model_key.clone(),
                context_window,
                input_price,
                output_price,
                cache_read_price,
                cache_creation_price,
                // models.dev 无独立 1h cache write 档位：暂 0.0（未配置不产生成本）
                cache_creation_1h_price: 0.0,
                price_source,
                supports_tools: model.tool_call,
                supports_thinking: model.reasoning,
                max_output_tokens: model.limit.and_then(|l| l.output),
            });
        }
    }
    Ok(drafts)
}

/// 应用 `[catalog_sync.alias]` 表：命中 slug 改写 `vendor_id`，未命中保持
/// slug 本身（配置语义：缺省 slug 即 vendor_id）。
pub fn apply_aliases(
    mut drafts: Vec<CatalogEntryDraft>,
    alias: &std::collections::HashMap<String, String>,
) -> Vec<CatalogEntryDraft> {
    for draft in &mut drafts {
        if let Some(vendor_id) = alias.get(&draft.vendor_id) {
            draft.vendor_id = vendor_id.clone();
        }
    }
    drafts
}

/// 进程内单飞标记：admin 触发与周期任务共享，重入得 [`SyncError::InFlight`]
///（design：并发防重入 AtomicBool）。
static SYNC_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 上游 ETag 缓存（进程内）：成功 Body 后回存，下次请求作 If-None-Match；
/// 304 命中时保持不变。
static LAST_ETAG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 单飞守卫：持有期间 SYNC_IN_FLIGHT = true，drop 复位（含 panic 路径）。
struct InFlightGuard;

impl InFlightGuard {
    fn claim() -> Option<Self> {
        SYNC_IN_FLIGHT
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| Self)
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        SYNC_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// 同步编排失败 — admin 侧映射：InFlight → 409、Fetch(TooLarge) → 413、
/// Fetch/Parse 其余 → 502、Persist → 500（GateError 语义；GateError 非
/// Clone/Eq，本枚举不 derive 比较）
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("catalog sync already in flight")]
    InFlight,
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("source parse failed: {0}")]
    Parse(String),
    #[error(transparent)]
    Persist(#[from] crate::error::GateError),
}

/// 拉取+解析+合并的单轮产物（落盘判定由调用方做）
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    /// If-None-Match 命中：上游无变化，目录零改动
    NotModified,
    /// 合并计划 + 本次响应 ETag（回存供下轮条件请求）
    Plan(Box<SyncPlan>, Option<String>),
}

/// 拉取上游 → 解析 → alias → 与现有目录只增不改合并。
///
/// 持有进程内单飞标记（重入 [`SyncError::InFlight`]）；网络/解析失败按
/// reason 计 `catalog_sync_fail_total`（fail-open：目录原样）。
pub async fn plan_from_source(
    section: &CatalogSyncSection,
    existing: &[ModelCatalogEntry],
    metrics: &crate::utils::metrics::GlobalMetrics,
) -> Result<PlanOutcome, SyncError> {
    let Some(_guard) = InFlightGuard::claim() else {
        return Err(SyncError::InFlight);
    };
    let result = plan_from_source_inner(section, existing).await;
    match &result {
        Err(SyncError::Fetch(e)) => metrics.record_catalog_sync_fail(e.reason()),
        Err(SyncError::Parse(_)) => metrics.record_catalog_sync_fail("parse"),
        Err(SyncError::InFlight) | Err(SyncError::Persist(_)) => {}
        Ok(PlanOutcome::NotModified) | Ok(PlanOutcome::Plan(..)) => {}
    }
    result
}

async fn plan_from_source_inner(
    section: &CatalogSyncSection,
    existing: &[ModelCatalogEntry],
) -> Result<PlanOutcome, SyncError> {
    let syncer = CatalogSyncer::new(section.timeout_secs)?;
    let etag = LAST_ETAG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match syncer
        .fetch(section.source_url.trim(), etag.as_deref())
        .await?
    {
        FetchOutcome::NotModified => Ok(PlanOutcome::NotModified),
        FetchOutcome::Body { bytes, etag } => {
            let mut stats = ParseStats::default();
            let drafts = parse_source_with_stats(&bytes, &mut stats)
                .map_err(SyncError::Parse)
                .map(|drafts| apply_aliases(drafts, &section.alias))?;
            if stats.skipped_missing_pricing > 0 || stats.skipped_missing_context > 0 {
                tracing::debug!(
                    skipped_pricing = stats.skipped_missing_pricing,
                    skipped_context = stats.skipped_missing_context,
                    "{}",
                    crate::i18n::t("catalog-sync-source-skipped")
                );
            }
            let plan = merge(existing, drafts);
            // ETag 不在此回存：persist_plan 失败（如负价校验拒绝）时下轮
            // If-None-Match 命中 304 会短路重拉——该批新增永久丢失。由调用方
            // persist 成功后经 [`commit_etag`] 回存（见其 doc）。
            Ok(PlanOutcome::Plan(Box::new(plan), etag))
        }
    }
}

/// persist 成功后的 ETag 回存（下轮 If-None-Match 生效）。persist 失败/
/// dry_run **不得**调用：etag 未回存时下轮全量重拉，未落盘批次得以重试
/// ——先回存后落盘会让 304 短路吞掉该批新增（直到上游内容再变）。
pub fn commit_etag(etag: Option<&str>) {
    *LAST_ETAG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = etag.map(str::to_string);
}

/// 合并计划落盘（仅非 dry_run 且有新增时调用）：走既有 admin 配置写入
/// 路径（update_config_with → validate_config → 原子落盘 → 切内存热生效）；
/// 落盘成功计 `catalog_sync_added_total`（dry_run 不计）。
pub fn persist_plan(
    manager: &crate::config::ConfigManager,
    plan: &SyncPlan,
    metrics: &crate::utils::metrics::GlobalMetrics,
) -> Result<(), SyncError> {
    if plan.additions.is_empty() {
        return Ok(());
    }
    manager.update_config_with(|config| {
        for draft in &plan.additions {
            config.model_catalog.push(draft.to_entry());
        }
        Ok(())
    })?;
    metrics.record_catalog_sync_added(plan.additions.len() as u64);
    Ok(())
}

/// diff 展示条目（admin 响应 `entries[]`；`price_source: "fallback"` 标注
/// 回退档——design Consequences：单源字段缺口不伪装成源价）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SyncEntryView {
    pub vendor_id: String,
    pub model_id: String,
    pub price_source: &'static str,
}

/// admin 响应体（design：`{added, skipped, unchanged, etag}` + diff 明细）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SyncReport {
    pub dry_run: bool,
    /// `ok`（有合并计划或零新增）
    pub status: &'static str,
    pub added: usize,
    pub skipped: usize,
    pub duplicates: usize,
    pub unchanged: usize,
    pub etag: Option<String>,
    pub entries: Vec<SyncEntryView>,
}

impl SyncReport {
    /// 从合并计划构造（dry_run 与非 dry_run 共用形状，dry_run 位区分）
    pub fn from_plan(dry_run: bool, plan: &SyncPlan, etag: Option<String>) -> Self {
        Self {
            dry_run,
            status: "ok",
            added: plan.additions.len(),
            skipped: plan.skipped_existing,
            duplicates: plan.duplicates_dropped,
            unchanged: plan.unchanged,
            etag,
            entries: plan
                .additions
                .iter()
                .map(|d| SyncEntryView {
                    vendor_id: d.vendor_id.clone(),
                    model_id: d.model_id.clone(),
                    price_source: match d.price_source {
                        PriceSource::Source => "source",
                        PriceSource::Fallback => "fallback",
                    },
                })
                .collect(),
        }
    }
}

/// models.dev 源模型层（未知字段忽略；价格与窗口字段全部可缺）
#[derive(Debug, Deserialize)]
struct SourceModel {
    #[serde(default)]
    cost: Option<SourceCost>,
    #[serde(default)]
    limit: Option<SourceLimit>,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
}

#[derive(Debug, Deserialize)]
struct SourceCost {
    input: Option<f64>,
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SourceLimit {
    #[serde(default)]
    context: u64,
    #[serde(default)]
    output: Option<u64>,
}

/// 响应体大小上限：8MB（R-catalog-001；超限拒绝不搬运，admin 侧映射 413）
pub const MAX_SOURCE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 单次拉取结果：304 短路（上游未变化）或携带 body + ETag 的完整响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    NotModified,
    Body {
        bytes: Vec<u8>,
        /// 上游 ETag（原样回存，下次请求作 If-None-Match）；上游未给则 None
        etag: Option<String>,
    },
}

/// 拉取失败 — reason 标签供 `catalog_sync_fail_total{reason}` 计数与
/// admin 错误详情（fail-open：调用方保持现 catalog 不动）
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("request timed out")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
    #[error("upstream returned HTTP {0}")]
    HttpStatus(u16),
    #[error("response body exceeds 8MB cap")]
    TooLarge,
    #[error("failed to build HTTP client: {0}")]
    Client(String),
}

impl FetchError {
    /// 指标 reason 标签（有界枚举，不透出网络错误原文——基数可控）
    pub fn reason(&self) -> &'static str {
        match self {
            FetchError::Timeout => "timeout",
            FetchError::Network(_) => "network",
            FetchError::HttpStatus(_) => "http_status",
            FetchError::TooLarge => "too_large",
            FetchError::Client(_) => "client",
        }
    }
}

/// 目录源拉取器：GET `source_url`，带 If-None-Match 条件请求与 8MB
/// 体积上限；总超时取 `[catalog_sync].timeout_secs`。
pub struct CatalogSyncer {
    client: reqwest::Client,
}

impl CatalogSyncer {
    pub fn new(timeout_secs: u64) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
            .build()
            .map_err(|e| FetchError::Client(e.to_string()))?;
        Ok(Self { client })
    }

    /// 拉取目录源：`etag` 非空时携带 If-None-Match；304 → [`FetchOutcome::
    /// NotModified`]；超限（Content-Length 预判或流式累计）→ TooLarge。
    pub async fn fetch(
        &self,
        source_url: &str,
        etag: Option<&str>,
    ) -> Result<FetchOutcome, FetchError> {
        let mut request = self.client.get(source_url);
        if let Some(tag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, tag);
        }
        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network(e.to_string())
            }
        })?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified);
        }
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus(response.status().as_u16()));
        }

        // Content-Length 预判：超限直接拒绝（不启动搬运）
        if let Some(len) = response.content_length() {
            if len as usize > MAX_SOURCE_BODY_BYTES {
                return Err(FetchError::TooLarge);
            }
        }

        // 流式累计搬运：无 Content-Length（chunked）或长度缺失时同样受
        // 上限约束（滴漏响应不能绕过体积门）。
        // 用 reqwest 自带 chunk() 轮询（futures-util 是 dev 依赖，lib 不引入）
        let etag_header = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network(e.to_string())
            }
        })? {
            if bytes.len() + chunk.len() > MAX_SOURCE_BODY_BYTES {
                return Err(FetchError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(FetchOutcome::Body {
            bytes,
            etag: etag_header,
        })
    }
}

impl CatalogEntryDraft {
    /// 草稿 → `[[model_catalog]]` 条目：源价为 USD 计价；rerank/长上下文价
    /// 源无此语义落 0；缓存断点策略 None（条目级未声明 = 内置默认）。
    pub fn to_entry(&self) -> ModelCatalogEntry {
        ModelCatalogEntry {
            model_id: self.model_id.clone(),
            context_window: self.context_window,
            rerank_price_per_1k_docs: 0.0,
            input_price: self.input_price,
            output_price: self.output_price,
            cache_read_price: self.cache_read_price,
            cache_creation_price: self.cache_creation_price,
            long_context_input_price: 0.0,
            cache_creation_1h_price: 0.0,
            long_context_threshold_tokens: 0,
            currency: PricingCurrency::Usd,
            max_output_tokens: self.max_output_tokens,
            supports_tools: self.supports_tools,
            supports_thinking: self.supports_thinking,
            cache_breakpoints: None,
        }
    }
}

/// 只增不改合并计划 — dry_run 直接序列化为 diff 响应；非 dry_run 按顺序
/// 经既有 admin 配置写入路径落盘。
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPlan {
    /// 允许新增的草稿（目录缺失的 (vendor, model)），保持源出现序
    pub additions: Vec<CatalogEntryDraft>,
    /// 既有目录中未被任何草稿命中的条目数（本次同步零改动）
    pub unchanged: usize,
    /// 因 (vendor, model) 已存在被丢弃的草稿数
    pub skipped_existing: usize,
    /// 草稿间重复（同 model_id 二次出现）被去重的条数（先到先得）
    pub duplicates_dropped: usize,
}

/// 只增不改合并（不可配置的硬策略，delta spec Constraints）：仅新增目录
/// 缺失的 `(vendor_id, model_id)`，既有条目（含用户自定价）零改动。
///
/// 键口径：`ModelCatalogEntry` 主键是 `model_id`（`validate_config` 强制
/// 目录去重、条目不携带 vendor 维度），故「已存在」按 `model_id` 判定——
/// 多 provider 重复收录同一模型名时只保留首条草稿，正是只增不改在
/// 现有目录模型上的保守化（specs R-catalog-003 的 (vendor, model) 键在
/// 单键目录上退化为 model_id，语义覆盖见测试 `merge_adds_missing_
/// never_touches_existing` 的同 model_id 异 vendor 用例）。
pub fn merge(existing: &[ModelCatalogEntry], drafts: Vec<CatalogEntryDraft>) -> SyncPlan {
    let mut seen: std::collections::HashSet<String> =
        existing.iter().map(|e| e.model_id.clone()).collect();
    let mut plan = SyncPlan {
        unchanged: existing.len(),
        additions: Vec::new(),
        skipped_existing: 0,
        duplicates_dropped: 0,
    };
    for draft in drafts {
        if seen.contains(&draft.model_id) {
            // 既有命中与草稿间重复同走「不落」路径，分列计数供 diff 展示
            if plan.additions.iter().any(|a| a.model_id == draft.model_id) {
                plan.duplicates_dropped += 1;
            } else {
                plan.skipped_existing += 1;
            }
            continue;
        }
        seen.insert(draft.model_id.clone());
        plan.additions.push(draft);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/config/fixtures/models_dev.json"
    ));

    /// fixture 解析与单位换算（R-catalog-002）：2026-10-28 实测 api.json
    /// 裁剪落盘——四档齐全 / 缺 cache_write 回退 / 无 cost 跳过三形态，
    /// 换算数值精确断言 + alias 生效 + 跳过计数。
    #[test]
    fn parses_models_dev_fixture_and_maps_units() {
        let mut stats = ParseStats::default();
        let drafts =
            parse_source_with_stats(FIXTURE.as_bytes(), &mut stats).expect("fixture 必须可解析");

        // gpt-image-1（无 cost 且 context=0）跳过：计 missing_pricing
        assert_eq!(drafts.len(), 3, "4 个源模型应产出 3 份草稿: {drafts:?}");
        assert_eq!(stats.skipped_missing_pricing, 1);
        assert_eq!(stats.skipped_missing_context, 0);

        let by_key = |vendor: &str, model: &str| {
            drafts
                .iter()
                .find(|d| d.vendor_id == vendor && d.model_id == model)
                .unwrap_or_else(|| panic!("缺草稿 {vendor}/{model}"))
        };

        // 四档齐全：数值恒等换算（USD/1M → catalog/1M），能力位与窗口直传
        let claude = by_key("anthropic", "claude-sonnet-4-5");
        assert_eq!(claude.input_price, 3.0);
        assert_eq!(claude.output_price, 15.0);
        assert_eq!(claude.cache_read_price, 0.3);
        assert_eq!(claude.cache_creation_price, 3.75);
        assert_eq!(claude.context_window, 200_000);
        assert_eq!(claude.max_output_tokens, Some(64_000));
        assert!(claude.supports_tools);
        assert!(claude.supports_thinking);
        assert_eq!(claude.price_source, PriceSource::Source);

        // 缺 cache_write → 档位回退 0 + Fallback 标记（openai/gpt-4o 实测）
        let gpt4o = by_key("openai", "gpt-4o");
        assert_eq!(gpt4o.input_price, 2.5);
        assert_eq!(gpt4o.output_price, 10.0);
        assert_eq!(gpt4o.cache_read_price, 1.25);
        assert_eq!(
            gpt4o.cache_creation_price, 0.0,
            "缺档回退 catalog 默认 0 价"
        );
        assert_eq!(gpt4o.price_source, PriceSource::Fallback);
        assert!(gpt4o.supports_tools);
        assert!(!gpt4o.supports_thinking);

        // 小数价保真（deepseek-v4-pro 实测 0.022）+ 未命中 alias 保持 slug
        let deepseek = by_key("deepseek", "deepseek-v4-pro");
        assert_eq!(deepseek.input_price, 0.66);
        assert_eq!(deepseek.cache_read_price, 0.022);
        assert_eq!(deepseek.context_window, 1_048_576);

        // alias 生效：命中改写 vendor_id、未命中保持 slug
        let alias = HashMap::from([("anthropic".to_string(), "claude-main".to_string())]);
        let aliased = apply_aliases(drafts, &alias);
        let claude = aliased
            .iter()
            .find(|d| d.vendor_id == "claude-main")
            .expect("alias 必须改写 vendor_id");
        assert_eq!(claude.model_id, "claude-sonnet-4-5");
        assert!(
            aliased.iter().any(|d| d.vendor_id == "deepseek"),
            "未命中 alias 的 slug 保持原样"
        );
    }

    /// 缺 limit.context（或 0）跳过并计数（catalog 登记 context_window
    /// 必须 > 0 的 validate 硬约束在源头挡住）。
    #[test]
    fn parses_skips_models_without_context_window() {
        let raw = br#"{"acme":{"models":{"no-ctx":{"cost":{"input":1,"output":2}},"zero-ctx":{"cost":{"input":1,"output":2},"limit":{"context":0}},"ok":{"cost":{"input":1,"output":2},"limit":{"context":8}}}}}"#;
        let mut stats = ParseStats::default();
        let drafts = parse_source_with_stats(raw, &mut stats).expect("inline 源必须可解析");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].model_id, "ok");
        assert_eq!(stats.skipped_missing_context, 2);
    }

    /// 非法 JSON：返回空草稿（fail-open 由调用方计数上报），不 panic。
    #[test]
    fn parse_source_invalid_json_yields_empty() {
        assert!(parse_source(b"not json").is_empty());
    }

    /// 409 in-flight 断言钉（R-catalog-004 验收）：进程内单飞标记置位时
    /// plan_from_source 必须拒绝重入（admin 映射 409）。
    #[tokio::test]
    async fn plan_from_source_rejects_reentrant_sync_with_in_flight() {
        let section = CatalogSyncSection::default();
        let existing: Vec<ModelCatalogEntry> = Vec::new();
        let metrics = crate::utils::metrics::GlobalMetrics::new();

        // 置位单飞标记（模拟一次同步进行中；守卫析构复位保证不外溢）
        SYNC_IN_FLIGHT.store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome = plan_from_source(&section, &existing, &metrics).await;
        SYNC_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);

        assert!(
            matches!(outcome, Err(SyncError::InFlight)),
            "单飞置位期间必须拒绝重入: {outcome:?}"
        );
        // 复位后可正常进入（fetch 会失败——无上游——但错误形态是 Fetch 非 InFlight）
        let outcome = plan_from_source(&section, &existing, &metrics).await;
        assert!(
            !matches!(outcome, Err(SyncError::InFlight)),
            "复位后不得再报 InFlight: {outcome:?}"
        );
    }

    /// 只增不改合并（R-catalog-003）：既有 (vendor, model) 零改动
    ///（byte 不变）、缺失条目新增、重复草稿去重（先到先得）。
    #[test]
    fn merge_adds_missing_never_touches_existing() {
        let existing = vec![ModelCatalogEntry {
            model_id: "claude-sonnet-4-5".into(),
            context_window: 200_000,
            input_price: 9.99,
            output_price: 19.99,
            cache_read_price: 0.9,
            cache_creation_price: 9.0,
            cache_creation_1h_price: 0.0,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            ..Default::default()
        }];
        let before = existing.clone();
        let drafts = vec![
            // 与既有条目同 model_id（不同 vendor 视角）→ 必须丢弃
            CatalogEntryDraft {
                vendor_id: "anthropic".into(),
                model_id: "claude-sonnet-4-5".into(),
                context_window: 200_000,
                input_price: 3.0,
                output_price: 15.0,
                cache_read_price: 0.3,
                cache_creation_price: 3.75,
                cache_creation_1h_price: 0.0,
                price_source: PriceSource::Source,
                supports_tools: true,
                supports_thinking: true,
                max_output_tokens: Some(64_000),
            },
            // 缺失条目 → 新增
            CatalogEntryDraft {
                vendor_id: "deepseek".into(),
                model_id: "deepseek-v4-pro".into(),
                context_window: 1_048_576,
                input_price: 0.66,
                output_price: 1.98,
                cache_read_price: 0.022,
                cache_creation_price: 0.0,
                cache_creation_1h_price: 0.0,
                price_source: PriceSource::Fallback,
                supports_tools: true,
                supports_thinking: true,
                max_output_tokens: Some(393_216),
            },
            // 与上一条同 model_id 的重复草稿（他源视角）→ 去重，先到先得
            CatalogEntryDraft {
                vendor_id: "openai".into(),
                model_id: "deepseek-v4-pro".into(),
                context_window: 1_000,
                input_price: 7.0,
                output_price: 7.0,
                cache_read_price: 0.0,
                cache_creation_price: 0.0,
                cache_creation_1h_price: 0.0,
                price_source: PriceSource::Source,
                supports_tools: false,
                supports_thinking: false,
                max_output_tokens: None,
            },
        ];

        let plan = merge(&existing, drafts);
        // 只增不改：既有条目逐位不变（含用户自定价 9.99）
        assert_eq!(existing, before, "merge 不得触碰既有目录条目");
        assert_eq!(
            plan.skipped_existing, 1,
            "既有 (vendor, model) 草稿必须丢弃"
        );
        assert_eq!(plan.duplicates_dropped, 1, "重复草稿去重（先到先得）");
        assert_eq!(plan.additions.len(), 1);
        let added = &plan.additions[0];
        assert_eq!(added.vendor_id, "deepseek");
        assert_eq!(added.model_id, "deepseek-v4-pro");
        assert_eq!(added.input_price, 0.66, "新增取先出现的草稿");

        // 空目录：全部草稿入 additions；重复仍去重
        let empty: Vec<ModelCatalogEntry> = Vec::new();
        let drafts = vec![
            CatalogEntryDraft {
                vendor_id: "a".into(),
                model_id: "m1".into(),
                context_window: 1,
                input_price: 1.0,
                output_price: 1.0,
                cache_read_price: 0.0,
                cache_creation_price: 0.0,
                cache_creation_1h_price: 0.0,
                price_source: PriceSource::Source,
                supports_tools: false,
                supports_thinking: false,
                max_output_tokens: None,
            },
            CatalogEntryDraft {
                vendor_id: "b".into(),
                model_id: "m1".into(),
                context_window: 2,
                input_price: 2.0,
                output_price: 2.0,
                cache_read_price: 0.0,
                cache_creation_price: 0.0,
                cache_creation_1h_price: 0.0,
                price_source: PriceSource::Source,
                supports_tools: false,
                supports_thinking: false,
                max_output_tokens: None,
            },
        ];
        let plan = merge(&empty, drafts);
        assert_eq!(plan.additions.len(), 1, "同 model_id 只落一条");
        assert_eq!(plan.additions[0].vendor_id, "a", "先到先得");
        assert_eq!(plan.duplicates_dropped, 1);
    }

    /// 草稿 → 目录条目转换：USD 币种、缓存断点策略 None（用内置默认）、
    /// rerank/长上下文价 0（源无此语义）。
    #[test]
    fn draft_to_entry_maps_catalog_defaults() {
        let draft = CatalogEntryDraft {
            vendor_id: "openai".into(),
            model_id: "gpt-4o".into(),
            context_window: 128_000,
            input_price: 2.5,
            output_price: 10.0,
            cache_read_price: 1.25,
            cache_creation_price: 0.0,
            cache_creation_1h_price: 0.0,
            price_source: PriceSource::Fallback,
            supports_tools: true,
            supports_thinking: false,
            max_output_tokens: Some(16_384),
        };
        let entry = draft.to_entry();
        assert_eq!(entry.model_id, "gpt-4o");
        assert_eq!(entry.currency, PricingCurrency::Usd, "源价为 USD 计价");
        assert_eq!(entry.rerank_price_per_1k_docs, 0.0);
        assert_eq!(entry.long_context_input_price, 0.0);
        assert_eq!(entry.long_context_threshold_tokens, 0);
        assert!(entry.cache_breakpoints.is_none());
        assert_eq!(entry.max_output_tokens, Some(16_384));
        assert!(entry.supports_tools);
        assert!(!entry.supports_thinking);
    }
}
