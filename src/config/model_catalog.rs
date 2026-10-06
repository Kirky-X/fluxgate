// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! per-model 能力/定价目录（N6）——`[[model_catalog]]` 段的类型与
//! 查询纯函数。
//!
//! 字段对齐 agentstem `crates/providers/src/model_catalog.rs:18-39` 的
//! `ModelEntry`（实现不复用旧代码；agentstem 侧静态内置表 + 前缀匹配的
//! 查询形态不迁——本目录由配置面/admin CRUD 显式登记）。
//!
//! **定价单位口径（计价消费的契约）** 四个价格字段均为
//! 「currency 计价，**每 1M tokens**」——对齐 agentstem
//! `model_catalog.rs:178-197 estimate_usage_cost` 的
//! `per_million(tokens, price) = (tokens / 1_000_000) * price` 公式。
//! 缓存档：无独立计价的档位填 0（OpenAI 平台不单独计费 cache write；
//! Anthropic cache read ≈ 0.1×输入价、cache write(5m) ≈ 1.25×输入价）。
//!
//! **落位裁决**：独立 `[[model_catalog]]` 全局段（按 model_id 键）而非
//! 扩展 `[[vendors.models]]` ModelConfig——能力/定价是厂商公布的模型事实，
//! 与路由身份（weight/abstract_id，ModelConfig 既有职责）正交；同一
//! model_id 可被多 vendor/代理同时服务，嵌套会强制复制条目且全局查询
//! 需扫描全部 vendor；admin CRUD 在平铺列表上按 model_id 单键操作。
//! persist_and_swap 全量写回对两种落位无差（同在 Config 内，D7.1）。

use serde::{Deserialize, Serialize};

/// 定价币种 — 对齐 agentstem `core::usage::Currency`（Usd/Cny）。
/// 严格枚举：未知币种串使配置加载失败（显性早暴露，与 api_key 档位同
/// 策略——本值无运行时回退语义）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum PricingCurrency {
    /// 美元计价（默认；四个价格字段共用）
    #[default]
    #[serde(rename = "usd")]
    Usd,
    /// 人民币计价
    #[serde(rename = "cny")]
    Cny,
}

/// 目录条目上限（审查 /3）：防配置爆炸——admin upsert 无上限会拖垮
/// persist_and_swap 全量写回与 GET 列表面；超限 validate_config 拒绝
/// （加载与热更新两路同拒）。
pub const MAX_MODEL_CATALOG_ENTRIES: usize = 1024;

/// per-model 目录条目 — 字段对齐 agentstem ModelEntry（:18-39）。
/// `Default` 仅为测试/占位便利：默认 `context_window = 0` 会被
/// `validate_config` 拒绝（登记条目必须声明真实窗口）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct ModelCatalogEntry {
    /// 模型全名（目录主键；validate_config 强制去重）
    pub model_id: String,
    /// 上下文窗口（tokens）。**登记条目必须 > 0**（validate_config 强制）：
    /// 0 会让 agentstem TUI auto-compact 静默不装配
    /// （crates/tui/src/agent_session.rs:3290-3291 语义）。
    pub context_window: u64,
    /// 输入单价（currency 计价，**每 1M tokens**）
    #[serde(default)]
    pub input_price: f64,
    /// 输出单价（currency 计价，**每 1M tokens**）
    #[serde(default)]
    pub output_price: f64,
    /// 缓存读取单价（cache hit；无独立计价填 0）
    #[serde(default)]
    pub cache_read_price: f64,
    /// 缓存创建单价（cache write；无独立计价填 0）
    #[serde(default)]
    pub cache_creation_price: f64,
    /// 定价币种 — 四个价格字段共用的计价币种
    #[serde(default)]
    pub currency: PricingCurrency,
    /// 最大输出 tokens（None = 厂商未公布/由上游决定）
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// 是否支持工具调用
    #[serde(default)]
    pub supports_tools: bool,
    /// 是否支持思考模式
    #[serde(default)]
    pub supports_thinking: bool,
    /// Anthropic 自动缓存断点注入策略（anthropic 型 vendor 转换出站
    /// 生效）。None = 用内置默认（启用 / 上限 4 / 锚序 tools→system→
    /// last_user，见 [`CacheBreakpointPolicy`]）。与 supports_* 能力位
    /// 正交——能力位描述模型能做什么，缓存断点是请求侧计费/性能工程。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_breakpoints: Option<CacheBreakpointPolicy>,
}

/// Anthropic 自动缓存断点注入策略。
///
/// 断点 = 出站请求内容块上的 `cache_control: {"type":"ephemeral"}` 标记
/// （Anthropic prompt caching；平台单请求断点上限 4）。命中后续请求按
/// 前缀缓存计费（cache read ≈ 0.1×输入价），未命中写缓存按 1.25×计费
/// ——默认开启是成本/性能工程取舍，条目级可关。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CacheBreakpointPolicy {
    /// 注入开关（anthropic 型 vendor 转换出站）
    #[serde(default = "default_cache_breakpoints_enabled")]
    pub enabled: bool,
    /// 断点数上限（候选锚点超出时保留锚序**尾部**、丢弃头部——尾部锚
    /// 更贴近对话末端，增量缓存命中率高）。0 = 不注入。
    #[serde(default = "default_max_cache_breakpoints")]
    pub max_breakpoints: u8,
    /// 锚点与应用顺序（溢出按序丢弃头部元素）：
    /// - `"tools"` — tools 数组最后一个定义块（工具前缀最稳定）；
    /// - `"system"` — system 段（字符串提升为单块数组后标记）；
    /// - `"last_user"` — 最后一条 user 消息的最后一个内容块。
    ///
    /// 未知锚名忽略（显性 debug 日志）。
    #[serde(default = "default_cache_breakpoint_anchors")]
    pub anchors: Vec<String>,
}

fn default_cache_breakpoints_enabled() -> bool {
    true
}

fn default_max_cache_breakpoints() -> u8 {
    4
}

fn default_cache_breakpoint_anchors() -> Vec<String> {
    vec!["tools".into(), "system".into(), "last_user".into()]
}

impl Default for CacheBreakpointPolicy {
    fn default() -> Self {
        Self {
            enabled: default_cache_breakpoints_enabled(),
            max_breakpoints: default_max_cache_breakpoints(),
            anchors: default_cache_breakpoint_anchors(),
        }
    }
}

/// 断点策略解析：目录精确命中且条目带覆盖 → 条目策略；否则内置默认
/// （未登记模型同样注入——缓存断点是请求侧工程，不依赖登记）。
pub fn cache_policy_for(catalog: &[ModelCatalogEntry], model_id: &str) -> CacheBreakpointPolicy {
    catalog
        .iter()
        .find(|e| e.model_id == model_id)
        .and_then(|e| e.cache_breakpoints.clone())
        .unwrap_or_default()
}

/// 能力查询结果 — capabilities() 语义面（无定价；定价消费在 vendor/cost 计价）。
/// 字段名对齐 agentstem 消费方（`max_context_tokens` 为
/// agent_session.rs:3290-3291 auto-compact 判读的字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// 上下文窗口（tokens）——已登记为目录真实值，未登记为 per-flavor 默认；
    /// 恒 > 0（默认表不含 0，登记条目被 validate_config 强制非 0）
    pub max_context_tokens: u64,
    /// 最大输出 tokens
    pub max_output_tokens: Option<u64>,
    /// 是否支持工具调用
    pub supports_tools: bool,
    /// 是否支持思考模式
    pub supports_thinking: bool,
}

/// 未登记模型的 per-flavor 保守默认（默认表）。
///
/// 来源（2026-10 公开文档口径，保守取值）：
/// - **openai**：128k ctx / 16,384 max out / tools 平台面支持
///   （platform.openai.com/docs/models，gpt-4o 家族口径）；
/// - **anthropic**：200k ctx / 8,192 max out / tools 平台面支持
///   （docs.anthropic.com，claude-3.5/4 家族口径）；
/// - **ollama**：4,096 ctx（Ollama 默认 `num_ctx`）/ max out None（服务端
///   决定）/ tools false（本地模型能力异构，保守按不支持——不发 tools
///   不会破坏不支持工具的本地模型）；
/// - 未知 vendor type 一律回落 ollama 档（最小窗口的保守兜底）。
///
/// 价格不进默认表：未登记模型不计价（计价按目录命中与否显式
/// 处理，0 价默认表会伪装成「免费」事实）。
pub fn flavor_default_capabilities(vendor_type: &str) -> ModelCapabilities {
    match vendor_type {
        "openai" => ModelCapabilities {
            max_context_tokens: 128_000,
            max_output_tokens: Some(16_384),
            supports_tools: true,
            supports_thinking: false,
        },
        "anthropic" => ModelCapabilities {
            max_context_tokens: 200_000,
            max_output_tokens: Some(8_192),
            supports_tools: true,
            supports_thinking: false,
        },
        // ollama 与未知 vendor type（保守兜底）
        _ => ModelCapabilities {
            max_context_tokens: 4_096,
            max_output_tokens: None,
            supports_tools: false,
            supports_thinking: false,
        },
    }
}

/// 目录查询：精确 model_id 命中 → 目录真实值；未命中 → per-flavor
/// （vendor type）保守默认。精确匹配（不做前缀匹配——登记面是显式
/// admin CRUD，模糊命中会造成「登记即全局生效」的意外面；前缀计价
/// 匹配属 计价消费的裁决域）。
pub fn capabilities_for(
    catalog: &[ModelCatalogEntry],
    vendor_type: &str,
    model_id: &str,
) -> ModelCapabilities {
    catalog
        .iter()
        .find(|e| e.model_id == model_id)
        .map(|e| ModelCapabilities {
            max_context_tokens: e.context_window,
            max_output_tokens: e.max_output_tokens,
            supports_tools: e.supports_tools,
            supports_thinking: e.supports_thinking,
        })
        .unwrap_or_else(|| flavor_default_capabilities(vendor_type))
}

#[cfg(test)]
mod tests {
    //! 目录单元测试（验收）：serde 面 / 能力查询回落 / 定价契约。

    use super::*;

    /// agentstem builtin claude-sonnet-4-20250514 的同值条目（:59-70）
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

    // ======== serde 面 ========

    #[test]
    fn toml_full_entry_roundtrips() {
        let toml = r#"
model_id = "claude-sonnet-4-20250514"
context_window = 200000
input_price = 3.0
output_price = 15.0
cache_read_price = 0.3
cache_creation_price = 3.75
currency = "usd"
max_output_tokens = 8192
supports_tools = true
supports_thinking = true
"#;
        let entry: ModelCatalogEntry = toml::from_str(toml).unwrap();
        assert_eq!(entry, claude_entry());
        let serialized = toml::to_string(&entry).unwrap();
        for key in [
            "model_id",
            "context_window",
            "input_price",
            "output_price",
            "cache_read_price",
            "cache_creation_price",
            "currency",
            "max_output_tokens",
            "supports_tools",
            "supports_thinking",
        ] {
            assert!(serialized.contains(key), "T007 契约：序列化面含 {key}");
        }
    }

    #[test]
    fn toml_minimal_entry_takes_defaults() {
        let toml = r#"
model_id = "m"
context_window = 128000
"#;
        let entry: ModelCatalogEntry = toml::from_str(toml).unwrap();
        assert_eq!(entry.input_price, 0.0);
        assert_eq!(entry.output_price, 0.0);
        assert_eq!(entry.cache_read_price, 0.0);
        assert_eq!(entry.cache_creation_price, 0.0);
        assert_eq!(entry.currency, PricingCurrency::Usd);
        assert_eq!(entry.max_output_tokens, None);
        assert!(!entry.supports_tools);
        assert!(!entry.supports_thinking);
    }

    #[test]
    fn unknown_currency_fails_config_load() {
        let toml = r#"
model_id = "m"
context_window = 128000
currency = "eur"
"#;
        assert!(
            toml::from_str::<ModelCatalogEntry>(toml).is_err(),
            "未知币种串显性失败（严格枚举，与 api_key 档位同策略）"
        );
        let cny: ModelCatalogEntry =
            toml::from_str("model_id = \"m\"\ncontext_window = 1\ncurrency = \"cny\"").unwrap();
        assert_eq!(cny.currency, PricingCurrency::Cny);
    }

    // ======== 能力查询（真实值 + per-flavor 回落） ========

    #[test]
    fn registered_model_returns_real_nonzero_capabilities() {
        let catalog = vec![claude_entry()];
        let caps = capabilities_for(&catalog, "anthropic", "claude-sonnet-4-20250514");
        assert_eq!(caps.max_context_tokens, 200_000, "登记条目返回真实值");
        assert_ne!(caps.max_context_tokens, 0, "非 0（auto-compact 守卫）");
        assert_eq!(caps.max_output_tokens, Some(8192));
        assert!(caps.supports_tools);
        assert!(caps.supports_thinking);
    }

    #[test]
    fn unregistered_model_falls_back_per_flavor() {
        let catalog: Vec<ModelCatalogEntry> = vec![];
        let openai = capabilities_for(&catalog, "openai", "gpt-9-turbo");
        assert_eq!(openai.max_context_tokens, 128_000);
        assert_eq!(openai.max_output_tokens, Some(16_384));
        assert!(openai.supports_tools);
        let anthropic = capabilities_for(&catalog, "anthropic", "unknown-claude");
        assert_eq!(anthropic.max_context_tokens, 200_000);
        assert_eq!(anthropic.max_output_tokens, Some(8_192));
        let ollama = capabilities_for(&catalog, "ollama", "local-model");
        assert_eq!(ollama.max_context_tokens, 4_096);
        assert_eq!(ollama.max_output_tokens, None);
        assert!(!ollama.supports_tools, "本地模型能力异构，保守不支持");
        let unknown_type = capabilities_for(&catalog, "crystal-ball", "m");
        assert_eq!(
            unknown_type.max_context_tokens, 4_096,
            "未知 vendor type 回落最保守档"
        );
    }

    // ======== 定价契约（计价消费） ========

    /// 单位口径契约：价格字段为「每 1M tokens」——对齐 agentstem
    /// estimate_usage_cost 的 per_million 公式。CostCalculator
    /// 迁移必须复现本式（含缓存档），本测试为回归锚。
    #[test]
    fn pricing_contract_per_1m_tokens_semantics() {
        let entry = claude_entry();
        let per_million = |tokens: u64, price: f64| (tokens as f64 / 1_000_000.0) * price;
        // 与 agentstem model_catalog.rs:331-349 cost_estimate_prices_all_four_tiers
        // 同一组用量（1M/1M/2M/0.5M）与同值条目——数字必须逐位一致
        let input = per_million(1_000_000, entry.input_price);
        let output = per_million(1_000_000, entry.output_price);
        let cache_read = per_million(2_000_000, entry.cache_read_price);
        let cache_creation = per_million(500_000, entry.cache_creation_price);
        assert!((input - 3.0).abs() < 1e-9);
        assert!((output - 15.0).abs() < 1e-9);
        assert!((cache_read - 0.6).abs() < 1e-9);
        assert!((cache_creation - 1.875).abs() < 1e-9);
        let total = input + output + cache_read + cache_creation;
        assert!((total - (3.0 + 15.0 + 0.6 + 1.875)).abs() < 1e-9);
    }

    /// 无独立计价档填 0：OpenAI 形态条目的 cache_creation 不产生成本。
    #[test]
    fn zero_price_tier_yields_zero_cost() {
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
        let per_million = |tokens: u64, price: f64| (tokens as f64 / 1_000_000.0) * price;
        let cache_creation = per_million(100_000, gpt4o.cache_creation_price);
        assert!(
            cache_creation.abs() < 1e-9,
            "0 价档不产生成本（非虚构计价）"
        );
    }

    // ======== 断点策略 ========

    #[test]
    fn cache_policy_entry_override_and_default_fallback() {
        let mut catalog = vec![claude_entry()];
        // 未覆盖条目 → 内置默认
        let default_policy = super::cache_policy_for(&catalog, "claude-sonnet-4-20250514");
        assert!(default_policy.enabled);
        assert_eq!(default_policy.max_breakpoints, 4);
        assert_eq!(default_policy.anchors, vec!["tools", "system", "last_user"]);

        // 条目覆盖（capability 位正交：supports_thinking=false 仍可注入）
        catalog[0].supports_thinking = false;
        catalog[0].cache_breakpoints = Some(CacheBreakpointPolicy {
            enabled: true,
            max_breakpoints: 2,
            anchors: vec!["system".into(), "last_user".into()],
        });
        let policy = super::cache_policy_for(&catalog, "claude-sonnet-4-20250514");
        assert_eq!(policy.max_breakpoints, 2, "条目覆盖生效");
        assert!(!catalog[0].supports_thinking, "能力位与注入策略正交");

        // 未登记模型 → 内置默认（注入不依赖登记）
        assert_eq!(
            super::cache_policy_for(&catalog, "unregistered").max_breakpoints,
            4
        );
    }

    #[test]
    fn cache_policy_toml_sub_table_roundtrip() {
        let toml = r#"
model_id = "claude-sonnet-4-20250514"
context_window = 200000

[cache_breakpoints]
enabled = false
max_breakpoints = 2
anchors = ["system", "last_user"]
"#;
        let entry: ModelCatalogEntry = toml::from_str(toml).unwrap();
        let policy = entry.cache_breakpoints.expect("sub-table parsed");
        assert!(!policy.enabled);
        assert_eq!(policy.max_breakpoints, 2);
        assert_eq!(policy.anchors, vec!["system", "last_user"]);
        // None 时不序列化（skip_serializing_if）
        let minimal: ModelCatalogEntry =
            toml::from_str("model_id = \"m\"\ncontext_window = 1").unwrap();
        assert!(minimal.cache_breakpoints.is_none());
        assert!(!toml::to_string(&minimal)
            .unwrap()
            .contains("cache_breakpoints"));
    }

    /// CNY 条目：币种随条目（按 currency 分道 cost_usd/cost_cny 的输入）。
    #[test]
    fn cny_entry_carries_currency() {
        let entry: ModelCatalogEntry = toml::from_str(
            r#"
model_id = "deepseek-chat"
context_window = 64000
input_price = 1.0
output_price = 2.0
currency = "cny"
"#,
        )
        .unwrap();
        assert_eq!(entry.currency, PricingCurrency::Cny);
        let caps = capabilities_for(std::slice::from_ref(&entry), "openai", "deepseek-chat");
        assert_eq!(caps.max_context_tokens, 64_000, "能力查询与币种正交");
    }
}
