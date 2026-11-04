// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use super::{
    ApiStyle, Config, ConfigManager, DifficultySection, ModelTask, VendorProtocol,
    MAX_DIFFICULTY_LENGTH_CAP, MAX_DIFFICULTY_LEXICON_ENTRIES, MAX_DIFFICULTY_LEXICON_ENTRY_CHARS,
};
use crate::error::GateError;
use crate::i18n::{t, t_args};
use confers::interface::ConfigProvider;
use confers::security::rules::{JwtSecretValidator, SecurityValidator, ViolationSeverity};
use confers::types::{AnnotatedValue, ConfigValue, SourceId};
use std::collections::HashMap;
use std::path::Path;

const DEFAULT_CONFIG_PATH: &str = "config.toml";

pub struct ConfigLoader {
    path: String,
}

impl ConfigLoader {
    pub fn new(path: Option<String>) -> Self {
        Self {
            path: path.unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string()),
        }
    }

    pub fn load(&self) -> Result<ConfigManager, GateError> {
        let path = Path::new(&self.path);

        if !path.exists() {
            return Err(GateError::Config {
                message: t_args("config-file-not-found", &[("path", self.path.clone())]),
            });
        }

        let config = load_config_from_path(path)?;

        let manager = ConfigManager::new(config);
        // 路径记忆：update_config 写回同一路径，消灭硬编码 CWD "config.toml"
        manager.set_path(path.to_path_buf());
        Ok(manager)
    }
}

/// 从路径解析并校验配置（加载路径与热重载共用；热重载不触发落盘）
pub fn load_config_from_path(path: &Path) -> Result<Config, GateError> {
    // 路径来自操作者显式 CLI 参数或热重载监听的自有配置文件（可信输入），
    // 放行绝对路径；symlink 检查与 max_size 防护保留默认开启
    let loader_config = confers::loader::LoaderConfig::new().allow_absolute();
    let annotated =
        confers::loader::load_file(path, &loader_config).map_err(|e| GateError::Config {
            message: t_args("config-load-failed", &[("error", e.to_string())]),
        })?;
    // 错误有意脱敏：只报字段路径不回显出错值（配置值可能是密钥原文）
    let config: Config = annotated.to_typed().map_err(|e| GateError::Config {
        message: t_args(
            "config-parse-failed",
            &[("error", locate_data_error(&e.to_string(), path))],
        ),
    })?;

    validate_config(&config)?;

    Ok(config)
}

/// 从内容串解析并校验配置（格式按路径扩展名检测，与 `load_config_from_path`
/// 的识别口径一致；与加载路径同一 confers 解析管线 + 共享校验，仅免去文件
/// 读取与错误定位增强）。热重载基线采纳的等价性复核专用：调用方必须用
/// 「被本函数解析的同一份字节」计算指纹，指纹与解析验证同源（字节绑定语义）。
pub fn parse_config_content(path: &Path, content: &str) -> Result<Config, GateError> {
    let format =
        confers::loader::detect_format_from_path(path).ok_or_else(|| GateError::Config {
            message: t_args(
                "config-parse-failed",
                &[(
                    "error",
                    format!("unknown extension: {:?}", path.extension()),
                )],
            ),
        })?;
    let annotated = confers::loader::parse_content(
        content,
        format,
        SourceId::new("config-content"),
        Some(path),
    )
    .map_err(|e| GateError::Config {
        message: t_args("config-parse-failed", &[("error", e.to_string())]),
    })?;
    let config: Config = annotated.to_typed().map_err(|e| GateError::Config {
        message: t_args("config-parse-failed", &[("error", e.to_string())]),
    })?;

    validate_config(&config)?;

    Ok(config)
}

/// confers data error 的定位增强：条目缺必填字段时原生消息只到容器坐标
/// （`data error at 'vendors[2]'`）、字段名为空，排障需人工比对结构体。
/// 此时回落一次纯 serde TOML 解析，补出「字段名 + 位置」。只透出 missing
/// field 类信息，其余错误保持原生脱敏口径——不回显配置值，值可能是密钥原文。
fn locate_data_error(original: &str, path: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return original.to_string();
    };
    let Err(error) = toml::from_str::<Config>(&raw) else {
        return original.to_string();
    };
    let message = error.message();
    if !message.contains("missing field") {
        return original.to_string();
    }
    match error.span().and_then(|span| line_column(&raw, span.start)) {
        Some((line, column)) => format!("{message} at {}:{line}:{column}", path.display()),
        None => message.to_string(),
    }
}

/// 字节偏移 → 1 起的（行, 列）；偏移不在字符边界时返回 None
fn line_column(raw: &str, offset: usize) -> Option<(usize, usize)> {
    let before = raw.get(..offset.min(raw.len()))?;
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|index| before.len() - index)
        .unwrap_or(before.len() + 1);
    Some((line, column))
}

/// `[web_search]` 段 serde 缺省单源（enabled=false、providers 空——
/// 两端点 404 的安全默认）。Default 实现落 loader（规则 14）。
impl Default for super::WebSearchSection {
    fn default() -> Self {
        Self {
            enabled: false,
            providers: Vec::new(),
        }
    }
}

/// `[masking]` 段 serde 缺省单源（enabled=false 零行为、kinds/actions 空 =
/// 全启用 + 缺省 mask）。Default 实现落 loader（规则 14）。
impl Default for super::MaskingSection {
    fn default() -> Self {
        Self {
            enabled: false,
            kinds: Vec::new(),
            actions: std::collections::HashMap::new(),
            words: Vec::new(),
            words_any_case: false,
            rules: Vec::new(),
            allow: Vec::new(),
            ledger: Default::default(),
        }
    }
}

/// `[masking.ledger]` 段 serde 缺省单源（enabled=false、保留 30 天、总量
/// 100MiB）。Default 实现落 loader（规则 14）。
impl Default for super::MaskingLedgerSection {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: default_masking_ledger_retention_days(),
            max_total_bytes: default_masking_ledger_max_total_bytes(),
        }
    }
}

/// `[masking.ledger]` retention_days 缺省 30 天。
pub(crate) fn default_masking_ledger_retention_days() -> u32 {
    30
}

/// `[masking.ledger]` max_total_bytes 缺省 100MiB。
pub(crate) fn default_masking_ledger_max_total_bytes() -> u64 {
    100 * 1024 * 1024
}

/// 校验配置 — 加载路径与更新路径共用；管理面弱密钥委托 confers
/// security-rules，跨 Vec 条目的唯一性约束保留手写（字段级规则无法表达）
pub fn validate_config(config: &Config) -> Result<(), GateError> {
    validate_admin_secrets(config)?;
    validate_vendor_entries(config)?;
    validate_model_types(config)?;
    validate_responses(config)?;
    validate_rerank_score_normalization(config)?;
    validate_embedding_groups(config)?;
    warn_missing_api_key_envs(config);
    validate_model_catalog(config)?;
    validate_resilience_circuit(config)?;
    validate_quota_window(config)?;
    warn_quota_without_auth(config);
    validate_quota_windows(config)?;
    validate_vendor_budgets(config)?;
    validate_key_budgets(config)?;
    validate_oidc_rules(config)?;
    validate_quota_factor(config)?;
    warn_quota_window_fields_conflict(config);
    validate_difficulty(config)?;
    warn_identical_difficulty_tiers(config);
    validate_metrics(config)?;
    validate_otlp(config)?;
    validate_affinity(config)?;
    validate_catalog_sync(config)?;
    validate_subscription(config)?;
    validate_masking(config)?;
    validate_web_search(config)?;
    Ok(())
}

/// `[web_search]` 段校验：enabled=false 完全旁路不校验（与 difficulty 缺段
/// 同语义）。enabled=true 时：kind 闭集（点名原值拒绝）、searxng 必须显式
/// base_url、需 key 的源（tavily/brave/exa）至少声明一个 key 来源（vault 引用
/// 优先于 env）。加载与热更共用本入口。
fn validate_web_search(config: &Config) -> Result<(), GateError> {
    let section = &config.web_search;
    if !section.enabled {
        return Ok(());
    }
    for provider in &section.providers {
        match provider.kind.as_str() {
            "tavily" | "brave" | "exa" => {
                if provider.api_key_env.trim().is_empty()
                    && provider.api_key_envs.iter().all(|n| n.trim().is_empty())
                    && provider.api_key_vault_ref.trim().is_empty()
                {
                    return Err(GateError::Config {
                        message: t_args(
                            "config-web-search-missing-key",
                            &[("kind", provider.kind.clone())],
                        ),
                    });
                }
            }
            "searxng" => {
                if provider.base_url.trim().is_empty() {
                    return Err(GateError::Config {
                        message: t("config-web-search-missing-base-url"),
                    });
                }
            }
            other => {
                return Err(GateError::Config {
                    message: t_args(
                        "config-web-search-unknown-kind",
                        &[("kind", other.to_string())],
                    ),
                });
            }
        }
    }
    Ok(())
}

/// `[masking]` 段校验：kinds/actions 闭集、actions 动作词闭集、rules 正则
/// 可编译（`masking` feature 启用时经检测器构造面刚性拒绝；feature 关闭时
/// 跳过编译校验——非法正则由门构造期 fail-closed 兜底）。kind 闭集在此本地
/// 声明（config 面不依赖 `masking` feature），与 `crate::masking` 的
/// `MaskKind::parse` 由单测对齐防漂移。加载与热更共用本入口。
fn validate_masking(config: &Config) -> Result<(), GateError> {
    const MASKING_KINDS: &[&str] = &[
        "api_key",
        "private_key",
        "token",
        "email",
        "card",
        "id",
        "custom",
    ];
    let m = &config.masking;
    for raw in m.kinds.iter().chain(m.actions.keys()) {
        if !MASKING_KINDS.contains(&raw.as_str()) {
            return Err(GateError::Config {
                message: t_args("config-masking-kind-invalid", &[("kind", raw.clone())]),
            });
        }
    }
    for value in m.actions.values() {
        if !matches!(value.as_str(), "mask" | "warn" | "block") {
            return Err(GateError::Config {
                message: t_args(
                    "config-masking-action-invalid",
                    &[("action", value.clone())],
                ),
            });
        }
    }
    #[cfg(feature = "masking")]
    {
        crate::masking::RuleDetector::validate_rules(&m.rules).map_err(|e| GateError::Config {
            message: t_args("config-masking-rule-invalid", &[("detail", e.to_string())]),
        })?;
    }
    if m.ledger.retention_days == 0 || m.ledger.retention_days > 3650 {
        return Err(GateError::Config {
            message: t_args(
                "config-masking-ledger-retention-invalid",
                &[("value", m.ledger.retention_days.to_string())],
            ),
        });
    }
    if m.ledger.max_total_bytes < 4096 {
        return Err(GateError::Config {
            message: t_args(
                "config-masking-ledger-max-bytes-invalid",
                &[("value", m.ledger.max_total_bytes.to_string())],
            ),
        });
    }
    Ok(())
}

/// `[routing.difficulty]` 段校验（V1-V7）：enabled=false 完全旁路不校验
///（与「缺段」同语义）；enabled=true 时全部规则刚性执行。加载与热更
/// 共用本入口（manager 更新路径同样走 validate_config），热更失败保持旧配置
fn validate_difficulty(config: &Config) -> Result<(), GateError> {
    use super::{DifficultySection, Tier};

    let d: &DifficultySection = &config.routing.difficulty;
    if !d.enabled {
        return Ok(());
    }

    // V1: 权重不可同时为 0（融合退化为常数，无意义）
    if d.w_clf == 0.0 && d.w_rule == 0.0 {
        return Err(GateError::Config {
            message: t("config-difficulty-weights-all-zero"),
        });
    }
    // V2: 阈值顺序
    if d.score_low >= d.score_high {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-threshold-order",
                &[
                    ("low", d.score_low.to_string()),
                    ("high", d.score_high.to_string()),
                ],
            ),
        });
    }
    // V3: 中带策略枚举
    if !matches!(
        d.mid_band_policy.to_ascii_lowercase().as_str(),
        "strong" | "cheap" | "nearest"
    ) {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-policy-invalid",
                &[("policy", d.mid_band_policy.clone())],
            ),
        });
    }
    // V4: w_clf>0 需要编译期 feature（配置承诺了实现必须存在）
    #[cfg(not(feature = "difficulty-classifier"))]
    if d.w_clf > 0.0 {
        return Err(GateError::Config {
            message: t("config-difficulty-clf-feature-missing"),
        });
    }
    // V9: 数值域（NaN 绕过 ==/>= 比较、负权重、阈值越界——审查 MEDIUM 收口）
    for (name, value) in [("w_clf", d.w_clf), ("w_rule", d.w_rule)] {
        if !value.is_finite() || value < 0.0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-field-invalid",
                    &[("field", name.to_string()), ("value", value.to_string())],
                ),
            });
        }
    }
    for (name, value) in [("score_low", d.score_low), ("score_high", d.score_high)] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-field-invalid",
                    &[("field", name.to_string()), ("value", value.to_string())],
                ),
            });
        }
    }
    // V10: classifier 子段界（timeout_ms=0 → .max(1) 恒超时空转；超大值实质
    // 取消超时兜底；engine 白名单防未知值静默回退）
    if !(1..=60_000).contains(&d.classifier.timeout_ms) {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-field-invalid",
                &[
                    ("field", "classifier.timeout_ms".to_string()),
                    ("value", d.classifier.timeout_ms.to_string()),
                ],
            ),
        });
    }
    if d.classifier.failure_threshold == 0 {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-field-invalid",
                &[
                    ("field", "classifier.failure_threshold".to_string()),
                    ("value", "0".to_string()),
                ],
            ),
        });
    }
    if !matches!(
        d.classifier.engine.trim().to_ascii_lowercase().as_str(),
        "candle" | "onnx"
    ) {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-field-invalid",
                &[
                    ("field", "classifier.engine".to_string()),
                    ("value", d.classifier.engine.clone()),
                ],
            ),
        });
    }

    // V11/V12: 词法表与规则计分数值域
    validate_difficulty_lexicon(d)?;
    validate_difficulty_scoring(d)?;

    // 真实 model_id 集（CI 折叠）——档位/分组只能指向抽象名，撞名即配置歧义
    let model_ids: std::collections::HashSet<String> = config
        .vendors
        .iter()
        .flat_map(|v| v.models.iter().map(|m| m.model_id.to_ascii_lowercase()))
        .collect();

    let tiers = [(Tier::Low, &d.tiers.low), (Tier::High, &d.tiers.high)];
    for (tier, name) in tiers {
        validate_tier_name(config, tier, name, &model_ids)?;
    }
    if let Some(mid) = &d.tiers.mid {
        validate_tier_name(config, Tier::Mid, mid, &model_ids)?;
    }

    for group in &d.groups {
        let normalized = group.trim().to_ascii_lowercase();
        if model_ids.contains(&normalized) {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-group-collision",
                    &[("name", group.clone())],
                ),
            });
        }
    }
    Ok(())
}

/// V11：词法表条目校验（五个列表共用一套规则）——条目数上限、非空、
/// 无首尾空白/控制字符、单条字符上限、无重复项。`None` = 内置默认，不校验。
fn validate_difficulty_lexicon(d: &DifficultySection) -> Result<(), GateError> {
    for (field, entries) in [
        ("lexicon.hard", &d.lexicon.hard),
        ("lexicon.easy", &d.lexicon.easy),
        ("lexicon.math_symbols", &d.lexicon.math_symbols),
        ("lexicon.question_marks", &d.lexicon.question_marks),
        ("lexicon.code_fence_markers", &d.lexicon.code_fence_markers),
    ] {
        let Some(entries) = entries else { continue };
        if entries.len() > MAX_DIFFICULTY_LEXICON_ENTRIES {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-lexicon-too-many",
                    &[
                        ("field", field.to_string()),
                        ("count", entries.len().to_string()),
                        ("max", MAX_DIFFICULTY_LEXICON_ENTRIES.to_string()),
                    ],
                ),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let invalid = entry.is_empty()
                || entry.trim() != entry
                || entry.chars().any(char::is_control)
                || entry.chars().count() > MAX_DIFFICULTY_LEXICON_ENTRY_CHARS;
            if invalid {
                return Err(GateError::Config {
                    message: t_args(
                        "config-difficulty-lexicon-entry-invalid",
                        &[
                            ("field", field.to_string()),
                            ("index", (index + 1).to_string()),
                            ("max", MAX_DIFFICULTY_LEXICON_ENTRY_CHARS.to_string()),
                        ],
                    ),
                });
            }
            if !seen.insert(entry.as_str()) {
                return Err(GateError::Config {
                    message: t_args(
                        "config-difficulty-lexicon-duplicate",
                        &[("field", field.to_string()), ("entry", entry.clone())],
                    ),
                });
            }
        }
    }
    Ok(())
}

/// V12：规则计分数值域——权重有限且 ∈ [0,1]，封顶有限且 > 0，
/// `length_cap` ∈ [1, 8000]（估算器封顶 9000 必须覆盖该值）。
fn validate_difficulty_scoring(d: &DifficultySection) -> Result<(), GateError> {
    let s = &d.scoring;
    for (name, value) in [
        ("scoring.w_hard", s.w_hard),
        ("scoring.w_code_fence", s.w_code_fence),
        ("scoring.w_math", s.w_math),
        ("scoring.w_questions", s.w_questions),
        ("scoring.w_messages", s.w_messages),
        ("scoring.w_tools", s.w_tools),
        ("scoring.w_length", s.w_length),
        ("scoring.w_easy_penalty", s.w_easy_penalty),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-scoring-invalid",
                    &[("field", name.to_string()), ("value", value.to_string())],
                ),
            });
        }
    }
    for (name, value) in [
        ("scoring.hard_cap", s.hard_cap),
        ("scoring.math_cap", s.math_cap),
        ("scoring.question_cap", s.question_cap),
        ("scoring.message_cap", s.message_cap),
        ("scoring.easy_cap", s.easy_cap),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-difficulty-scoring-invalid",
                    &[("field", name.to_string()), ("value", value.to_string())],
                ),
            });
        }
    }
    if !s.length_cap.is_finite() || !(1.0..=MAX_DIFFICULTY_LENGTH_CAP).contains(&s.length_cap) {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-scoring-invalid",
                &[
                    ("field", "scoring.length_cap".to_string()),
                    ("value", s.length_cap.to_string()),
                ],
            ),
        });
    }
    Ok(())
}

/// 单档校验：空名拒绝（V6 前置）+ model_id 撞名拒绝（V5）+ 无 enabled 候选拒绝
///（V6/V7）。候选匹配语义与 Router::collect_abstract_candidates 一致：
/// enabled vendor × enabled model × (model_id | abstract_id) CI 折叠
fn validate_tier_name(
    config: &Config,
    tier: super::Tier,
    name: &str,
    model_ids: &std::collections::HashSet<String>,
) -> Result<(), GateError> {
    use super::Tier;

    let tier_label = match tier {
        Tier::Low => "low",
        Tier::Mid => "mid",
        Tier::High => "high",
    };
    if name.trim().is_empty() {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-tier-empty",
                &[("tier", tier_label.to_string())],
            ),
        });
    }
    let normalized = name.trim().to_ascii_lowercase();
    if model_ids.contains(&normalized) {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-tier-collision",
                &[("kind", "tier".to_string()), ("name", name.to_string())],
            ),
        });
    }
    let served = config.vendors.iter().any(|v| {
        v.enabled
            && v.models.iter().any(|m| {
                m.enabled
                    && (m.model_id.eq_ignore_ascii_case(&normalized)
                        || m.abstract_id
                            .as_deref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(&normalized)))
            })
    });
    if !served {
        return Err(GateError::Config {
            message: t_args(
                "config-difficulty-tier-no-candidate",
                &[("tier", tier_label.to_string()), ("name", name.to_string())],
            ),
        });
    }
    Ok(())
}

/// V8（warn 级）：各档候选集完全相同 → 定档对厂商选择无效果，提示不阻断
fn warn_identical_difficulty_tiers(config: &Config) {
    use super::Tier;

    let d = &config.routing.difficulty;
    if !d.enabled {
        return;
    }
    let mut sets: Vec<Vec<String>> = Vec::new();
    let mut tier_names: Vec<(Tier, &str)> =
        vec![(Tier::Low, &d.tiers.low), (Tier::High, &d.tiers.high)];
    if let Some(mid) = &d.tiers.mid {
        tier_names.push((Tier::Mid, mid));
    }
    for (_, name) in &tier_names {
        sets.push(
            config
                .vendors
                .iter()
                .filter(|v| {
                    v.enabled
                        && v.models.iter().any(|m| {
                            m.enabled
                                && (m.model_id.eq_ignore_ascii_case(name)
                                    || m.abstract_id
                                        .as_deref()
                                        .is_some_and(|a| a.eq_ignore_ascii_case(name)))
                        })
                })
                .map(|v| v.id.clone())
                .collect(),
        );
    }
    if sets.len() >= 2 && sets.windows(2).all(|w| w[0] == w[1]) {
        tracing::warn!("{}", t("config-difficulty-tiers-identical"));
    }
}

/// vendor id 全局唯一 + 每 vendor 内 model id 唯一（跨 Vec 条目的唯一性
/// 约束，confers::validator 的字段级规则无法表达，保留手写）。
/// 去重按 ASCII 大小写不敏感：请求边界 ID 匹配（get_vendor_ci / 直连 /
/// cb_model）均 CI 折叠，仅差大小写的重复条目会让请求侧无法寻址意图项
///（get_vendor_ci 取声明序首个），故在配置面拒绝。
/// 路由查找键（vendor id / model_id / 声明的 abstract_id）先行空白拒绝
///（trim 为空），空白检查先于去重（空白非「重复」语义）。
fn validate_vendor_entries(config: &Config) -> Result<(), GateError> {
    let mut vendor_ids = std::collections::HashSet::new();
    for vendor in &config.vendors {
        if vendor.id.trim().is_empty() {
            return Err(GateError::Config {
                message: t("config-vendor-id-empty"),
            });
        }
        if !vendor_ids.insert(vendor.id.to_ascii_lowercase()) {
            return Err(GateError::Config {
                message: t_args(
                    "config-duplicate-vendor-id",
                    &[("vendor", vendor.id.clone())],
                ),
            });
        }
        if vendor.id.trim().chars().count() > VENDOR_ID_MAX_CHARS {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-id-too-long",
                    &[
                        ("vendor", vendor.id.clone()),
                        ("max", VENDOR_ID_MAX_CHARS.to_string()),
                    ],
                ),
            });
        }
        // rerank 型无官方默认端点：api_base 必须显式声明（URL 构建侧同拒）
        if matches!(vendor.protocol(), VendorProtocol::Rerank) && vendor.api_base.trim().is_empty()
        {
            return Err(GateError::Config {
                message: t_args(
                    "config-rerank-api-base-required",
                    &[("vendor", vendor.id.clone())],
                ),
            });
        }
        // gemini 的端点由 model+action 动态拼接，chat_path 静态覆盖语义冲突
        if matches!(vendor.protocol(), VendorProtocol::Gemini)
            && vendor
                .chat_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty())
        {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-gemini-chat-path-conflict",
                    &[("vendor", vendor.id.clone())],
                ),
            });
        }
        // anthropic_version 一旦声明必须为 YYYY-MM-DD 形态（缺省 None →
        // 出站按 "2023-06-01"）；仅做格式校验，不限制厂商类型
        if let Some(version) = vendor.anthropic_version.as_deref() {
            let bytes = version.as_bytes();
            let valid = bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
            if !valid {
                return Err(GateError::Config {
                    message: t_args(
                        "config-vendor-anthropic-version-invalid",
                        &[
                            ("vendor", vendor.id.clone()),
                            ("value", version.to_string()),
                        ],
                    ),
                });
            }
        }
        // 端点风格：白名单；responses 仅限 openai 型且与 chat_path 互斥。
        // 合法集与 CI 归一化单源 ApiStyle（未知值仍持原始串点名拒绝，守脱敏
        // 口径与既有守卫断言）
        if let Some(raw) = vendor.api_style_raw() {
            let Some(style) = ApiStyle::parse(raw) else {
                return Err(GateError::Config {
                    message: t_args(
                        "config-vendor-api-style-unknown",
                        &[("vendor", vendor.id.clone()), ("value", raw.to_string())],
                    ),
                });
            };
            if style == ApiStyle::Responses {
                if !matches!(vendor.protocol(), VendorProtocol::OpenAi) {
                    return Err(GateError::Config {
                        message: t_args(
                            "config-vendor-api-style-requires-openai",
                            &[
                                ("vendor", vendor.id.clone()),
                                ("type", vendor.type_.clone()),
                            ],
                        ),
                    });
                }
                if vendor
                    .chat_path
                    .as_deref()
                    .is_some_and(|path| !path.trim().is_empty())
                {
                    return Err(GateError::Config {
                        message: t_args(
                            "config-vendor-api-style-chat-path-conflict",
                            &[("vendor", vendor.id.clone())],
                        ),
                    });
                }
            }
        }
    }

    for vendor in &config.vendors {
        let mut model_ids = std::collections::HashSet::new();
        for model in &vendor.models {
            if model.model_id.trim().is_empty() {
                return Err(GateError::Config {
                    message: t_args("config-model-id-empty", &[("vendor", vendor.id.clone())]),
                });
            }
            if model
                .abstract_id
                .as_ref()
                .is_some_and(|abstract_id| abstract_id.trim().is_empty())
            {
                return Err(GateError::Config {
                    message: t_args(
                        "config-abstract-id-empty",
                        &[
                            ("vendor", vendor.id.clone()),
                            ("model", model.model_id.clone()),
                        ],
                    ),
                });
            }
            if !model_ids.insert(model.model_id.to_ascii_lowercase()) {
                return Err(GateError::Config {
                    message: t_args(
                        "config-duplicate-model-id",
                        &[
                            ("vendor", vendor.id.clone()),
                            ("model", model.model_id.clone()),
                        ],
                    ),
                });
            }
        }
    }
    Ok(())
}

/// 模型任务类型校验：`model_type` 合法值 chat / embedding / rerank；
/// 缺省或 trim 后为空等价未声明（存量配置零改动），非空未知值配置错误
/// 并点名 vendor / model / 原值。独立函数，不与嵌入组校验共享状态
fn validate_model_types(config: &Config) -> Result<(), GateError> {
    for vendor in &config.vendors {
        for model in &vendor.models {
            let Some(model_type) = model.model_type.as_deref() else {
                continue;
            };
            if model_type.trim().is_empty() {
                continue;
            }
            if ModelTask::parse(model_type).is_some() {
                continue;
            }
            return Err(GateError::Config {
                message: t_args(
                    "config-model-type-unknown",
                    &[
                        ("vendor", vendor.id.clone()),
                        ("model", model.model_id.clone()),
                        ("model_type", model_type.to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// rerank 分数归一化模式校验：none / sigmoid / minmax（trim 后空 = 默认 none）。
/// 未知值配置错误并点名原值（加载与热载同入口）。
fn validate_rerank_score_normalization(config: &Config) -> Result<(), GateError> {
    let mode = config.routing.rerank_score_normalization.trim();
    if mode.is_empty() || matches!(mode, "none" | "sigmoid" | "minmax") {
        return Ok(());
    }
    Err(GateError::Config {
        message: t_args(
            "config-rerank-score-normalization-unknown",
            &[("value", mode.to_string())],
        ),
    })
}

/// `[metrics]` 段缺省值：成本恢复窗口 24h（serde default 路径引用；
/// mod.rs 规则 14 禁实现函数，默认值落在本实现文件）。
pub fn default_metrics_cost_restore_window_hours() -> u64 {
    24
}

/// `[routing.affinity]` 段缺省值：`mode = "legacy"` 冻结现行为（成功即粘
/// 1h），`cache_worth_tokens = 1024`、`turn_gap_secs = 120`。
pub fn default_affinity_economy_mode() -> String {
    "legacy".to_string()
}

pub fn default_affinity_cache_worth_tokens() -> u64 {
    1024
}

pub fn default_affinity_turn_gap_secs() -> u64 {
    120
}

/// `[[vendors]] cache_ttl_secs` 缺省 300s（5 分钟缓存部署；1h 缓存部署配 3600）。
pub fn default_vendor_cache_ttl_secs() -> u64 {
    300
}

impl Default for crate::config::AffinitySection {
    fn default() -> Self {
        Self {
            mode: default_affinity_economy_mode(),
            session_headers: None,
            cache_worth_tokens: default_affinity_cache_worth_tokens(),
            turn_gap_secs: default_affinity_turn_gap_secs(),
        }
    }
}

impl crate::config::AffinityMode {
    /// 大小写不敏感 + trim 解析（与 [`ApiStyle::parse`] 同口径）；未知值
    /// None，由校验侧持原值点名拒绝。
    pub fn parse(raw: &str) -> Option<Self> {
        use crate::config::AffinityMode;
        // 热路径（每响应 settle + 每请求路由）调用：比较不分配
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("legacy") {
            Some(AffinityMode::Legacy)
        } else if raw.eq_ignore_ascii_case("auto") {
            Some(AffinityMode::Auto)
        } else if raw.eq_ignore_ascii_case("turn") {
            Some(AffinityMode::Turn)
        } else if raw.eq_ignore_ascii_case("off") {
            Some(AffinityMode::Off)
        } else {
            None
        }
    }

    /// 生效模式：未知值按 `Legacy`（回滚阀——配置期已拒绝非法值，此处仅
    /// 作手构配置的运行期兜底）。
    pub fn from_section(section: &crate::config::AffinitySection) -> Self {
        Self::parse(&section.mode).unwrap_or(Self::Legacy)
    }

    /// 机器码（与解释端点/指标标签同口径；合法值集单源于本文件）。
    pub fn as_str(self) -> &'static str {
        use crate::config::AffinityMode;
        match self {
            AffinityMode::Legacy => "legacy",
            AffinityMode::Auto => "auto",
            AffinityMode::Turn => "turn",
            AffinityMode::Off => "off",
        }
    }
}

impl Default for crate::config::MetricsSection {
    fn default() -> Self {
        Self {
            cost_restore_window_hours: default_metrics_cost_restore_window_hours(),
        }
    }
}

/// `[otlp]` 段缺省值：推送周期 60s、单次超时 10s（serde default 路径引用；
/// mod.rs 规则 14 禁实现函数，默认值落在本实现文件）。
pub fn default_otlp_interval_secs() -> u64 {
    60
}

pub fn default_otlp_timeout_secs() -> u64 {
    10
}

/// `[usage_insights]` 缺省开：纯内部统计、零出站、零新依赖，默认采集
/// 不改变任何响应面（关闭时四列写 NULL、两端点 404）。
pub fn default_usage_insights_enabled() -> bool {
    true
}

/// `[catalog_sync]` 缺省源：models.dev 公开模型目录 JSON API（2026-10-28
/// 实测形态：顶层 provider slug → {models: {id: {cost, limit, ...}}}）。
pub fn default_catalog_sync_source_url() -> String {
    "https://models.dev/api.json".to_string()
}

/// `[catalog_sync]` 单次拉取超时缺省 10s。
pub fn default_catalog_sync_timeout_secs() -> u64 {
    10
}

impl Default for crate::config::CatalogSyncSection {
    /// Rust 侧手构缺省与 serde 缺省单源：enabled=false（双门控运行时半边）、
    /// interval_secs=0（不周期）、alias 空表（slug 即 vendor_id）。
    fn default() -> Self {
        Self {
            enabled: false,
            source_url: default_catalog_sync_source_url(),
            interval_secs: 0,
            timeout_secs: default_catalog_sync_timeout_secs(),
            alias: HashMap::new(),
        }
    }
}

impl Default for crate::config::SubscriptionSection {
    /// Rust 侧手构缺省与 serde 缺省单源：enabled=false（双门控运行时半边）、
    /// client_id 空（未配置——authorize 请求点名拒绝，不猜默认应用）。
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: String::new(),
        }
    }
}

/// `[[quota.windows]]` 缺省值（serde default 路径引用；mod.rs 规则 14 禁
/// 实现函数，默认值落在本实现文件）：scope=api_key、period=rolling、
/// calendar 空串。限额与 rolling_secs 默认 0 = 由校验/判定侧解释。
pub fn default_quota_window_scope() -> String {
    "api_key".to_string()
}

pub fn default_quota_period() -> String {
    "rolling".to_string()
}

impl Default for crate::config::QuotaWindowConfig {
    /// Rust 侧手构缺省与 serde 缺省单源：scope=api_key、period=rolling。
    /// （derive Default 会给空串，validate 会拒——构造面缺省必须是合法缺省。）
    fn default() -> Self {
        Self {
            id: String::new(),
            scope: default_quota_window_scope(),
            period: default_quota_period(),
            rolling_secs: 0,
            calendar: String::new(),
            tz_offset_secs: 0,
            tokens_limit: 0,
            cost_limit_millicents: 0,
        }
    }
}

impl Default for crate::config::VendorBudgetConfig {
    /// 同 [`Default for QuotaWindowConfig`]：period=rolling 单源。
    fn default() -> Self {
        Self {
            vendor_id: String::new(),
            tokens_limit: 0,
            cost_limit_millicents: 0,
            period: default_quota_period(),
            rolling_secs: 0,
            calendar: String::new(),
            tz_offset_secs: 0,
        }
    }
}

/// `[[key_budgets.rules]]` 缺省窗口：daily（UTC 日历）。
pub fn default_key_budget_window() -> String {
    "daily".to_string()
}

impl Default for crate::config::KeyBudgetRule {
    /// Rust 侧手构缺省与 serde 缺省单源：window=daily、fail_open=false。
    fn default() -> Self {
        Self {
            api_key_hash: String::new(),
            cost_limit_millicents: 0,
            window: default_key_budget_window(),
            fail_open: false,
        }
    }
}

/// `[[auth.oidc_rules.rules]]` 缺省 TTL：3600s（1h；上限 86400s 由校验钳）。
pub fn default_oidc_ttl_secs() -> u64 {
    3600
}

impl Default for crate::config::OidcRuleConfig {
    /// Rust 侧手构缺省与 serde 缺省单源：ttl_secs=3600。
    fn default() -> Self {
        Self {
            issuer: String::new(),
            audience: String::new(),
            claims: std::collections::HashMap::new(),
            bind_key_hash: String::new(),
            ttl_secs: default_oidc_ttl_secs(),
        }
    }
}

/// `[auth.oidc_rules]` 校验（R-auth-001）：issuer 仅 https（loopback 放行
/// http，口径同 validate_catalog_sync）、audience 非空、claims 表**精确
/// 匹配红线**——任何键/值含 `*` 或 `?`（通配/前缀意图）硬拦、bind_key_hash
/// 必须是 64 位小写 hex 且存在于既有 `[auth]` api_keys/keys（短时键继承其
/// tier 与全部限制，母键缺失即规则悬空）、ttl_secs ∈ [1, 86400]、
/// 同 (issuer, audience) 不得重复声明。无规则 = 整体关闭不校验。
fn validate_oidc_rules(config: &Config) -> Result<(), GateError> {
    let Some(rules) = config
        .auth
        .as_ref()
        .and_then(|auth| auth.oidc_rules.as_ref())
        .filter(|section| !section.rules.is_empty())
        .map(|section| &section.rules)
    else {
        return Ok(());
    };
    let known_keys: std::collections::HashSet<&str> = config
        .auth
        .as_ref()
        .map(|auth| {
            auth.api_keys
                .iter()
                .map(|hash| hash.trim())
                .chain(auth.keys.iter().map(|entry| entry.hash.trim()))
                .collect()
        })
        .unwrap_or_default();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for rule in rules {
        let issuer = rule.issuer.trim();
        let (scheme, rest) = issuer.split_once("://").ok_or_else(|| GateError::Config {
            message: t_args(
                "config-oidc-issuer-invalid",
                &[("value", issuer.to_string())],
            ),
        })?;
        if scheme != "https" && !is_loopback_source(scheme, rest) {
            return Err(GateError::Config {
                message: t_args(
                    "config-oidc-issuer-scheme",
                    &[("value", issuer.to_string())],
                ),
            });
        }
        if rule.audience.trim().is_empty() {
            return Err(GateError::Config {
                message: t("config-oidc-audience-empty"),
            });
        }
        for (key, value) in &rule.claims {
            if key.trim().is_empty() {
                return Err(GateError::Config {
                    message: t("config-oidc-claim-key-empty"),
                });
            }
            if key.contains('*') || key.contains('?') || value.contains('*') || value.contains('?')
            {
                return Err(GateError::Config {
                    message: t_args(
                        "config-oidc-claims-wildcard",
                        &[("key", key.trim().to_string())],
                    ),
                });
            }
        }
        let bind = rule.bind_key_hash.trim();
        let is_64_hex =
            bind.len() == 64 && bind.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !is_64_hex || !known_keys.contains(bind) {
            return Err(GateError::Config {
                message: t_args(
                    "config-oidc-bind-key-missing",
                    &[("hash", bind.to_string())],
                ),
            });
        }
        if !(1..=86_400).contains(&rule.ttl_secs) {
            return Err(GateError::Config {
                message: t_args(
                    "config-oidc-ttl-out-of-range",
                    &[("value", rule.ttl_secs.to_string())],
                ),
            });
        }
        if !seen.insert((issuer.to_string(), rule.audience.trim().to_string())) {
            return Err(GateError::Config {
                message: t_args(
                    "config-oidc-rule-duplicate",
                    &[
                        ("issuer", issuer.to_string()),
                        ("audience", rule.audience.trim().to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// 旧单窗字段缺省：3600s 固定窗口 + 4 字符/token 估算（与历史一致）。
pub fn default_quota_window_secs() -> u64 {
    3600
}

pub fn default_est_chars_per_token() -> u64 {
    4
}

impl Default for crate::config::QuotaSection {
    fn default() -> Self {
        Self {
            enabled: false,
            window_secs: default_quota_window_secs(),
            est_chars_per_token: default_est_chars_per_token(),
            windows: Vec::new(),
        }
    }
}

/// `[routing.quota_factor]` 缺省值：mode=off（零参与）、w_quota=0.3——
/// 与 w_model/w_queue/w_window 同温标，仅 mode≠off 时被咨询。
pub fn default_quota_factor_mode() -> String {
    "off".to_string()
}

pub fn default_quota_factor_weight() -> f64 {
    0.3
}

impl Default for crate::config::QuotaFactorSection {
    fn default() -> Self {
        Self {
            mode: default_quota_factor_mode(),
            w_quota: default_quota_factor_weight(),
            tie_epsilon: default_quota_tie_epsilon(),
            degrade_enabled: default_quota_degrade_enabled(),
            degrade_used_tier1: default_quota_degrade_used_tier1(),
            degrade_multiplier_tier1: default_quota_degrade_multiplier_tier1(),
            degrade_used_tier2: default_quota_degrade_used_tier2(),
            degrade_multiplier_tier2: default_quota_degrade_multiplier_tier2(),
        }
    }
}

/// 平局打破 ε 缺省：0.05（score 分差 <5% 视为平局；0 = 关闭）。
pub fn default_quota_tie_epsilon() -> f64 {
    0.05
}

/// 两级降档缺省启用（缺省 true；显式 false 才关闭）。
pub fn default_quota_degrade_enabled() -> bool {
    true
}

/// 降档档位一阈值：已用 ≥90%（剩余 ≤10%）。
pub fn default_quota_degrade_used_tier1() -> f64 {
    0.90
}

/// 档位一乘数：×0.9（轻档，缓转向）。
pub fn default_quota_degrade_multiplier_tier1() -> f64 {
    0.9
}

/// 降档档位二阈值：已用 ≥98%（剩余 ≤2%，硬耗尽前最后一段）。
pub fn default_quota_degrade_used_tier2() -> f64 {
    0.98
}

/// 档位二乘数：×0.75（重档，强转向）。
pub fn default_quota_degrade_multiplier_tier2() -> f64 {
    0.75
}

impl crate::config::QuotaFactorMode {
    /// 大小写不敏感 + trim 解析（与 [`crate::config::AffinityMode::parse`]
    /// 同口径）；未知值 None，由校验侧持原值点名拒绝。
    pub fn parse(raw: &str) -> Option<Self> {
        use crate::config::QuotaFactorMode;
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("off") {
            Some(QuotaFactorMode::Off)
        } else if raw.eq_ignore_ascii_case("smart") {
            Some(QuotaFactorMode::Smart)
        } else if raw.eq_ignore_ascii_case("pace") {
            Some(QuotaFactorMode::Pace)
        } else {
            None
        }
    }

    /// 生效模式：未知值按 `Off`（回滚阀——配置期已拒绝非法值，此处仅作
    /// 手构配置的运行期兜底）。
    pub fn from_section(section: &crate::config::QuotaFactorSection) -> Self {
        Self::parse(&section.mode).unwrap_or(Self::Off)
    }

    /// 机器码（与指标标签/解释块同口径；合法值集单源于本文件）。
    pub fn as_str(self) -> &'static str {
        use crate::config::QuotaFactorMode;
        match self {
            QuotaFactorMode::Off => "off",
            QuotaFactorMode::Smart => "smart",
            QuotaFactorMode::Pace => "pace",
        }
    }
}

/// 生效配额窗口集（热读单点）：`[[quota.windows]]` 非空时原样返回（旧
/// `window_secs` 忽略），为空时把旧单窗字段编译为 `id="default"` 的
/// rolling 窗——旧配置行为逐位不变。两处并存时由调用方在加载期 WARN
///（`warn_quota_window_fields_conflict`）。
pub fn effective_quota_windows(
    quota: &crate::config::QuotaSection,
) -> Vec<crate::config::QuotaWindowConfig> {
    if !quota.windows.is_empty() {
        return quota.windows.clone();
    }
    vec![crate::config::QuotaWindowConfig {
        id: "default".to_string(),
        scope: default_quota_window_scope(),
        period: default_quota_period(),
        rolling_secs: quota.window_secs,
        calendar: String::new(),
        tz_offset_secs: 0,
        tokens_limit: 0,
        cost_limit_millicents: 0,
    }]
}

impl Default for crate::config::UsageInsightsSection {
    fn default() -> Self {
        Self {
            enabled: default_usage_insights_enabled(),
        }
    }
}

impl Default for crate::config::OtlpSection {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            interval_secs: default_otlp_interval_secs(),
            timeout_secs: default_otlp_timeout_secs(),
            tracing: Default::default(),
        }
    }
}

/// `[otlp.tracing]` 段 serde 缺省单源（enabled=false + ratio 0.0 = 双零默认
/// 关）。Default 实现落 loader（规则 14）。
impl Default for crate::config::OtlpTracingSection {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_ratio: default_otlp_tracing_sample_ratio(),
            endpoint: String::new(),
            protocol: default_otlp_tracing_protocol(),
            timeout_secs: default_otlp_tracing_timeout_secs(),
        }
    }
}

/// `[otlp.tracing]` sample_ratio 缺省 0.0（全不采样）。
pub(crate) fn default_otlp_tracing_sample_ratio() -> f64 {
    0.0
}

/// `[otlp.tracing]` 传输协议缺省 `http`。
pub(crate) fn default_otlp_tracing_protocol() -> String {
    "http".to_string()
}

/// `[otlp.tracing]` 单次导出超时缺省 10s。
pub(crate) fn default_otlp_tracing_timeout_secs() -> u64 {
    10
}

/// `[otlp]` 段边界校验：推送周期 1..=86400s（1s..1d）、超时 1..=300s、
/// enabled 时 endpoint 必填。enabled=false 不校验 endpoint（双门控的运行时
/// 半边关着即可，留空合法）。
fn validate_otlp(config: &Config) -> Result<(), GateError> {
    let otlp = &config.otlp;
    if !(1..=86400).contains(&otlp.interval_secs) {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-interval-secs-out-of-range",
                &[("value", otlp.interval_secs.to_string())],
            ),
        });
    }
    if !(1..=300).contains(&otlp.timeout_secs) {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-timeout-secs-out-of-range",
                &[("value", otlp.timeout_secs.to_string())],
            ),
        });
    }
    if otlp.enabled {
        validate_otlp_endpoint(otlp.endpoint.trim())?;
    }
    let tracing = &otlp.tracing;
    if !(0.0..=1.0).contains(&tracing.sample_ratio) {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-tracing-sample-ratio-out-of-range",
                &[("value", tracing.sample_ratio.to_string())],
            ),
        });
    }
    if !matches!(tracing.protocol.as_str(), "http" | "grpc") {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-tracing-protocol-invalid",
                &[("value", tracing.protocol.clone())],
            ),
        });
    }
    if !(1..=300).contains(&tracing.timeout_secs) {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-tracing-timeout-secs-out-of-range",
                &[("value", tracing.timeout_secs.to_string())],
            ),
        });
    }
    if tracing.enabled {
        validate_otlp_endpoint(tracing.endpoint.trim())?;
    }
    Ok(())
}

/// OTLP endpoint 校验（metrics 与 tracing 两面共用）：非空、只接受
/// http/https（其它 scheme 在 OTLP builder 内部才报错，配置期就拒掉更早
/// 暴露）、禁止 userinfo（凭证会随启动日志外泄）。此处按字符串判定而非
/// `url::Url`——`url` 是 optional 依赖（仅 database/sdk 启用），config 层
/// 无条件编译，引用它会打断 `--no-default-features` 构建。
fn validate_otlp_endpoint(endpoint: &str) -> Result<(), GateError> {
    if endpoint.is_empty() {
        return Err(GateError::Config {
            message: t_args("config-otlp-endpoint-required", &[]),
        });
    }
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| GateError::Config {
            message: t_args(
                "config-otlp-endpoint-invalid",
                &[("value", endpoint.to_string())],
            ),
        })?;
    if !matches!(scheme, "http" | "https") {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-endpoint-scheme",
                &[("value", scheme.to_string())],
            ),
        });
    }
    // authority 段 = 首个 `/` 之前；含 `@` 即 userinfo。
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(GateError::Config {
            message: t_args("config-otlp-endpoint-userinfo", &[]),
        });
    }
    if authority.is_empty() {
        return Err(GateError::Config {
            message: t_args(
                "config-otlp-endpoint-invalid",
                &[("value", endpoint.to_string())],
            ),
        });
    }
    Ok(())
}

/// `[routing.affinity].mode` 闭集校验 + `[[vendors]] cache_ttl_secs` 边界
/// 校验（加载与热载同入口）。非法值点名原值；`cache_ttl_secs` 合法区间
/// 1..=86400s（1s..1d）——0 会让粘持记录写完即过期、等于永不粘。
fn validate_affinity(config: &Config) -> Result<(), GateError> {
    use crate::config::AffinityMode;
    // 会话头别名：小写归一存回（HeaderMap 取值大小写不敏感，归一防重复
    // 探测漂移）；空白元素拒绝（空串头名无意义且吞掉后续探测位）。
    let normalized_session_headers = config
        .routing
        .affinity
        .session_headers
        .as_ref()
        .map(|list| {
            list.iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        });
    if let Some(list) = normalized_session_headers.as_ref() {
        for h in list.iter() {
            if h.is_empty() {
                return Err(GateError::Config {
                    message: t("config-affinity-session-headers-blank"),
                });
            }
        }
    }
    let raw = config.routing.affinity.mode.trim();
    if AffinityMode::parse(raw).is_none() {
        return Err(GateError::Config {
            message: t_args(
                "config-affinity-mode-unknown",
                &[("value", raw.to_string())],
            ),
        });
    }
    for vendor in &config.vendors {
        if !(1..=86400).contains(&vendor.cache_ttl_secs) {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-cache-ttl-secs-out-of-range",
                    &[
                        ("vendor", vendor.id.clone()),
                        ("value", vendor.cache_ttl_secs.to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// `[catalog_sync]` 段校验：enabled=false 完全旁路不校验（与「缺段」同
/// 语义，difficulty 先例）；enabled 时 source_url 仅 HTTPS（loopback 放行
/// http 供嵌入式上游/本机镜像）且禁 userinfo；alias 表键值非空白。
/// 加载与热更共用本入口（manager 更新路径同走 validate_config）。
fn validate_catalog_sync(config: &Config) -> Result<(), GateError> {
    let section = &config.catalog_sync;
    if !section.enabled {
        return Ok(());
    }
    if section.timeout_secs == 0 {
        return Err(GateError::Config {
            message: t("config-catalog-sync-timeout-zero"),
        });
    }
    // scheme/authority 按字符串判定（`url` 是 optional 依赖，与 validate_otlp
    // 同口径，保 `--no-default-features` 构建可编译）。
    let raw = section.source_url.trim();
    let (scheme, rest) = raw.split_once("://").ok_or_else(|| GateError::Config {
        message: t_args(
            "config-catalog-sync-source-url-invalid",
            &[("value", raw.to_string())],
        ),
    })?;
    if scheme != "https" && !is_loopback_source(scheme, rest) {
        // 安全约束（delta spec Constraints）：源固定 HTTPS；唯一例外是
        // loopback 上的 http（嵌入式上游 / 本机镜像，不出主机）
        return Err(GateError::Config {
            message: t_args(
                "config-catalog-sync-source-url-scheme",
                &[("value", raw.to_string())],
            ),
        });
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(GateError::Config {
            message: t("config-catalog-sync-source-url-userinfo"),
        });
    }
    for (slug, vendor_id) in &section.alias {
        if slug.trim().is_empty() || vendor_id.trim().is_empty() {
            return Err(GateError::Config {
                message: t("config-catalog-sync-alias-blank"),
            });
        }
    }
    Ok(())
}

/// loopback http 豁免判定：scheme=http 且 authority 主机 ∈
/// {127.0.0.1, localhost, ::1}（剥离端口；IPv6 字面量剥方括号）。
fn is_loopback_source(scheme: &str, rest: &str) -> bool {
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `[metrics]` 段边界校验：成本恢复窗口 1..=8760 小时（1h..1y）。
/// 越界一律配置错误并点名字段与原值（加载与热载同入口）。
fn validate_metrics(config: &Config) -> Result<(), GateError> {
    let window = config.metrics.cost_restore_window_hours;
    if !(1..=8760).contains(&window) {
        return Err(GateError::Config {
            message: t_args(
                "config-metrics-cost-restore-window-hours-out-of-range",
                &[("value", window.to_string())],
            ),
        });
    }
    Ok(())
}

/// `[responses]` 段边界校验：保留时长、单列上限与后台并发上限。
/// 越界一律配置错误并点名字段与原值（加载与热载同入口）。
fn validate_responses(config: &Config) -> Result<(), GateError> {
    let responses = &config.responses;
    if !(1..=8760).contains(&responses.retention_hours) {
        return Err(GateError::Config {
            message: t_args(
                "config-responses-retention-hours-out-of-range",
                &[("value", responses.retention_hours.to_string())],
            ),
        });
    }
    if !(1024..=16 * 1024 * 1024).contains(&responses.max_response_bytes) {
        return Err(GateError::Config {
            message: t_args(
                "config-responses-max-response-bytes-out-of-range",
                &[("value", responses.max_response_bytes.to_string())],
            ),
        });
    }
    if !(1..=256).contains(&responses.max_background_jobs) {
        return Err(GateError::Config {
            message: t_args(
                "config-responses-max-background-jobs-out-of-range",
                &[("value", responses.max_background_jobs.to_string())],
            ),
        });
    }
    Ok(())
}

/// 嵌入兼容组校验（R1-R3）：同一 `compat_group`（trim 后精确匹配）内
/// upstream `model_id` 大小写不敏感一致（R1）、已声明 `dimensions` 全部
/// 相等（R2）、组名不得跨 `abstract_id`（R3）；违反任一条 → 配置错误且
/// 错误信息点名 vendor id 与冲突条目（R4）。两个新字段未声明的条目零
/// 参与——存量配置行为逐字节不变；加载与热载共用本入口，两路同拒
fn validate_embedding_groups(config: &Config) -> Result<(), GateError> {
    /// 组内首个声明条目锚点（后续条目与之比对，错误信息取两侧原文）
    struct GroupAnchor<'a> {
        vendor: &'a str,
        model_id: &'a str,
        dims: Option<(&'a str, &'a str, u32)>,
        abstract_id: Option<(&'a str, &'a str)>,
    }

    let mut groups: std::collections::HashMap<&str, GroupAnchor<'_>> =
        std::collections::HashMap::new();
    for vendor in &config.vendors {
        for model in &vendor.models {
            // trim 后为空 = 未声明（与路由侧组键解析同口径）
            let Some(group) = model
                .compat_group
                .as_deref()
                .map(str::trim)
                .filter(|group| !group.is_empty())
            else {
                continue;
            };
            // 声明组名不得含单例组键分隔符 `/::`——否则可与「未声明条目的
            // 单例键 {vendor}/::{model}」撞键，静默击穿安全默认（未声明 =
            // 永不与任何厂商互换）
            if group.contains("/::") {
                return Err(GateError::Config {
                    message: t_args(
                        "config-embedding-group-reserved-separator",
                        &[("group", group.to_string())],
                    ),
                });
            }
            match groups.get_mut(group) {
                None => {
                    groups.insert(
                        group,
                        GroupAnchor {
                            vendor: &vendor.id,
                            model_id: &model.model_id,
                            dims: model
                                .dimensions
                                .map(|dims| (vendor.id.as_str(), model.model_id.as_str(), dims)),
                            abstract_id: model
                                .abstract_id
                                .as_deref()
                                .map(str::trim)
                                .filter(|abstract_id| !abstract_id.is_empty())
                                .map(|abstract_id| (abstract_id, vendor.id.as_str())),
                        },
                    );
                }
                Some(anchor) => {
                    // R1：同组 upstream model_id 必须一致（大小写不敏感）
                    if !model.model_id.eq_ignore_ascii_case(anchor.model_id) {
                        return Err(GateError::Config {
                            message: t_args(
                                "config-embedding-group-model-mismatch",
                                &[
                                    ("group", group.to_string()),
                                    ("vendor_a", anchor.vendor.to_string()),
                                    ("model_a", anchor.model_id.to_string()),
                                    ("vendor_b", vendor.id.clone()),
                                    ("model_b", model.model_id.clone()),
                                ],
                            ),
                        });
                    }
                    // R2：同组已声明 dimensions 必须相等（未声明不参与比较）
                    if let Some(dims) = model.dimensions {
                        match anchor.dims {
                            Some((old_vendor, old_model, old_dims)) if old_dims != dims => {
                                return Err(GateError::Config {
                                    message: t_args(
                                        "config-embedding-group-dimension-conflict",
                                        &[
                                            ("group", group.to_string()),
                                            ("vendor_a", old_vendor.to_string()),
                                            ("model_a", old_model.to_string()),
                                            ("dims_a", old_dims.to_string()),
                                            ("vendor_b", vendor.id.clone()),
                                            ("model_b", model.model_id.clone()),
                                            ("dims_b", dims.to_string()),
                                        ],
                                    ),
                                });
                            }
                            None => anchor.dims = Some((&vendor.id, &model.model_id, dims)),
                            _ => {}
                        }
                    }
                    // R3：组名不得跨 abstract_id（缺 abstract 的条目不参与）
                    if let Some(abstract_id) = model
                        .abstract_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|abstract_id| !abstract_id.is_empty())
                    {
                        match anchor.abstract_id {
                            Some((old_abstract, old_vendor)) if old_abstract != abstract_id => {
                                return Err(GateError::Config {
                                    message: t_args(
                                        "config-embedding-group-cross-abstract",
                                        &[
                                            ("group", group.to_string()),
                                            ("abstract_a", old_abstract.to_string()),
                                            ("vendor_a", old_vendor.to_string()),
                                            ("abstract_b", abstract_id.to_string()),
                                            ("vendor_b", vendor.id.clone()),
                                        ],
                                    ),
                                });
                            }
                            None => anchor.abstract_id = Some((abstract_id, vendor.id.as_str())),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// 脱敏：缺失告警不回显配置原文（防密钥泄进日志），只报值长度/序号；
/// key 池显式列表逐名校验，缺失只告警不拒
fn warn_missing_api_key_envs(config: &Config) {
    for vendor in &config.vendors {
        if std::env::var(&vendor.api_key_env).is_err() {
            tracing::warn!(
                "{}",
                t_args(
                    "config-api-key-env-missing",
                    &[
                        ("vendor", vendor.id.clone()),
                        ("length", vendor.api_key_env.len().to_string()),
                    ],
                )
            );
        }
        for (index, env) in vendor.api_key_envs.iter().enumerate() {
            if std::env::var(env).is_err() {
                tracing::warn!(
                    "{}",
                    t_args(
                        "config-api-key-pool-env-missing",
                        &[
                            ("vendor", vendor.id.clone()),
                            ("index", index.to_string()),
                            ("length", env.len().to_string()),
                        ],
                    )
                );
            }
        }
    }
}

/// [[model_catalog]] 段校验：条目数上限、model_id 去重、context_window > 0；
/// 加载与热更新共用，两路同拒
fn validate_model_catalog(config: &Config) -> Result<(), GateError> {
    if config.model_catalog.len() > crate::config::model_catalog::MAX_MODEL_CATALOG_ENTRIES {
        return Err(GateError::Config {
            message: t_args(
                "config-catalog-too-many",
                &[
                    ("count", config.model_catalog.len().to_string()),
                    (
                        "cap",
                        crate::config::model_catalog::MAX_MODEL_CATALOG_ENTRIES.to_string(),
                    ),
                ],
            ),
        });
    }
    let mut catalog_ids = std::collections::HashSet::new();
    for entry in &config.model_catalog {
        if entry.model_id.trim().is_empty() {
            return Err(GateError::Config {
                message: t_args("config-catalog-model-id-empty", &[]),
            });
        }
        if !catalog_ids.insert(&entry.model_id) {
            return Err(GateError::Config {
                message: t_args(
                    "config-duplicate-catalog-model-id",
                    &[("model", entry.model_id.clone())],
                ),
            });
        }
        if entry.context_window == 0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-catalog-context-window-zero",
                    &[("model", entry.model_id.clone())],
                ),
            });
        }
        validate_catalog_prices(entry)?;
    }
    Ok(())
}

/// 审查：价格有限（拒 NaN/Inf）且 ≥ 0——NaN 的比较恒 false、
/// 负价是配置错误，两路同拒； 计价按此面消费。
fn validate_catalog_prices(
    entry: &crate::config::model_catalog::ModelCatalogEntry,
) -> Result<(), GateError> {
    let prices = [
        ("input_price", entry.input_price),
        ("output_price", entry.output_price),
        ("cache_read_price", entry.cache_read_price),
        ("cache_creation_price", entry.cache_creation_price),
        ("cache_creation_1h_price", entry.cache_creation_1h_price),
        ("rerank_price_per_1k_docs", entry.rerank_price_per_1k_docs),
    ];
    for (field, price) in prices {
        if !price.is_finite() || price < 0.0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-catalog-price-invalid",
                    &[
                        ("model", entry.model_id.clone()),
                        ("field", field.to_string()),
                        ("value", price.to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// 窗口 <100ms 使 CB Open→HalfOpen 即刻转换（故障期探针风暴，等于关闭
/// 熔断冷却）。加载与热更新共用，两路同拒
fn validate_resilience_circuit(config: &Config) -> Result<(), GateError> {
    if config.resilience.circuit_open_timeout_ms < 100 {
        return Err(GateError::Config {
            message: t_args(
                "config-circuit-open-timeout-too-small",
                &[(
                    "value",
                    config.resilience.circuit_open_timeout_ms.to_string(),
                )],
            ),
        });
    }
    Ok(())
}

/// [quota] 窗口须 ≥1s：0 窗口每次访问即翻转（配额永不满），两路同拒
fn validate_quota_window(config: &Config) -> Result<(), GateError> {
    if config.quota.window_secs == 0 {
        return Err(GateError::Config {
            message: t_args(
                "config-quota-window-too-small",
                &[("value", config.quota.window_secs.to_string())],
            ),
        });
    }
    Ok(())
}

/// [quota] × [auth] 组合告警：配额开启而认证关闭/缺段时，配额租户键取自
/// 客户端自报的 key 头（不经认证校验），计费控制退化为尽力而为——
/// 不构成拒绝级错误，显性 warn 不静默
fn warn_quota_without_auth(config: &Config) {
    if quota_without_auth(config) {
        tracing::warn!("{}", t_args("config-quota-without-auth", &[]));
    }
}

/// 告警谓词（纯函数）：quota 开 && auth 关/缺段。
fn quota_without_auth(config: &Config) -> bool {
    config.quota.enabled && !config.auth.as_ref().is_some_and(|auth| auth.enabled)
}

/// window id 长度上限（字符）：与 MySQL 方言 `quota_ledger.window_id
/// VARCHAR(128)` 对齐——超长行每 flush 必败，配置面提前拒绝
const QUOTA_WINDOW_ID_MAX_CHARS: usize = 128;
/// vendor id 长度上限（字符）：账本行键 `__vendor__:{vendor_id}`（前缀
/// 11 字符）须放进 MySQL 方言 `quota_ledger.api_key_hash VARCHAR(64)`
const VENDOR_ID_MAX_CHARS: usize = 53;

/// `[[quota.windows]]` 校验：id 非空去重、scope/period 闭集、rolling 周期
/// ≥1s（0 窗口每次访问即翻转、配额永不满，与 `window_secs` 同款拒绝）、
/// calendar 形态要求 daily|weekly|monthly 且 rolling_secs 必须为 0（两形态
/// 互斥，同配会让窗口起点语义二义）；数值域 tokens_limit / tz_offset_secs
/// 合法性。enabled=false 时同样校验（段缺省即空 vec，零成本）。
fn validate_quota_windows(config: &Config) -> Result<(), GateError> {
    // 去重按 ASCII 大小写不敏感：window id 进指标标签与账本行键，仅差
    // 大小写的重复条目会产生双标签/双行（与 vendor id 去重口径一致）
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for window in &config.quota.windows {
        let id = window.id.trim();
        if id.is_empty() {
            return Err(GateError::Config {
                message: t("config-quota-window-id-empty"),
            });
        }
        if !seen.insert(id.to_ascii_lowercase()) {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-window-id-duplicate",
                    &[("id", id.to_string())],
                ),
            });
        }
        if id.chars().count() > QUOTA_WINDOW_ID_MAX_CHARS {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-window-id-too-long",
                    &[
                        ("id", id.to_string()),
                        ("max", QUOTA_WINDOW_ID_MAX_CHARS.to_string()),
                    ],
                ),
            });
        }
        if !matches!(window.scope.trim(), "api_key" | "global") {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-window-scope-unknown",
                    &[("id", id.to_string()), ("value", window.scope.clone())],
                ),
            });
        }
        // scope=global 必须显式声明限额：全局窗按请求租户回填会让同一窗的
        // 判定随请求者漂移（A 租户的 per-key 限额约束整个舰队）、无限额租户
        // 整体绕过——舰队级约束必须配置面显式
        if window.scope.trim() == "global"
            && window.tokens_limit == 0
            && window.cost_limit_millicents == 0
        {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-window-global-empty-limits",
                    &[("id", id.to_string())],
                ),
            });
        }
        validate_quota_period(
            "quota_window",
            id,
            &window.period,
            window.rolling_secs,
            &window.calendar,
            window.tz_offset_secs,
        )?;
    }
    Ok(())
}

/// 周期两形态校验（`[[quota.windows]]` 与 `[[vendor_budgets]]` 共用单点——
/// 两段周期语法同源，故 i18n 键也共用、由 `$section` 区分来源）：rolling
/// 要求 `rolling_secs ≥ 1` 且 calendar 留空；calendar 要求
/// daily|weekly|monthly 且 `rolling_secs == 0`（两形态互斥，同配会让窗口
/// 起点语义二义）。tz_offset_secs 限 ±24h（偏移超过一天会与相邻日历窗重叠）。
fn validate_quota_period(
    section: &str,
    id: &str,
    period: &str,
    rolling_secs: u64,
    calendar: &str,
    tz_offset_secs: i64,
) -> Result<(), GateError> {
    let period = period.trim();
    match period {
        "rolling" => {
            if rolling_secs == 0 {
                return Err(GateError::Config {
                    message: t_args(
                        "config-quota-rolling-secs-too-small",
                        &[("section", section.to_string()), ("id", id.to_string())],
                    ),
                });
            }
            if !calendar.trim().is_empty() {
                return Err(GateError::Config {
                    message: t_args(
                        "config-quota-period-mixed",
                        &[("section", section.to_string()), ("id", id.to_string())],
                    ),
                });
            }
        }
        "calendar" => {
            if rolling_secs != 0 {
                return Err(GateError::Config {
                    message: t_args(
                        "config-quota-period-mixed",
                        &[("section", section.to_string()), ("id", id.to_string())],
                    ),
                });
            }
            if !matches!(calendar.trim(), "daily" | "weekly" | "monthly") {
                return Err(GateError::Config {
                    message: t_args(
                        "config-quota-calendar-unknown",
                        &[
                            ("section", section.to_string()),
                            ("id", id.to_string()),
                            ("value", calendar.trim().to_string()),
                        ],
                    ),
                });
            }
        }
        _ => {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-period-unknown",
                    &[
                        ("section", section.to_string()),
                        ("id", id.to_string()),
                        ("value", period.to_string()),
                    ],
                ),
            });
        }
    }
    if !(-86_400..=86_400).contains(&tz_offset_secs) {
        return Err(GateError::Config {
            message: t_args(
                "config-quota-tz-offset-out-of-range",
                &[
                    ("section", section.to_string()),
                    ("id", id.to_string()),
                    ("value", tz_offset_secs.to_string()),
                ],
            ),
        });
    }
    Ok(())
}

/// `[[vendor_budgets]]` 校验：vendor_id 非空且与 `[[vendors]]` 对得上（错拼
/// 预算段静默失效是运维事故）、去重、周期两形态同 [`validate_quota_period`]；
/// 双限额均 0 = 该段空转，拒绝（声明式预算必须至少一个维度）。
fn validate_vendor_budgets(config: &Config) -> Result<(), GateError> {
    // HashSet + 小写归一：与 validate_vendor_entries 的去重口径一致（那里用
    // to_ascii_lowercase）。原实现用 Vec::contains 且大小写敏感，导致
    // `"OpenAI"` 与 `"openai"` 两段预算都通过校验、运行时抢同一账本行；
    // 顺带把 O(n²) 降为 O(n)。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for budget in &config.vendor_budgets {
        let vendor_id = budget.vendor_id.trim();
        if vendor_id.is_empty() {
            return Err(GateError::Config {
                message: t("config-vendor-budget-id-empty"),
            });
        }
        if !seen.insert(vendor_id.to_ascii_lowercase()) {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-budget-id-duplicate",
                    &[("vendor", vendor_id.to_string())],
                ),
            });
        }
        if !config
            .vendors
            .iter()
            .any(|v| v.id.trim().eq_ignore_ascii_case(vendor_id))
        {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-budget-unknown-vendor",
                    &[("vendor", vendor_id.to_string())],
                ),
            });
        }
        if budget.tokens_limit == 0 && budget.cost_limit_millicents == 0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-vendor-budget-empty-limits",
                    &[("vendor", vendor_id.to_string())],
                ),
            });
        }
        validate_quota_period(
            "vendor_budget",
            vendor_id,
            &budget.period,
            budget.rolling_secs,
            &budget.calendar,
            budget.tz_offset_secs,
        )?;
    }
    Ok(())
}

/// `[key_budgets]` 校验：hash 必须是 64 位小写 hex（与 `[auth]` 摘要同口径，
/// 错拼预算段静默失效是运维事故）、cost_limit_millicents > 0、window 闭集
/// daily|monthly、同 (hash, window) 不得重复声明（重复条目会在同一账本窗
/// 上双写占用，语义二义）。空 rules = 整体关闭，不校验其余面。
fn validate_key_budgets(config: &Config) -> Result<(), GateError> {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for rule in &config.key_budgets.rules {
        let hash = rule.api_key_hash.trim();
        let is_64_hex =
            hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !is_64_hex {
            return Err(GateError::Config {
                message: t_args(
                    "config-key-budget-hash-invalid",
                    &[("hash", hash.to_string())],
                ),
            });
        }
        if rule.cost_limit_millicents == 0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-key-budget-limit-invalid",
                    &[("hash", hash.to_string())],
                ),
            });
        }
        if !matches!(rule.window.trim(), "daily" | "monthly") {
            return Err(GateError::Config {
                message: t_args(
                    "config-key-budget-window-unknown",
                    &[
                        ("hash", hash.to_string()),
                        ("value", rule.window.trim().to_string()),
                    ],
                ),
            });
        }
        if !seen.insert((hash.to_string(), rule.window.trim().to_string())) {
            return Err(GateError::Config {
                message: t_args(
                    "config-key-budget-duplicate",
                    &[
                        ("hash", hash.to_string()),
                        ("window", rule.window.trim().to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// `[[subscription_accounts]]` / `[subscription]` 校验：账号 id 非空且唯一、
/// vendor 绑定非空且与 `[[vendors]]` 对得上（错拼绑定静默不服务是运维事故，
/// 口径同 [`validate_vendor_budgets`]）。enabled=false 时账号声明仍校验——
/// 声明是持久 vault 键的绑定面，与运行时开关正交。
fn validate_subscription(config: &Config) -> Result<(), GateError> {
    let mut seen: Vec<&str> = Vec::new();
    for account in &config.subscription_accounts {
        let id = account.id.trim();
        if id.is_empty() {
            return Err(GateError::Config {
                message: t("config-subscription-account-id-empty"),
            });
        }
        if seen.contains(&id) {
            return Err(GateError::Config {
                message: t_args(
                    "config-subscription-account-id-duplicate",
                    &[("account", id.to_string())],
                ),
            });
        }
        seen.push(id);
        let vendor_id = account.vendor_id.trim();
        if vendor_id.is_empty() {
            return Err(GateError::Config {
                message: t_args(
                    "config-subscription-account-vendor-empty",
                    &[("account", id.to_string())],
                ),
            });
        }
        if !config
            .vendors
            .iter()
            .any(|v| v.id.trim().eq_ignore_ascii_case(vendor_id))
        {
            return Err(GateError::Config {
                message: t_args(
                    "config-subscription-account-unknown-vendor",
                    &[
                        ("account", id.to_string()),
                        ("vendor", vendor_id.to_string()),
                    ],
                ),
            });
        }
    }
    Ok(())
}

/// `[routing.quota_factor].mode` 闭集校验（off/smart/pace，大小写不敏感 +
/// trim；与 [`crate::config::QuotaFactorMode::parse`] 单源）。非法值以原值
/// 点名拒绝——热读回滚阀 `from_section` 只兜手构配置，加载面必须刚性拒绝。
fn validate_quota_factor(config: &Config) -> Result<(), GateError> {
    let section = &config.routing.quota_factor;
    if crate::config::QuotaFactorMode::parse(&section.mode).is_none() {
        return Err(GateError::Config {
            message: t_args(
                "config-quota-factor-mode-unknown",
                &[("value", section.mode.trim().to_string())],
            ),
        });
    }
    // 配额时钟补全数值域（R-budget-002）：ε ∈ [0,1]（0 = 关闭平局打破）、
    // 阈值 ∈ (0,1]、乘数 ∈ (0,1]（>1 会变成升档，语义不允许）、档一阈值
    // 必须严格小于档二（嵌套先判紧档；相等让档二永不可达）
    if !section.tie_epsilon.is_finite() || !(0.0..=1.0).contains(&section.tie_epsilon) {
        return Err(GateError::Config {
            message: t_args(
                "config-quota-factor-field-invalid",
                &[("field", "tie_epsilon".to_string())],
            ),
        });
    }
    for (field, value) in [
        ("degrade_used_tier1", section.degrade_used_tier1),
        ("degrade_used_tier2", section.degrade_used_tier2),
        ("degrade_multiplier_tier1", section.degrade_multiplier_tier1),
        ("degrade_multiplier_tier2", section.degrade_multiplier_tier2),
    ] {
        if !value.is_finite() || value <= 0.0 || value > 1.0 {
            return Err(GateError::Config {
                message: t_args(
                    "config-quota-factor-field-invalid",
                    &[("field", field.to_string())],
                ),
            });
        }
    }
    if section.degrade_used_tier1 >= section.degrade_used_tier2 {
        return Err(GateError::Config {
            message: t("config-quota-factor-tier-order"),
        });
    }
    Ok(())
}

/// 两窗口配置形态并存告警：`[[quota.windows]]` 非空时旧 `window_secs` 被
/// 忽略（生效集见 [`effective_quota_windows`]）——显性留痕，不静默忽略。
fn warn_quota_window_fields_conflict(config: &Config) {
    if !config.quota.windows.is_empty() && config.quota.window_secs != default_quota_window_secs() {
        tracing::warn!(
            "{}",
            t_args(
                "config-quota-window-fields-conflict",
                &[("value", config.quota.window_secs.to_string())],
            )
        );
    }
}

/// [auth] 段条目的 ConfigProvider 薄适配：管理面密钥逐条挂到合成键上，
/// 供 JwtSecretValidator 按标量键读取
struct AdminSecretProvider {
    entries: HashMap<String, AnnotatedValue>,
}

impl AdminSecretProvider {
    fn insert(&mut self, key: String, secret: &str) {
        let annotated = AnnotatedValue::new(
            ConfigValue::string(secret),
            SourceId::new("fluxweir-config"),
            key.clone(),
        );
        self.entries.insert(key, annotated);
    }
}

impl ConfigProvider for AdminSecretProvider {
    fn get_raw(&self, key: &str) -> Option<&AnnotatedValue> {
        self.entries.get(key)
    }

    fn keys(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

/// 管理面密钥启动校验：[auth] 条目的契约是 64 个小写 hex 字符的 SHA256
/// 摘要。两层校验——① 形态契约（confers 长度/弱词表拦不住「长口令误填
/// 当摘要」「大写摘要」）；② confers JwtSecretValidator（仅 Critical 级
/// 升级为拒绝启动，非 Critical 留 debug）。条目不达标 fail-fast（加载拒
/// 启动、热更拒绝）；[auth] 缺段或列表为空直接放行。错误按配置索引固定
/// 顺序拼装，只报位置与结论不回显原文
fn validate_admin_secrets(config: &Config) -> Result<(), GateError> {
    let Some(auth) = &config.auth else {
        return Ok(());
    };

    // 固定序条目表：配置索引即遍历序（错误明细顺序跨重启稳定）
    let mut entries: Vec<(String, &str)> = Vec::new();
    for (index, key) in auth.api_keys.iter().enumerate() {
        entries.push((format!("auth.api_keys[{index}]"), key));
    }
    for (index, entry) in auth.keys.iter().enumerate() {
        entries.push((format!("auth.keys[{index}].hash"), &entry.hash));
    }
    if entries.is_empty() {
        return Ok(());
    }

    let mut provider = AdminSecretProvider {
        entries: HashMap::new(),
    };
    let mut rejections: Vec<String> = Vec::new();
    let mut shape_ok: Vec<String> = Vec::new();
    for (label, secret) in &entries {
        // 小写 hex-64：与认证侧 sha256_hex 输出形态对齐（大写摘要恒无法
        // 认证）；谓词须「hex 字符且非大写」，防 64 个非 hex 小写串漏过
        if secret.len() != 64
            || !secret
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            rejections.push(format!(
                "{label}: {}",
                t_args(
                    "config-admin-secret-not-hex-digest",
                    &[("length", secret.len().to_string())],
                )
            ));
            continue;
        }
        provider.insert(label.clone(), secret);
        shape_ok.push(label.clone());
    }

    for key in &shape_ok {
        let validator = JwtSecretValidator::new().with_secret_key(key.clone());
        if let Err(violations) = validator.validate(&provider) {
            for violation in violations {
                if violation.severity == ViolationSeverity::Critical {
                    rejections.push(format!("{key}: {}", violation.message));
                } else {
                    // 非 Critical（当前规则内核为 Warning 级）不构成拒绝
                    // 启动，但必须留痕：规则内核后续增强新增违规时不会被
                    // 零痕迹吞掉（错误显性化纪律）
                    tracing::debug!(
                        target: "fluxweir::config",
                        key = %key,
                        severity = ?violation.severity,
                        "{}",
                        t_args(
                            "config-non-critical-admin-secret-violation",
                            &[("message", violation.message.clone())]
                        )
                    );
                }
            }
        }
    }

    if rejections.is_empty() {
        Ok(())
    } else {
        Err(GateError::Config {
            message: t_args(
                "config-weak-admin-secret",
                &[("details", rejections.join("; "))],
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(quota_enabled: bool, auth: Option<crate::config::AuthSection>) -> Config {
        Config {
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
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: vec![],
            auth,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: crate::config::QuotaSection {
                enabled: quota_enabled,
                ..Default::default()
            },
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
        }
    }

    /// quota 开 + auth 关/缺段 → 告警；quota 关或 auth 开 → 不告警。
    #[test]
    fn quota_without_auth_predicate() {
        use crate::config::{AuthSection, AuthTier};
        let auth_on = AuthSection {
            enabled: true,
            api_keys: vec![],
            header: Default::default(),
            keys: vec![crate::config::AuthKeyEntry {
                hash: "a".repeat(64),
                tier: AuthTier::Elastic,
                token_quota: 0,
                cost_quota_millicents: 0,
            }],
            oidc_rules: None,
        };
        let auth_off = AuthSection {
            enabled: false,
            oidc_rules: None,
            ..auth_on.clone()
        };
        assert!(quota_without_auth(&config_with(true, None)), "缺段 → 告警");
        assert!(
            quota_without_auth(&config_with(true, Some(auth_off))),
            "enabled=false → 告警"
        );
        assert!(
            !quota_without_auth(&config_with(true, Some(auth_on))),
            "认证开启 → 不告警"
        );
        assert!(
            !quota_without_auth(&config_with(false, None)),
            "quota 关 → 不告警"
        );
    }

    /// vendors 条目缺必填字段的定位增强：confers data error 只给容器坐标
    /// （`data error at 'vendors[0]'`）且字段名为空，回落纯 serde TOML
    /// 解析后必须点名缺失字段并给出文件位置。
    #[test]
    fn missing_vendor_field_error_names_the_field() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"[server]
host = "127.0.0.1"
port = 8080

[database]
path = ":memory:"

[[vendors]]
id = "dead-vendor"
name = "Dead"
type_ = "openai"
api_key_env = "DEAD_KEY"
concurrency = { requests_per_second = 1 }
"#,
        )
        .expect("write failing config");

        let error = load_config_from_path(&path).expect_err("缺 rate_limit/concurrency 必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("rate_limit"),
            "错误必须点名缺失字段: {message}"
        );
        assert!(
            message.contains(&format!("{}:", path.display())),
            "错误必须带文件位置定位: {message}"
        );
    }

    // [routing.difficulty] 校验（V1-V8）

    /// 组装含 [[vendors]] 与 [routing.difficulty] 的完整配置并解析
    ///（parse_config_content 解析即校验，难度校验错误在 Err 消息中点名字段）
    fn parse_difficulty_toml(vendors: &str, difficulty: &str) -> Result<Config, GateError> {
        // Config.vendors 无 serde default：空厂商场景必须显式 `vendors = []`；
        // 顶层键须在任何 [table] 头之前，否则落进 [database]
        let vendors_block = if vendors.is_empty() {
            "vendors = []\n".to_string()
        } else {
            format!("{vendors}\n")
        };
        let toml = format!(
            "{vendors_block}[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n\
             [database]
path = \":memory:\"

[routing]
[routing.difficulty]
{difficulty}
"
        );
        parse_config_content(Path::new("config.toml"), &toml)
    }

    /// 一个 enabled vendor，models 为 `[[vendors.models]]` TOML 片段
    fn difficulty_vendor(id: &str, models: &str) -> String {
        format!(
            "[[vendors]]\nid = \"{id}\"\nname = \"{id}\"\ntype_ = \"openai\"\n\
             api_key_env = \"{id}_KEY\"\n\
             rate_limit = {{ type = \"time_window\", start = \"00:00\", end = \"24:00\" }}\n\
             concurrency = {{ requests_per_second = 1 }}\n{models}\n"
        )
    }

    fn model_entry(model_id: &str, abstract_id: &str) -> String {
        format!("[[vendors.models]]\nmodel_id = \"{model_id}\"\nabstract_id = \"{abstract_id}\"\n")
    }

    const DIFF_VENDORS_OK: &str = "";

    #[test]
    fn difficulty_disabled_bypasses_validation() {
        // enabled=false：垃圾值也不校验（与「缺段」同语义）
        let config = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = false\nw_clf = 0.0\nw_rule = 0.0\nscore_low = 0.9\nscore_high = 0.1\n",
        )
        .expect("disabled 段即使全非法也必须旁路");
        assert!(!config.routing.difficulty.enabled);
    }

    #[test]
    fn difficulty_v1_rejects_all_zero_weights() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.0\nw_rule = 0.0\nscore_low = 0.35\nscore_high = 0.65\n",
        )
        .expect_err("V1 权重全零必须拒绝");
        assert!(
            error.to_string().contains("w_clf"),
            "错误必须点名 w_clf: {error}"
        );
    }

    #[test]
    fn difficulty_v2_rejects_inverted_thresholds() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nscore_low = 0.65\nscore_high = 0.35\n",
        )
        .expect_err("V2 阈值倒置必须拒绝");
        assert!(
            error.to_string().contains("score_low"),
            "错误必须点名 score_low: {error}"
        );
    }

    #[test]
    fn difficulty_v3_rejects_unknown_policy() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nmid_band_policy = \"medium\"\n",
        )
        .expect_err("V3 非法策略必须拒绝");
        assert!(
            error.to_string().contains("mid_band_policy"),
            "错误必须点名 mid_band_policy: {error}"
        );
    }

    #[test]
    fn difficulty_v5_rejects_tier_model_id_collision() {
        let vendors = difficulty_vendor("v1", &model_entry("gpt-x", "fast"));
        let error = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"gpt-x\"\nhigh = \"reasoning\"\n",
        )
        .expect_err("V5 档位撞 model_id 必须拒绝");
        assert!(
            error.to_string().contains("gpt-x"),
            "错误必须点名撞名项: {error}"
        );
    }

    #[test]
    fn difficulty_v6_rejects_tier_without_candidate() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n",
        )
        .expect_err("V6 无候选档必须拒绝");
        assert!(
            error.to_string().contains("fast"),
            "错误必须点名档名: {error}"
        );
    }

    #[test]
    fn difficulty_v7_rejects_mid_without_candidate() {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        let error = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]
low = \"fast\"
mid = \"balanced\"
high = \"reasoning\"
",
        )
        .expect_err("V7 mid 无候选必须拒绝");
        assert!(
            error.to_string().contains("balanced"),
            "错误必须点名 mid 档名: {error}"
        );
    }

    #[test]
    fn difficulty_group_collision_rejected() {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"));
        let error = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\ngroups = [\"m1\"]\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"fast\"\n",
        )
        .expect_err("分组名撞 model_id 必须拒绝");
        assert!(
            error.to_string().contains("m1"),
            "错误必须点名分组: {error}"
        );
    }

    #[test]
    fn difficulty_happy_path_three_tiers_resolving() {
        // 两 vendor 分别服务 low/high 抽象名，mid 未配置 = 两档退化形态合法
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        let config = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\ngroups = [\"auto\"]\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n",
        )
        .expect("合法三档（mid 缺省）必须通过");
        assert_eq!(config.routing.difficulty.tiers.low, "fast");
        assert_eq!(config.routing.difficulty.tiers.high, "reasoning");
        assert!(config.routing.difficulty.tiers.mid.is_none());
    }

    #[test]
    fn difficulty_tier_empty_name_rejected() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"\"\nhigh = \"reasoning\"\n",
        )
        .expect_err("空档名必须拒绝");
        assert!(
            error.to_string().contains("low"),
            "错误必须点名档位: {error}"
        );
    }

    #[test]
    fn difficulty_v9_rejects_negative_weights_and_out_of_range_thresholds() {
        // 负权重（NaN/inf 在 confers 解析层即拒，V9 的有限性检查是
        // 结构体直构路径的纵深防御）；阈值越界 [0,1] 同拒
        for toml in [
            "enabled = true\nw_clf = -0.1\nw_rule = 0.4\nscore_low = 0.35\nscore_high = 0.65\n",
            "enabled = true\nw_clf = 0.6\nw_rule = -0.4\nscore_low = 0.35\nscore_high = 0.65\n",
            "enabled = true\nscore_low = 0.35\nscore_high = 1.5\n",
        ] {
            let error =
                parse_difficulty_toml(DIFF_VENDORS_OK, toml).expect_err("V9 非法数值域必须拒绝");
            let message = error.to_string();
            assert!(
                message.contains("w_clf")
                    || message.contains("w_rule")
                    || message.contains("score_high"),
                "错误须点名字段: {message}"
            );
        }
    }

    #[test]
    fn difficulty_v10_rejects_classifier_bounds_and_engine() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n             [routing.difficulty.classifier]\ntimeout_ms = 0\n",
        )
        .expect_err("V10 timeout_ms=0 必须拒绝");
        assert!(
            error.to_string().contains("timeout_ms"),
            "错误须点名 timeout_ms: {error}"
        );
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n             [routing.difficulty.classifier]\nengine = \"tpu\"\n",
        )
        .expect_err("V10 未知 engine 必须拒绝");
        assert!(
            error.to_string().contains("engine"),
            "错误须点名 engine: {error}"
        );
    }

    #[cfg(not(feature = "difficulty-classifier"))]
    #[test]
    fn difficulty_v4_rejects_clf_weight_without_feature() {
        let error = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = true\nw_clf = 0.6\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n",
        )
        .expect_err("V4 无 feature 时 w_clf>0 必须拒绝");
        assert!(
            error.to_string().contains("difficulty-classifier"),
            "错误必须点名 feature: {error}"
        );
    }

    // ---- parse_config_content 错误分支 ----

    fn minimal_ok_toml() -> String {
        "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n\
         [database]\npath = \":memory:\"\n"
            .to_string()
    }

    #[test]
    fn parse_rejects_unknown_extension() {
        let error = parse_config_content(Path::new("config.txt"), &minimal_ok_toml())
            .expect_err("未知扩展名必须拒绝");
        assert!(
            error.to_string().contains("unknown extension"),
            "错误必须说明扩展名不可识别: {error}"
        );
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        assert!(
            parse_config_content(Path::new("config.toml"), "not [ valid").is_err(),
            "TOML 语法错必须拒绝"
        );
    }

    #[test]
    fn parse_rejects_type_mismatched_field() {
        let toml = "vendors = \"not-a-list\"\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n\
                    [database]\npath = \":memory:\"\n";
        assert!(
            parse_config_content(Path::new("config.toml"), toml).is_err(),
            "类型不匹配必须拒绝"
        );
    }

    // ---- locate_data_error 回退分支 ----

    #[test]
    fn locate_data_error_keeps_original_when_file_unreadable() {
        let original = "native error".to_string();
        let located = locate_data_error(&original, Path::new("/nonexistent/dir/cfg.toml"));
        assert_eq!(located, original, "文件不可读必须原样返回原生消息");
    }

    #[test]
    fn locate_data_error_keeps_original_when_toml_parses_clean() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, minimal_ok_toml()).expect("write valid config");
        let original = "native error".to_string();
        let located = locate_data_error(&original, &path);
        assert_eq!(
            located, original,
            "能完整解析的文件与 data error 无关，必须原样返回"
        );
    }

    #[test]
    fn line_column_maps_offset_and_rejects_invalid_boundaries() {
        // 字节布局：a(0) \n(1) b(2) b(3) \n(4) c(5) 中(6..9) c(9)
        let raw = "a\nbb\nc中c";
        assert_eq!(line_column(raw, 0), Some((1, 1)));
        assert_eq!(line_column(raw, 2), Some((2, 1)));
        assert_eq!(line_column(raw, 5), Some((3, 1)));
        // offset 7 落在 '中' 的多字节序列内部 → 非字符边界
        assert_eq!(line_column(raw, 7), None);
        // 末尾越界 clamp 到 len（行 3，行内 5 字节 + 1）
        assert_eq!(line_column(raw, raw.len()), Some((3, 6)));
    }

    // ---- V9 分数越界的直构路径（confers 解析层先拦 TOML 同类值）----

    /// enabled=true 的合法基线（双 vendor 供 low/high 候选），供直构变异用
    fn enabled_ok_config() -> Config {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nw_rule = 0.6\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n",
        )
        .expect("基线配置必须合法")
    }

    #[test]
    fn difficulty_score_range_rejected_on_direct_validate_path() {
        let mut config = enabled_ok_config();
        config.routing.difficulty.score_high = 1.5;
        let error = validate_difficulty(&config).expect_err("越界阈值直构路径必须拒绝");
        assert!(
            error.to_string().contains("score_high"),
            "错误必须点名 score_high: {error}"
        );
    }

    #[test]
    fn difficulty_classifier_failure_threshold_zero_rejected() {
        let mut config = enabled_ok_config();
        config.routing.difficulty.classifier.failure_threshold = 0;
        let error = validate_difficulty(&config).expect_err("failure_threshold=0 必须拒绝");
        assert!(
            error.to_string().contains("failure_threshold"),
            "错误必须点名 failure_threshold: {error}"
        );
    }

    #[test]
    fn difficulty_classifier_timeout_bounds_and_engine_case_insensitive_pass() {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        for classifier in [
            "timeout_ms = 1",
            "timeout_ms = 60000",
            "engine = \"CANDLE\"",
        ] {
            parse_difficulty_toml(
                &vendors,
                &format!(
                    "enabled = true\nw_clf = 0.0\nw_rule = 0.6\nscore_low = 0.35\nscore_high = 0.65\n\
                     [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n\
                     [routing.difficulty.classifier]\n{classifier}\n"
                ),
            )
            .unwrap_or_else(|_| panic!("合法 classifier 值必须放行: {classifier}"));
        }
    }

    #[test]
    fn difficulty_lexicon_rejections_name_the_field() {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        let base = "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
                    [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n";
        let cases: Vec<(&str, String)> = vec![
            ("empty", "hard = [\"\"]".to_string()),
            ("whitespace", "hard = [\" proof \"]".to_string()),
            ("control", "hard = [\"a\\nb\"]".to_string()),
            ("too_long", format!("hard = [\"{}\"]", "x".repeat(65))),
            ("duplicate", "hard = [\"证明\", \"证明\"]".to_string()),
        ];
        for (name, lexicon) in cases {
            let difficulty = format!("{base}\n[routing.difficulty.lexicon]\n{lexicon}\n");
            let error = parse_difficulty_toml(&vendors, &difficulty)
                .expect_err(&format!("{name} 必须拒绝"));
            assert!(
                error.to_string().contains("lexicon.hard"),
                "{name} 错误必须点名 lexicon.hard: {error}"
            );
        }

        let too_many = (0..257)
            .map(|i| format!("\"w{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let difficulty = format!("{base}\n[routing.difficulty.lexicon]\nhard = [{too_many}]\n");
        let error = parse_difficulty_toml(&vendors, &difficulty).expect_err("超条目数必须拒绝");
        assert!(
            error.to_string().contains("lexicon.hard"),
            "错误必须点名字段: {error}"
        );
    }

    #[test]
    fn difficulty_scoring_rejections_name_the_field() {
        let mut config = enabled_ok_config();
        config.routing.difficulty.scoring.w_hard = -0.1;
        let error = validate_difficulty(&config).expect_err("负权重必须拒绝");
        assert!(
            error.to_string().contains("scoring.w_hard"),
            "错误必须点名 scoring.w_hard: {error}"
        );

        let mut config = enabled_ok_config();
        config.routing.difficulty.scoring.hard_cap = 0.0;
        let error = validate_difficulty(&config).expect_err("cap=0 必须拒绝");
        assert!(
            error.to_string().contains("scoring.hard_cap"),
            "错误必须点名 scoring.hard_cap: {error}"
        );

        let mut config = enabled_ok_config();
        config.routing.difficulty.scoring.length_cap = 9000.0;
        let error = validate_difficulty(&config).expect_err("length_cap 超上限必须拒绝");
        assert!(
            error.to_string().contains("scoring.length_cap"),
            "错误必须点名 scoring.length_cap: {error}"
        );
    }

    #[test]
    fn difficulty_custom_lexicon_and_scoring_pass() {
        let vendors = difficulty_vendor("v1", &model_entry("m1", "fast"))
            + &difficulty_vendor("v2", &model_entry("m2", "reasoning"));
        let config = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]\nlow = \"fast\"\nhigh = \"reasoning\"\n\
             [routing.difficulty.lexicon]\nhard = [\"量子\"]\nmath_symbols = [\"§\"]\n\
             [routing.difficulty.scoring]\nw_hard = 0.5\nlength_cap = 1000.0\n",
        )
        .expect("合法自定义词法表与计分必须放行");
        assert_eq!(config.routing.difficulty.lexicon.hard_words().len(), 1);
        assert_eq!(config.routing.difficulty.lexicon.hard_words()[0], "量子");
    }

    #[test]
    fn difficulty_disabled_bypasses_lexicon_and_scoring_validation() {
        let config = parse_difficulty_toml(
            DIFF_VENDORS_OK,
            "enabled = false\nlexicon = { hard = [\"\"] }\nscoring = { hard_cap = 0.0 }\n",
        )
        .expect("disabled 段即使词法表/计分非法也必须旁路");
        assert!(!config.routing.difficulty.enabled);
    }

    // ---- 告警路径（warn-only，断言不拒不 panic）----

    #[test]
    fn quota_window_zero_rejected() {
        let mut config = config_with(true, None);
        config.quota.window_secs = 0;
        let error = validate_config(&config).expect_err("window_secs=0 必须拒绝");
        assert!(
            matches!(error, GateError::Config { .. }),
            "必须是配置类错误: {error}"
        );
    }

    #[test]
    fn quota_without_auth_warning_path_executes() {
        // warn-only 旁路：谓词为真只发告警不拒绝，行为面 = 不 panic 不改配置
        warn_quota_without_auth(&config_with(true, None));
    }

    #[test]
    fn missing_api_key_env_warning_covers_primary_and_pool_lists() {
        let vendors = "[[vendors]]\nid = \"v1\"\nname = \"v1\"\ntype_ = \"openai\"\n\
             api_key_env = \"UNSET_POOL_PRIMARY_XYZ\"\n\
             api_key_envs = [\"UNSET_POOL_A_XYZ\", \"UNSET_POOL_B_XYZ\"]\n\
             rate_limit = { type = \"time_window\", start = \"00:00\", end = \"24:00\" }\n\
             concurrency = { requests_per_second = 1 }\n";
        let config = parse_difficulty_toml(vendors, "").expect("含池名单的 vendor 配置必须合法");
        // warn-only：主 env 与池 env 均缺失只告警不拒绝
        warn_missing_api_key_envs(&config);
    }

    #[test]
    fn identical_tier_warning_with_mid_tier_executes() {
        // 单 vendor 服务全部三档 → 三档候选集相同 → identical 告警分支触发
        let vendors = difficulty_vendor(
            "v1",
            &format!(
                "{}\n{}\n{}",
                model_entry("m1", "fast"),
                model_entry("m2", "balanced"),
                model_entry("m3", "reasoning")
            ),
        );
        let config = parse_difficulty_toml(
            &vendors,
            "enabled = true\nw_clf = 0.0\nscore_low = 0.35\nscore_high = 0.65\n\
             [routing.difficulty.tiers]
low = \"fast\"
mid = \"balanced\"
high = \"reasoning\"
",
        )
        .expect("三档各有候选必须合法（同集仅告警不拒绝）");
        warn_identical_difficulty_tiers(&config);
    }

    // ---- admin secret 校验分支 ----

    fn auth_section(api_keys: Vec<String>, hashes: Vec<String>) -> crate::config::AuthSection {
        use crate::config::{AuthKeyEntry, AuthTier};
        crate::config::AuthSection {
            enabled: true,
            api_keys,
            header: Default::default(),
            keys: hashes
                .into_iter()
                .map(|hash| AuthKeyEntry {
                    hash,
                    tier: AuthTier::Elastic,
                    token_quota: 0,
                    cost_quota_millicents: 0,
                })
                .collect(),
            oidc_rules: None,
        }
    }

    #[test]
    fn admin_secret_shape_rejections_and_digest_acceptance() {
        // 非 64 长度
        let mut config = config_with(true, Some(auth_section(vec!["SHORT".into()], vec![])));
        assert!(
            validate_admin_secrets(&config).is_err(),
            "非 64 长度摘要必须拒绝"
        );
        // 大写 hex（认证侧恒无法匹配）
        *config.auth.as_mut().expect("auth 已设置") = auth_section(vec!["A".repeat(64)], vec![]);
        assert!(
            validate_admin_secrets(&config).is_err(),
            "大写 hex 摘要必须拒绝"
        );
        // 合法 64 小写 hex 放行；[auth] 缺段放行
        let digest = "0123456789abcdef".repeat(4);
        *config.auth.as_mut().expect("auth 已设置") =
            auth_section(vec![digest.clone()], vec![digest]);
        validate_admin_secrets(&config).expect("合法摘要必须放行");
        config.auth = None;
        validate_admin_secrets(&config).expect("缺 [auth] 段必须放行");
    }

    #[test]
    fn admin_secret_provider_keys_lists_inserted_entries() {
        let mut provider = AdminSecretProvider {
            entries: std::collections::HashMap::new(),
        };
        provider.insert("auth.api_keys[0]".to_string(), &"a".repeat(64));
        let keys = provider.keys();
        assert_eq!(
            keys,
            vec!["auth.api_keys[0]".to_string()],
            "keys 必须列出全部条目"
        );
    }

    #[test]
    fn admin_secret_digest_shape_passes_weak_secret_scan() {
        // 形态门放行的 64 位小写 hex 不是 WEAK_SECRETS 命中项，且长度恒达标：
        // 校验器循环对合法摘要必须零违规（防止弱词表误伤摘要形态）
        let config = config_with(true, Some(auth_section(vec!["0".repeat(64)], vec![])));
        validate_admin_secrets(&config).expect("全零 64 位 hex 形态合法，不得被弱词表误拒");
    }

    // ---- 嵌入兼容组校验（R1-R4）----

    /// `[[vendors.models]]` 嵌入条目：compat_group / dimensions 均可选缺席
    fn embedding_model(
        model_id: &str,
        abstract_id: &str,
        group: Option<&str>,
        dimensions: Option<u32>,
    ) -> String {
        let mut block = format!(
            "[[vendors.models]]\nmodel_id = \"{model_id}\"\nabstract_id = \"{abstract_id}\"\n"
        );
        if let Some(group) = group {
            block.push_str(&format!("compat_group = \"{group}\"\n"));
        }
        if let Some(dims) = dimensions {
            block.push_str(&format!("dimensions = {dims}\n"));
        }
        block
    }

    /// 组装含 [[vendors]] 的完整配置并解析（难度段留空）：复用难度用例的
    /// 装配器保持加载口径单一，本组用例只关心嵌入兼容组校验
    fn parse_vendors_toml(vendors: &str) -> Result<Config, GateError> {
        parse_difficulty_toml(vendors, "")
    }

    #[test]
    fn embedding_group_model_id_mismatch_rejected() {
        // R1 + R4：同组 upstream model_id 实质不同 → 拒绝，点名两厂商与各自 model_id
        let vendors = difficulty_vendor(
            "v1",
            &embedding_model("text-embedding-3-large", "emb-a", Some("g1"), None),
        ) + &difficulty_vendor(
            "v2",
            &embedding_model("text-embedding-3-small", "emb-a", Some("g1"), None),
        );
        let error = parse_vendors_toml(&vendors).expect_err("同组异构 model_id 必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("v1") && message.contains("v2"),
            "错误必须点名两个 vendor id: {message}"
        );
        assert!(
            message.contains("text-embedding-3-large")
                && message.contains("text-embedding-3-small"),
            "错误必须点名两个 model_id: {message}"
        );
        assert!(message.contains("g1"), "错误必须点名组名: {message}");
    }

    #[test]
    fn embedding_group_dimension_conflict_rejected() {
        // R2 + R4：同组已声明 dimensions 冲突 → 拒绝，点名冲突条目与两维度值
        let vendors =
            difficulty_vendor("v1", &embedding_model("m", "emb-a", Some("g1"), Some(1024)))
                + &difficulty_vendor("v2", &embedding_model("m", "emb-a", Some("g1"), Some(1536)));
        let error = parse_vendors_toml(&vendors).expect_err("同组维度冲突必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("1024") && message.contains("1536"),
            "错误必须点名两个维度值: {message}"
        );
        assert!(
            message.contains("v1") && message.contains("v2"),
            "错误必须点名冲突条目所属厂商: {message}"
        );
    }

    #[test]
    fn embedding_group_name_cross_abstract_rejected() {
        // R3 + R4：同组名挂两个不同 abstract_id → 拒绝，点名两 abstract
        //（model_id 完全一致以隔离 R1，确保命中的是 R3 分支）
        let vendors = difficulty_vendor(
            "v1",
            &embedding_model("text-embedding-3-large", "emb-a", Some("g1"), None),
        ) + &difficulty_vendor(
            "v2",
            &embedding_model("text-embedding-3-large", "emb-b", Some("g1"), None),
        );
        let error = parse_vendors_toml(&vendors).expect_err("组名跨 abstract 必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("emb-a") && message.contains("emb-b"),
            "错误必须点名两个冲突 abstract: {message}"
        );
    }

    #[test]
    fn embedding_group_name_with_singleton_separator_rejected() {
        // S1/F2：组名含单例键分隔符 "/::" → 拒绝（防与未声明条目的单例键
        // {vendor}/::{model} 撞键击穿安全默认）；错误信息点名组名
        let vendors =
            difficulty_vendor("v1", &embedding_model("m", "emb-a", Some("v2/::m2"), None));
        let error = parse_vendors_toml(&vendors).expect_err("组名含保留分隔符必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("v2/::m2"),
            "错误必须点名违规组名: {message}"
        );
    }

    #[test]
    fn embedding_group_valid_variants_pass() {
        // R4 零破坏 + R1 trim/大小写语义：组名 trim 后精确匹配、model_id 比较
        // 大小写不敏感、组内部分条目未声明 dimensions 不触发拒绝
        let vendors = difficulty_vendor(
            "v1",
            &embedding_model("Text-Embedding-3-Large", "emb-a", Some(" g1 "), Some(1024)),
        ) + &difficulty_vendor(
            "v2",
            &embedding_model("text-embedding-3-large", "emb-a", Some("g1"), None),
        );
        let config = parse_vendors_toml(&vendors).expect("大小写差异 + 部分维度声明必须通过");
        assert_eq!(config.vendors.len(), 2);

        // 不同组名各自服务不同 abstract（组名不跨 abstract 的合法对照）
        let distinct = difficulty_vendor("v1", &embedding_model("m1", "emb-a", Some("ga"), None))
            + &difficulty_vendor("v2", &embedding_model("m2", "emb-b", Some("gb"), None));
        parse_vendors_toml(&distinct).expect("不同组名不同 abstract 必须通过");
    }

    // ---- model_type 校验 ----

    /// `[[vendors.models]]` model_type 条目（None = 缺省不写）
    fn typed_model(model_id: &str, model_type: Option<&str>) -> String {
        let mut block = format!(
            "[[vendors.models]]\nmodel_id = \"{model_id}\"\nabstract_id = \"{model_id}\"\n"
        );
        if let Some(model_type) = model_type {
            block.push_str(&format!("model_type = \"{model_type}\"\n"));
        }
        block
    }

    #[test]
    fn model_type_valid_values_pass() {
        for value in ["chat", "embedding", "rerank"] {
            let vendors = difficulty_vendor("v1", &typed_model("m", Some(value)));
            parse_vendors_toml(&vendors)
                .unwrap_or_else(|e| panic!("合法 model_type={value} 必须通过: {e}"));
        }
    }

    #[test]
    fn model_type_unknown_value_rejected() {
        let vendors = difficulty_vendor("v1", &typed_model("m", Some("reranker")));
        let error = parse_vendors_toml(&vendors).expect_err("未知 model_type 必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("reranker"),
            "错误必须点名非法值: {message}"
        );
        assert!(message.contains("v1"), "错误必须点名 vendor: {message}");
        assert!(
            message.contains("model_type"),
            "错误必须点名字段: {message}"
        );
    }

    #[test]
    fn model_type_absent_and_blank_are_untyped() {
        let absent = difficulty_vendor("v1", &typed_model("m", None));
        parse_vendors_toml(&absent).expect("缺省 model_type 必须通过");

        let blank = difficulty_vendor("v1", &typed_model("m", Some("  ")));
        parse_vendors_toml(&blank).expect("空白 model_type 等价未声明");
    }

    #[test]
    fn rerank_vendor_requires_explicit_api_base() {
        let vendors = r#"
[[vendors]]
id = "r1"
name = "R"
type_ = "rerank"
api_key_env = "R_KEY"  # pragma: allowlist secret（测试环境变量名，非凭证）
rate_limit = { type = "time_window", start = "00:00", end = "24:00" }
concurrency = { requests_per_second = 1 }

[[vendors.models]]
model_id = "rerank-v3"
model_type = "rerank"
"#;
        let error = parse_vendors_toml(vendors).expect_err("rerank 空 api_base 必须拒绝");
        assert!(
            error.to_string().contains("api_base"),
            "错误必须点名 api_base: {error}"
        );
    }

    #[test]
    fn rerank_score_normalization_validates_whitelist() {
        let vendors = difficulty_vendor("v1", &typed_model("m", None));
        for mode in ["none", "sigmoid", "minmax", ""] {
            let mut config = parse_vendors_toml(&vendors).expect("基础配置");
            config.routing.rerank_score_normalization = mode.into();
            validate_config(&config).unwrap_or_else(|e| panic!("mode={mode:?} 必须通过: {e}"));
        }

        let mut config = parse_vendors_toml(&vendors).expect("基础配置");
        config.routing.rerank_score_normalization = "softmax".into();
        let error = validate_config(&config).expect_err("未知模式必须拒绝");
        assert!(
            error.to_string().contains("softmax"),
            "错误必须点名非法值: {error}"
        );
    }

    /// `[responses]` 边界：下限/上限通过，三类越界均拒绝并点名字段。
    #[test]
    fn responses_section_bounds_are_validated() {
        let vendors = difficulty_vendor("v1", &typed_model("m", None));
        let base = parse_vendors_toml(&vendors).expect("基础配置");

        let mut valid = base.clone();
        valid.responses.retention_hours = 1;
        valid.responses.max_response_bytes = 1024;
        valid.responses.max_background_jobs = 1;
        validate_config(&valid).expect("下限值必须通过");
        valid.responses.retention_hours = 8760;
        valid.responses.max_response_bytes = 16 * 1024 * 1024;
        valid.responses.max_background_jobs = 256;
        validate_config(&valid).expect("上限值必须通过");

        let mut zero_retention = base.clone();
        zero_retention.responses.retention_hours = 0;
        let error = validate_config(&zero_retention).expect_err("retention=0 必须拒绝");
        assert!(
            error.to_string().contains("retention_hours"),
            "错误必须点名字段: {error}"
        );

        let mut small_bytes = base.clone();
        small_bytes.responses.max_response_bytes = 1023;
        let error = validate_config(&small_bytes).expect_err("bytes=1023 必须拒绝");
        assert!(
            error.to_string().contains("max_response_bytes"),
            "错误必须点名字段: {error}"
        );

        let mut too_many_jobs = base;
        too_many_jobs.responses.max_background_jobs = 257;
        let error = validate_config(&too_many_jobs).expect_err("jobs=257 必须拒绝");
        assert!(
            error.to_string().contains("max_background_jobs"),
            "错误必须点名字段: {error}"
        );
    }

    /// `[metrics]` 边界：下限/上限通过，两类越界均拒绝并点名字段。
    #[test]
    fn metrics_section_bounds_are_validated() {
        let vendors = difficulty_vendor("v1", &typed_model("m", None));
        let base = parse_vendors_toml(&vendors).expect("基础配置");

        let mut valid = base.clone();
        valid.metrics.cost_restore_window_hours = 1;
        validate_config(&valid).expect("下限值必须通过");
        valid.metrics.cost_restore_window_hours = 8760;
        validate_config(&valid).expect("上限值必须通过");

        let mut zero_window = base.clone();
        zero_window.metrics.cost_restore_window_hours = 0;
        let error = validate_config(&zero_window).expect_err("window=0 必须拒绝");
        assert!(
            error.to_string().contains("cost_restore_window_hours"),
            "错误必须点名字段: {error}"
        );

        let mut huge_window = base;
        huge_window.metrics.cost_restore_window_hours = 8761;
        let error = validate_config(&huge_window).expect_err("window=8761 必须拒绝");
        assert!(
            error.to_string().contains("cost_restore_window_hours"),
            "错误必须点名字段: {error}"
        );
    }

    /// `api_style` 白名单、responses 仅限 openai 型、与 chat_path 互斥。
    #[test]
    fn vendor_api_style_is_validated() {
        fn vendor_with_style(type_: &str, style: Option<&str>, chat_path: Option<&str>) -> String {
            let mut block = format!(
                "[[vendors]]\nid = \"v1\"\nname = \"v1\"\ntype_ = \"{type_}\"\n\
                 api_key_env = \"V_KEY\"\n\
                 rate_limit = {{ type = \"time_window\", start = \"00:00\", end = \"24:00\" }}\n\
                 concurrency = {{ requests_per_second = 1 }}\n"
            );
            if let Some(style) = style {
                block.push_str(&format!("api_style = \"{style}\"\n"));
            }
            if let Some(path) = chat_path {
                block.push_str(&format!("chat_path = \"{path}\"\n"));
            }
            block.push_str(&typed_model("m", None));
            block
        }

        for (type_, style, chat_path) in [
            ("openai", None, None),
            ("openai", Some("chat"), None),
            ("openai", Some("RESPONSES"), None),
            ("openai", Some(""), None),
        ] {
            let vendors = vendor_with_style(type_, style, chat_path);
            parse_vendors_toml(&vendors)
                .unwrap_or_else(|e| panic!("合法 api_style={style:?} 必须通过: {e}"));
        }

        let unknown = vendor_with_style("openai", Some("grpc"), None);
        let error = parse_vendors_toml(&unknown).expect_err("未知 api_style 必须拒绝");
        assert!(
            error.to_string().contains("grpc"),
            "错误必须点名非法值: {error}"
        );

        let wrong_type = vendor_with_style("anthropic", Some("responses"), None);
        let error = parse_vendors_toml(&wrong_type).expect_err("非 openai 型必须拒绝");
        assert!(
            error.to_string().contains("openai"),
            "错误必须说明需要 openai 型: {error}"
        );

        let conflict = vendor_with_style("openai", Some("responses"), Some("/chat/completions"));
        let error = parse_vendors_toml(&conflict).expect_err("chat_path 互斥必须拒绝");
        assert!(
            error.to_string().contains("chat_path"),
            "错误必须点名冲突字段: {error}"
        );

        // gemini 型：chat_path 静态覆盖与 model+action 动态端点冲突，加载期拒绝
        let gemini = vendor_with_style("gemini", None, None);
        parse_vendors_toml(&gemini).expect("gemini 无 chat_path 必须通过");
        let gemini_style = vendor_with_style("gemini", Some("chat"), None);
        parse_vendors_toml(&gemini_style).expect("gemini + api_style=chat 必须通过");
        let gemini_path = vendor_with_style("gemini", None, Some("/v1beta/models/x"));
        let error = parse_vendors_toml(&gemini_path).expect_err("gemini + chat_path 必须拒绝");
        assert!(
            error.to_string().contains("chat_path"),
            "错误必须点名 chat_path: {error}"
        );
    }
    /// masking 台账边界校验：retention_days 与 max_total_bytes 的负路径。
    #[test]
    fn masking_ledger_bounds_are_validated() {
        let mut config = config_with(true, None);
        for bad_retention in [0u32, 3651, 9_999] {
            config.masking.ledger.retention_days = bad_retention;
            assert!(
                validate_masking(&config).is_err(),
                "retention_days={bad_retention} 必须拒绝"
            );
        }
        config.masking.ledger.retention_days = 30;
        config.masking.ledger.max_total_bytes = 4095;
        assert!(
            validate_masking(&config).is_err(),
            "max_total_bytes<4096 必须拒绝"
        );
        config.masking.ledger.max_total_bytes = 4096;
        validate_masking(&config).expect("合法边界必须通过");
    }

    /// Rust 手构 Default 与 serde 缺省单源：KeyBudgetRule / OidcRuleConfig。
    #[test]
    fn hand_built_defaults_match_documented_values() {
        let kb = crate::config::KeyBudgetRule::default();
        assert_eq!(kb.window, "daily");
        assert!(!kb.fail_open, "fail_open 缺省必须收紧（false）");
        assert_eq!(kb.cost_limit_millicents, 0);
        let oidc = crate::config::OidcRuleConfig::default();
        assert_eq!(oidc.ttl_secs, 3600, "缺省 TTL 1h");
        assert!(oidc.claims.is_empty());
    }

    /// otlp.tracing 字段域校验：ratio/protocol/timeout 越界值逐项拒绝。
    #[test]
    fn otlp_tracing_field_ranges_are_validated() {
        let mut config = config_with(true, None);
        config.otlp.tracing.enabled = true;
        config.otlp.tracing.endpoint = "http://localhost:4318/v1/traces".into();
        validate_otlp(&config).expect("合法基线必须通过");

        config.otlp.tracing.sample_ratio = 1.5;
        assert!(validate_otlp(&config).is_err(), "ratio>1 必须拒绝");
        config.otlp.tracing.sample_ratio = 0.5;

        config.otlp.tracing.protocol = "tcp".into();
        assert!(validate_otlp(&config).is_err(), "非 http/grpc 协议必须拒绝");
        config.otlp.tracing.protocol = "grpc".into();

        config.otlp.tracing.timeout_secs = 301;
        assert!(validate_otlp(&config).is_err(), "timeout>300 必须拒绝");
        config.otlp.tracing.timeout_secs = 10;
        validate_otlp(&config).expect("恢复合法后必须通过");
    }

    /// otlp endpoint 形态校验：scheme/userinfo/空 authority 逐项拒绝。
    #[test]
    fn otlp_endpoint_rejects_bad_shapes() {
        for bad in [
            "ftp://collector:4318/v1/traces",
            "http://user:pass@collector:4318/v1/traces",
            "http:///v1/traces",
        ] {
            assert!(
                validate_otlp_endpoint(bad).is_err(),
                "endpoint `{bad}` 必须拒绝"
            );
        }
        validate_otlp_endpoint("http://collector:4318/v1/traces").expect("合法端点通过");
    }

    /// vendor 预算行校验：空 vendor_id 拒绝（trim 后）。
    #[test]
    fn vendor_budget_rejects_blank_vendor_id() {
        let mut config = config_with(true, None);
        config.vendor_budgets = vec![crate::config::VendorBudgetConfig {
            vendor_id: "   ".into(),
            tokens_limit: 100,
            period: "rolling".into(),
            rolling_secs: 3600,
            ..Default::default()
        }];
        assert!(
            validate_vendor_budgets(&config).is_err(),
            "空白 vendor_id 必须拒绝"
        );
    }

    // ---- 覆盖补测：模式机器码 / OIDC / affinity / catalog_sync ----

    /// 两个模式枚举的机器码全量映射（指标标签与解释块同口径）。
    #[test]
    fn mode_machine_codes_cover_all_variants() {
        use crate::config::{AffinityMode, QuotaFactorMode};
        for (mode, code) in [
            (AffinityMode::Legacy, "legacy"),
            (AffinityMode::Auto, "auto"),
            (AffinityMode::Turn, "turn"),
            (AffinityMode::Off, "off"),
        ] {
            assert_eq!(mode.as_str(), code);
        }
        for (mode, code) in [
            (QuotaFactorMode::Off, "off"),
            (QuotaFactorMode::Smart, "smart"),
            (QuotaFactorMode::Pace, "pace"),
        ] {
            assert_eq!(mode.as_str(), code);
        }
    }

    fn oidc_rule(issuer: &str, audience: &str) -> crate::config::OidcRuleConfig {
        crate::config::OidcRuleConfig {
            issuer: issuer.into(),
            audience: audience.into(),
            claims: std::collections::HashMap::new(),
            bind_key_hash: "a".repeat(64),
            ttl_secs: 3600,
        }
    }

    fn oidc_config(rules: Vec<crate::config::OidcRuleConfig>) -> Config {
        let digest = "a".repeat(64);
        let mut auth = auth_section(vec![digest], vec![]);
        auth.oidc_rules = Some(crate::config::OidcRulesSection { rules });
        config_with(false, Some(auth))
    }

    /// OIDC 规则逐类非法形态点名拒绝；合法规则通过。
    #[test]
    fn oidc_rules_validation_rejects_each_invalid_shape() {
        let error = validate_oidc_rules(&oidc_config(vec![oidc_rule("issuer.example.com", "aud")]))
            .expect_err("缺 scheme:// 的 issuer 必须拒绝");
        assert!(error.to_string().contains("issuer.example.com"), "{error}");

        let mut rule = oidc_rule("https://issuer.example.com", "aud");
        rule.audience = "   ".into();
        let error =
            validate_oidc_rules(&oidc_config(vec![rule])).expect_err("空白 audience 必须拒绝");
        assert!(error.to_string().contains("audience"), "{error}");

        let mut rule = oidc_rule("https://issuer.example.com", "aud");
        rule.claims.insert("  ".into(), "v".into());
        let error =
            validate_oidc_rules(&oidc_config(vec![rule])).expect_err("空白 claim 键必须拒绝");
        assert!(error.to_string().contains("claims"), "{error}");

        let mut rule = oidc_rule("https://issuer.example.com", "aud");
        rule.claims.insert("sub".into(), "repo:*".into());
        let error =
            validate_oidc_rules(&oidc_config(vec![rule])).expect_err("claim 通配值必须拒绝");
        assert!(error.to_string().contains("claims"), "{error}");

        let duplicate = vec![
            oidc_rule("https://issuer.example.com", "aud"),
            oidc_rule("https://issuer.example.com", "aud"),
        ];
        let error = validate_oidc_rules(&oidc_config(duplicate))
            .expect_err("同 issuer+audience 重复规则必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("https://issuer.example.com") && message.contains("aud"),
            "{message}"
        );

        validate_oidc_rules(&oidc_config(vec![oidc_rule(
            "https://issuer.example.com",
            "aud",
        )]))
        .expect("合法规则必须通过");
    }

    /// OTLP 端点缺 scheme:// 分隔符 → 非法 URL（与空 authority 不同分支）。
    #[test]
    fn otlp_endpoint_requires_scheme_separator() {
        let error =
            validate_otlp_endpoint("collector:4318/v1/traces").expect_err("缺 scheme:// 必须拒绝");
        assert!(error.to_string().contains("collector"), "{error}");
    }

    /// affinity 会话头名 trim + 小写归一后判空；空白元素拒绝。
    #[test]
    fn affinity_session_headers_normalize_and_reject_blank() {
        let mut config = config_with(false, None);
        config.routing.affinity.session_headers =
            Some(vec![" X-Session-ID ".into(), "X-Conversation-Id".into()]);
        validate_affinity(&config).expect("非空头名归一后必须通过");

        config.routing.affinity.session_headers = Some(vec!["  ".into()]);
        let error = validate_affinity(&config).expect_err("空白头名必须拒绝");
        assert!(error.to_string().contains("session_headers"), "{error}");
    }

    /// catalog_sync 校验：timeout 零、URL 缺 scheme、userinfo、alias 空白逐项拒绝；
    /// loopback http 放行。
    #[test]
    fn catalog_sync_validation_rejects_each_invalid_shape() {
        let mut config = config_with(false, None);
        config.catalog_sync.enabled = true;
        config.catalog_sync.timeout_secs = 0;
        let error = validate_catalog_sync(&config).expect_err("enabled 且 timeout=0 必须拒绝");
        assert!(error.to_string().contains("timeout_secs"), "{error}");

        config.catalog_sync.timeout_secs = 10;
        config.catalog_sync.source_url = "models.dev/api.json".into();
        let error = validate_catalog_sync(&config).expect_err("缺 scheme:// 的 URL 必须拒绝");
        assert!(error.to_string().contains("source_url"), "{error}");

        config.catalog_sync.source_url = "http://localhost:8080/api.json".into();
        validate_catalog_sync(&config).expect("loopback http 放行");

        config.catalog_sync.source_url = "https://user:pass@models.dev/api.json".into();
        let error = validate_catalog_sync(&config).expect_err("userinfo 必须拒绝");
        assert!(error.to_string().contains("userinfo"), "{error}");

        config.catalog_sync.source_url = "https://models.dev/api.json".into();
        config.catalog_sync.alias.insert("   ".into(), "v".into());
        let error = validate_catalog_sync(&config).expect_err("空白 alias 键必须拒绝");
        assert!(error.to_string().contains("alias"), "{error}");
    }

    /// calendar 周期携带 rolling_secs 属两形态混用（与 rolling 带 calendar 对称）。
    #[test]
    fn quota_period_rejects_calendar_with_rolling_secs() {
        let error = validate_quota_period("quota_window", "w1", "calendar", 3600, "daily", 0)
            .expect_err("calendar + rolling_secs 必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("w1") && message.contains("quota_window"),
            "{message}"
        );
    }

    /// 兼容组锚点后置声明：首条目未声明 dimensions/abstract_id 时，后续条目
    /// 的声明就地补齐锚点（R2/R3 的合法对照面）。
    #[test]
    fn embedding_group_late_declarations_seed_dimension_and_abstract_anchors() {
        let vendors = difficulty_vendor(
            "v1",
            "[[vendors.models]]\nmodel_id = \"text-embedding-3-large\"\ncompat_group = \"g1\"\n",
        ) + &difficulty_vendor(
            "v2",
            &embedding_model("text-embedding-3-large", "emb-a", Some("g1"), Some(1024)),
        );
        let config =
            parse_vendors_toml(&vendors).expect("后置的 dimensions/abstract 声明不得被拒绝");
        assert_eq!(config.vendors.len(), 2);
    }
}
