// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, ConfigManager};
use crate::error::GateError;
use crate::i18n::t_args;
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

        // confers::loader::load_file：路径防护（symlink 穿越/大小限制/格式探测，
        // confers/src/impl_/loader.rs:578）。LoaderConfig 默认拒绝绝对路径
        // （防不可信输入穿越）；此处路径来自操作者显式 CLI 参数（可信输入），
        // 放行绝对路径，symlink 检查与 max_size 防护保留默认开启。
        let loader_config = confers::loader::LoaderConfig::new().allow_absolute();
        let annotated =
            confers::loader::load_file(path, &loader_config).map_err(|e| GateError::Config {
                message: t_args("config-load-failed", &[("error", e.to_string())]),
            })?;
        // 强类型投影走 confers AnnotatedValue::to_typed（取代手写递归剥壳）：
        // 元数据丢弃、Bytes→base64、非有限 F64→null 语义与 to_json 一致
        // （confers/src/types.rs:930-942）。TOML 解析链不产生 Bytes 变体
        // （confers/src/impl_/convert.rs 的 toml_value_to_config_value 无
        // Bytes 分支），base64 语义对 TOML 加载路径不可观察。错误有意脱敏：
        // 只报字段路径不回显出错值（配置值可能是密钥原文）。
        let config: Config = annotated.to_typed().map_err(|e| GateError::Config {
            message: t_args("config-parse-failed", &[("error", e.to_string())]),
        })?;

        validate_config(&config)?;

        let manager = ConfigManager::new(config);
        // 路径记忆：update_config 写回同一路径，消灭硬编码 CWD "config.toml"
        manager.set_path(path.to_path_buf());
        Ok(manager)
    }
}

/// 校验配置 — 加载路径（ConfigLoader::load）与更新路径
/// （ConfigManager::update_config）共用。
///
/// 管理面弱密钥校验委托 confers security-rules 的 JwtSecretValidator 规则
/// 内核（[`validate_admin_secrets`]）；vendor/model id 去重是跨 Vec 条目的
/// 唯一性约束，confers::validator 的 ValidationRule 仅支持字段级规则
///（MinLength/Range/Email 等，见 confers/src/impl_/validator.rs:42-62），
/// 无法表达，故保留手写。
pub fn validate_config(config: &Config) -> Result<(), GateError> {
    validate_admin_secrets(config)?;

    let mut vendor_ids = std::collections::HashSet::new();
    for vendor in &config.vendors {
        if !vendor_ids.insert(&vendor.id) {
            return Err(GateError::Config {
                message: t_args(
                    "config-duplicate-vendor-id",
                    &[("vendor", vendor.id.clone())],
                ),
            });
        }
    }

    for vendor in &config.vendors {
        let mut model_ids = std::collections::HashSet::new();
        for model in &vendor.models {
            if !model_ids.insert(&model.model_id) {
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

    // 安全 L4 脱敏：缺失告警不回显配置原文（用户可能误将 key 原文当作
    // env 名填入配置，回显会把密钥泄进日志）——只报值长度/序号 + 核对提示。
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
        // T005：key 池显式列表逐名校验（缺失只告警不拒——池解析时跳过
        // 缺失名并显性化，与 api_key_env 同一宽容级别）
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

    // T006：[[model_catalog]] 段校验 —— model_id 去重（与 vendor/model id
    // 同级唯一性约束，confers 字段级规则无法表达，保留手写）+ 登记条目
    // context_window > 0（0 会让 agentstem TUI auto-compact 静默不装配）。
    // 加载与 POST /admin/config 热更新共用本函数，两路同拒。
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
        // 审查 LOW-1：价格有限（拒 NaN/Inf）且 ≥ 0——NaN 的比较恒 false、
        // 负价是配置错误，两路同拒；T007 计价按此面消费
        let prices = [
            ("input_price", entry.input_price),
            ("output_price", entry.output_price),
            ("cache_read_price", entry.cache_read_price),
            ("cache_creation_price", entry.cache_creation_price),
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
    }

    // 窗口 <100ms 使 CB Open→HalfOpen 即刻转换：故障期退化为探针风暴，
    // 等于关闭熔断冷却。加载与 POST /admin/config 热更新共用本函数，两路同拒。
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

/// [auth] 段条目的 ConfigProvider 薄适配 — 把管理面密钥逐条挂到合成键
/// （`auth.api_keys[{index}]` / `auth.keys[{index}].hash`）上，供
/// JwtSecretValidator 按标量键读取（其规则内核只消费单个 string 键，见
/// confers/src/security/rules/jwt.rs 的 with_secret_key）。
struct AdminSecretProvider {
    entries: HashMap<String, AnnotatedValue>,
}

impl AdminSecretProvider {
    fn insert(&mut self, key: String, secret: &str) {
        let annotated = AnnotatedValue::new(
            ConfigValue::string(secret),
            SourceId::new("llm-gate-config"),
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

/// 管理面密钥启动校验 — `[auth].api_keys` 与 `[[auth.keys]].hash`
/// 的配置契约是 SHA256 hex 摘要（64 个小写 hex 字符，api_keys 存储
/// 约定见 config/mod.rs），/admin/* 管理路由与数据面共用同一认证
/// 中间件，违约条目即无效或弱管理面凭证。
///
/// 两层校验：
/// ① 形态契约（应用层业务约定）：长度 64 且全为小写 hex 字符（认证侧
///    sha256_hex 经 hex::encode 恒输出小写，见 src 侧 auth 中间件，故
///    契约按小写对齐）。认证对传入 key 现场算摘要后与存储值逐字节
///    比对，非小写 hex-64 条目（含大写摘要——certutil/Get-FileHash 的
///    真实输出形态）永远无法认证成功，属纯配置错误；confers 长度/
///    弱词表拦不住「≥32 字节长口令误填当摘要」「大写摘要」这两类
///    误填，由本层强制（FTL 宣称的 hex 契约以此为实现）。
/// ② confers JwtSecretValidator 规则内核（最小长度 + 弱词表，词表不
///    复制进本仓库；不接 CORS 校验器——其三键缺失语义会稳定误报），
///    每条目一次 `with_secret_key` 适配标量读取形状。对已过形态关的
///    条目当前无增量命中（64 小写 hex 必然 ≥32 字节且不可能是弱词表
///    任何词）。继承边界：只有 Critical 级违规升级为拒绝启动，非
///    Critical 级留 debug 日志痕迹不拦截——规则内核后续增强新增
///    Critical 违规时本仓库自动继承拒绝行为。
///
/// 条目不达标 → 与其余配置校验同 fail-fast：加载拒绝启动、
/// POST /admin/config 拒绝热更，防止误填明文在重启用时静默生效。
/// `[auth]` 缺段或列表为空 = 无密钥可校验，直接放行（认证禁用是既有
/// 合法形态；校验不看 enabled——条目落盘即按契约校验，与开关状态
/// 无关）。错误明细按配置索引固定顺序拼装（api_keys 升序 → keys
/// 升序；HashMap 随机序会让同一坏配置的重启日志文案漂移，妨碍日志
/// 比对与告警去重），只报条目位置与规则结论，不回显条目原文（脱敏
/// 先例同 env 告警）。
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
        // 小写 hex-64：与认证侧 sha256_hex 输出形态逐位对齐（大写摘要
        // 恒无法认证，放行只会把全量 401 推迟到重启后）。谓词必须是
        // 「hex 字符且非大写」——单纯小写检查会放过非 hex 小写串
        // （如 64 个 'x' 的长口令）
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
                        target: "llm_gate::config",
                        key = %key,
                        severity = ?violation.severity,
                        "non-critical admin secret rule violation: {}",
                        violation.message
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
