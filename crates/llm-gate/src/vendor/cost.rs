// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! CostCalculator — 基于 (vendor_id, model_id) 维度的定价表查找。
//!
//! 设计：
//! - 内部用 `Arc<RwLock<HashMap<(String, String), PricingEntry>>>` 存储
//! - `from_config` 从 `&[PricingConfig]` 初始化表
//! - `calculate` 按 `(prompt/1000)*in_price + (completion/1000)*out_price` 计算 USD
//! - 未命中返回 0.0（避免阻塞响应）
//! - `update_entry` 运行时动态更新

use crate::config::PricingConfig;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// 单条定价 — 输入/输出 token 单价（每 1k tokens，USD）
#[derive(Debug, Clone, Copy)]
pub struct PricingEntry {
    pub input_per_1k_tokens: f64,
    pub output_per_1k_tokens: f64,
}

impl From<&PricingConfig> for PricingEntry {
    fn from(p: &PricingConfig) -> Self {
        Self {
            input_per_1k_tokens: p.input_per_1k_tokens,
            output_per_1k_tokens: p.output_per_1k_tokens,
        }
    }
}

/// 成本计算器 — 线程安全，支持运行时动态更新定价表
pub struct CostCalculator {
    table: Arc<RwLock<HashMap<(String, String), PricingEntry>>>,
}

impl CostCalculator {
    /// 从 PricingConfig 列表构建定价表
    pub fn from_config(pricing: &[PricingConfig]) -> Self {
        let mut map = HashMap::with_capacity(pricing.len());
        for p in pricing {
            map.insert(
                (p.vendor_id.clone(), p.model_id.clone()),
                PricingEntry::from(p),
            );
        }
        Self {
            table: Arc::new(RwLock::new(map)),
        }
    }

    /// 计算 USD 成本：`(prompt/1000)*in_price + (completion/1000)*out_price`
    /// 未命中 vendor/model 返回 0.0
    pub fn calculate(&self, vendor: &str, model: &str, prompt: u32, completion: u32) -> f64 {
        let guard = match self.table.read() {
            Ok(g) => g,
            Err(_) => {
                // RwLock poisoned — 不应阻塞响应，返回 0.0
                // 上层 tracing::warn! 已记录
                return 0.0;
            }
        };
        let entry = match guard.get(&(vendor.to_string(), model.to_string())) {
            Some(e) => *e,
            None => return 0.0,
        };
        let input_cost = (prompt as f64 / 1000.0) * entry.input_per_1k_tokens;
        let output_cost = (completion as f64 / 1000.0) * entry.output_per_1k_tokens;
        input_cost + output_cost
    }

    /// 动态更新单条定价 entry — 立即生效
    pub fn update_entry(&self, vendor: &str, model: &str, entry: PricingEntry) {
        if let Ok(mut guard) = self.table.write() {
            guard.insert((vendor.to_string(), model.to_string()), entry);
        }
        // poisoned lock 静默忽略 — 上层应自行监控
    }
}

impl Default for CostCalculator {
    fn default() -> Self {
        Self {
            table: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

// ── T007：model_catalog 四档计价（cache token 进计价） ───────────────
//
// D7.11 双表裁决：[[model_catalog]] 承载能力 + 四档单价（本节消费）；
// [[pricing]] 留 SLA 因子（sla_v 惩罚项）与旧 [`CostCalculator`]，至
// T011 桥接切换后退役——本节不读 [[pricing]]。

use crate::config::model_catalog::ModelCatalogEntry;
use crate::protocol::TokenUsage;

/// prompt_tokens 的 cache 档包含语义（IR 随上游协议分裂，T003 线格式）：
/// - **Inclusive**（OpenAI 口径）：`prompt_tokens` 含 cached 部分——非缓存
///   基数 = prompt − cache_read − cache_creation（饱和减法）；
/// - **Exclusive**（Anthropic 口径）：`input_tokens` 不含 cache 档
///   （cache_read/creation_input_tokens 为独立字段）——非缓存基数 =
///   prompt 原值。
///
/// 调用方按 vendor `type_` 选择（openai → Inclusive、anthropic →
/// Exclusive）；任务书公式 `(prompt−cache_read−cache_creation)×input`
/// 即 Inclusive 臂。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCacheSemantics {
    /// prompt 含 cache 档（OpenAI 口径）：基数 = prompt − cache 档（饱和）
    Inclusive,
    /// prompt 不含 cache 档（Anthropic 口径）：基数 = prompt
    Exclusive,
}

/// 四档成本估算结果 — 币种随条目。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CatalogCostEstimate {
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub cache_creation_cost: f64,
    /// 四档之和
    pub total_cost: f64,
    pub currency: crate::config::PricingCurrency,
}

/// 定价查找：精确命中 → 前缀回退（命中多个取字典序最小，确定性）。
/// 对齐 agentstem `estimate_usage_cost` 的 `lookup_pricing` 短名语义
///（`claude-sonnet-4` → `claude-sonnet-4-20250514`）。**与
/// `capabilities_for`（纯精确）的非对称为 D7.11 有意裁决**：能力位取
/// 「登记即真实」不容模糊；计价容忍用户短名习惯。
pub fn lookup_pricing_entry<'a>(
    catalog: &'a [ModelCatalogEntry],
    model_id: &str,
) -> Option<&'a ModelCatalogEntry> {
    if let Some(entry) = catalog.iter().find(|e| e.model_id == model_id) {
        return Some(entry);
    }
    catalog
        .iter()
        .filter(|e| e.model_id.starts_with(model_id))
        .min_by(|a, b| a.model_id.cmp(&b.model_id))
}

/// 四档成本估算（T007）——未登记模型返回 `None`（显式缺失，调用方不得
/// 按 0 成本处理，防 0 价伪装免费；T006 语义延续）。
///
/// 公式（per 1M tokens，币种随条目；semantics 决定非缓存基数的折算）：
/// `cost = base×input_price/1M + cache_read×cache_read_price/1M
///       + cache_creation×cache_creation_price/1M + completion×output_price/1M`
pub fn estimate_cost_from_catalog(
    catalog: &[ModelCatalogEntry],
    model_id: &str,
    usage: &TokenUsage,
    semantics: PromptCacheSemantics,
) -> Option<CatalogCostEstimate> {
    let entry = lookup_pricing_entry(catalog, model_id)?;
    let per_million = |tokens: u64, price: f64| (tokens as f64 / 1_000_000.0) * price;
    let base = match semantics {
        PromptCacheSemantics::Exclusive => usage.prompt_tokens,
        PromptCacheSemantics::Inclusive => {
            // cache 档 u64 与 prompt u32 宽度差：先饱和钳到 u32 域再饱和减
            let cache = usage
                .cache_read_tokens
                .saturating_add(usage.cache_creation_tokens)
                .min(u32::MAX as u64) as u32;
            usage.prompt_tokens.saturating_sub(cache)
        }
    };
    let input_cost = per_million(base as u64, entry.input_price);
    let output_cost = per_million(usage.completion_tokens as u64, entry.output_price);
    let cache_read_cost = per_million(usage.cache_read_tokens, entry.cache_read_price);
    let cache_creation_cost = per_million(usage.cache_creation_tokens, entry.cache_creation_price);
    Some(CatalogCostEstimate {
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
        total_cost: input_cost + output_cost + cache_read_cost + cache_creation_cost,
        currency: entry.currency,
    })
}

#[cfg(test)]
mod catalog_pricing_tests {
    //! T007 验收：cache 档计价 / 短名前缀命中 / 未登记 None / 非对称锚。

    use super::*;
    use crate::config::model_catalog::ModelCatalogEntry;
    use crate::config::PricingCurrency;

    fn claude_entry() -> ModelCatalogEntry {
        ModelCatalogEntry {
            model_id: "claude-sonnet-4-20250514".into(),
            context_window: 200_000,
            input_price: 3.0,
            output_price: 15.0,
            cache_read_price: 0.3,
            cache_creation_price: 3.75,
            currency: PricingCurrency::Usd,
            max_output_tokens: Some(8192),
            supports_tools: true,
            supports_thinking: true,
            cache_breakpoints: None,
        }
    }

    fn usage(prompt: u32, completion: u32, cache_read: u64, cache_creation: u64) -> TokenUsage {
        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            cost_usd: None,
            cost_cny: None,
        }
    }

    /// 未登记模型 None（显式缺失，防 0 价伪装免费——T006 语义延续）
    #[test]
    fn unregistered_model_returns_none() {
        let catalog = vec![claude_entry()];
        assert!(estimate_cost_from_catalog(
            &catalog,
            "unknown-model",
            &usage(1000, 100, 0, 0),
            PromptCacheSemantics::Exclusive
        )
        .is_none());
        assert!(lookup_pricing_entry(&[], "anything").is_none());
    }

    /// cache 档生效（Exclusive = Anthropic 口径：基数不含 cache 档）
    #[test]
    fn cache_tiers_priced_exclusive() {
        let catalog = vec![claude_entry()];
        // 1M 非缓存输入 + 1M 输出 + 2M cache read + 0.5M cache write
        let est = estimate_cost_from_catalog(
            &catalog,
            "claude-sonnet-4-20250514",
            &usage(1_000_000, 1_000_000, 2_000_000, 500_000),
            PromptCacheSemantics::Exclusive,
        )
        .unwrap();
        assert!((est.input_cost - 3.0).abs() < 1e-9);
        assert!((est.output_cost - 15.0).abs() < 1e-9);
        assert!((est.cache_read_cost - 0.6).abs() < 1e-9);
        assert!((est.cache_creation_cost - 1.875).abs() < 1e-9);
        assert!((est.total_cost - 20.475).abs() < 1e-9);
        assert_eq!(est.currency, PricingCurrency::Usd);
    }

    /// Inclusive 臂 = 任务书公式：基数 = prompt − cache 档（饱和减法）
    #[test]
    fn cache_tiers_priced_inclusive() {
        let catalog = vec![claude_entry()];
        // prompt 2M 含 cache read 2M → 非缓存基数 0
        let est = estimate_cost_from_catalog(
            &catalog,
            "claude-sonnet-4-20250514",
            &usage(2_000_000, 500_000, 2_000_000, 0),
            PromptCacheSemantics::Inclusive,
        )
        .unwrap();
        assert!((est.input_cost - 0.0).abs() < 1e-9, "全命中输入基数为 0");
        assert!((est.output_cost - 7.5).abs() < 1e-9);
        assert!((est.cache_read_cost - 0.6).abs() < 1e-9);
        assert!((est.total_cost - 8.1).abs() < 1e-9);
        // 饱和减法：cache 档超 prompt 不下溢
        let est = estimate_cost_from_catalog(
            &catalog,
            "claude-sonnet-4-20250514",
            &usage(100, 10, 2_000_000, 0),
            PromptCacheSemantics::Inclusive,
        )
        .unwrap();
        assert!((est.input_cost - 0.0).abs() < 1e-9);
    }

    /// 短名前缀命中（estimate_usage_cost 语义）
    #[test]
    fn prefix_lookup_hits_full_name() {
        let catalog = vec![claude_entry()];
        let entry = lookup_pricing_entry(&catalog, "claude-sonnet-4").unwrap();
        assert_eq!(entry.model_id, "claude-sonnet-4-20250514");
        let est = estimate_cost_from_catalog(
            &catalog,
            "claude-sonnet-4",
            &usage(1_000_000, 0, 0, 0),
            PromptCacheSemantics::Exclusive,
        )
        .unwrap();
        assert!((est.input_cost - 3.0).abs() < 1e-9);
    }

    /// 前缀歧义取字典序最小（确定性）
    #[test]
    fn ambiguous_prefix_takes_lexicographic_min() {
        let short = ModelCatalogEntry {
            model_id: "gpt-x-2".into(),
            context_window: 128_000,
            input_price: 9.0,
            ..Default::default()
        };
        let long = ModelCatalogEntry {
            model_id: "gpt-x-10".into(),
            context_window: 128_000,
            input_price: 1.0,
            ..Default::default()
        };
        let catalog = vec![short, long];
        // 精确命中优先于前缀
        assert_eq!(
            lookup_pricing_entry(&catalog, "gpt-x-10").unwrap().model_id,
            "gpt-x-10"
        );
        // 前缀 "gpt-x-1" 命中 gpt-x-10 与 gpt-x-2? "gpt-x-2" 不以 "gpt-x-1" 开头 → 唯一
        let entry = lookup_pricing_entry(&catalog, "gpt-x-1").unwrap();
        assert_eq!(entry.model_id, "gpt-x-10");
        // 真歧义：gpt-x vs {gpt-x-2, gpt-x-10} → 字典序 gpt-x-10 < gpt-x-2
        let entry = lookup_pricing_entry(&catalog, "gpt-x").unwrap();
        assert_eq!(entry.model_id, "gpt-x-10");
    }

    /// D7.11 非对称锚契约：同一短名——capabilities 精确不命中走 per-flavor
    /// 默认，pricing 前缀命中登记价。两查找语义的有意分叉由此测试钉住。
    #[test]
    fn asymmetry_anchor_capabilities_exact_vs_pricing_prefix() {
        use crate::config::capabilities_for;
        // capabilities：短名精确不命中 → anthropic 默认 200k（恰好同值，
        // 用条目特异窗口 250_000 保证断言区分度）
        let mut entry = claude_entry();
        entry.context_window = 250_000;
        entry.model_id = "claude-sonnet-4-20250514".into();
        let catalog = vec![entry];
        let caps = capabilities_for(&catalog, "anthropic", "claude-sonnet-4");
        assert_eq!(
            caps.max_context_tokens, 200_000,
            "capabilities 精确不命中 → per-flavor 默认（非前缀）"
        );
        // pricing：同一短名前缀命中登记价
        let est = estimate_cost_from_catalog(
            &catalog,
            "claude-sonnet-4",
            &usage(1_000_000, 0, 0, 0),
            PromptCacheSemantics::Exclusive,
        )
        .unwrap();
        assert!(
            (est.input_cost - 3.0).abs() < 1e-9,
            "pricing 前缀命中登记价"
        );
    }

    /// 零价缓存档（OpenAI 形态）不产生 cache write 成本
    #[test]
    fn zero_price_cache_tier_yields_zero() {
        let gpt4o = ModelCatalogEntry {
            model_id: "gpt-4o".into(),
            context_window: 128_000,
            input_price: 2.5,
            output_price: 10.0,
            cache_read_price: 1.25,
            cache_creation_price: 0.0,
            currency: PricingCurrency::Usd,
            max_output_tokens: Some(16_384),
            supports_tools: true,
            supports_thinking: false,
            cache_breakpoints: None,
        };
        let catalog = vec![gpt4o];
        let est = estimate_cost_from_catalog(
            &catalog,
            "gpt-4o",
            &usage(1_000_000, 500_000, 2_000_000, 100_000),
            PromptCacheSemantics::Inclusive,
        )
        .unwrap();
        // Inclusive：基数 = 1M − 2M(saturating) = 0 → input 0；read 2M×1.25=2.5
        assert!((est.input_cost - 0.0).abs() < 1e-9);
        assert!((est.cache_read_cost - 2.5).abs() < 1e-9);
        assert!((est.cache_creation_cost - 0.0).abs() < 1e-9);
        assert!((est.output_cost - 5.0).abs() < 1e-9);
    }
}
