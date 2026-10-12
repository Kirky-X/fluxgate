// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, ConfigManager};
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

/// 校验配置 — 加载路径与更新路径共用；管理面弱密钥委托 confers
/// security-rules，跨 Vec 条目的唯一性约束保留手写（字段级规则无法表达）
pub fn validate_config(config: &Config) -> Result<(), GateError> {
    validate_admin_secrets(config)?;
    validate_vendor_entries(config)?;
    warn_missing_api_key_envs(config);
    validate_model_catalog(config)?;
    validate_resilience_circuit(config)?;
    validate_quota_window(config)?;
    warn_quota_without_auth(config);
    Ok(())
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

/// [auth] 段条目的 ConfigProvider 薄适配：管理面密钥逐条挂到合成键上，
/// 供 JwtSecretValidator 按标量键读取
struct AdminSecretProvider {
    entries: HashMap<String, AnnotatedValue>,
}

impl AdminSecretProvider {
    fn insert(&mut self, key: String, secret: &str) {
        let annotated = AnnotatedValue::new(
            ConfigValue::string(secret),
            SourceId::new("nexusflow-config"),
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
                        target: "nexusflow::config",
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
            rate_limit: Default::default(),
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
        };
        let auth_off = AuthSection {
            enabled: false,
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
}
