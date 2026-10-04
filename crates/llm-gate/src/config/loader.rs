// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, ConfigManager};
use crate::error::GateError;
use crate::i18n::t_args;
use confers::types::{AnnotatedValue, ConfigValue};
use std::path::Path;

const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// 把 confers 的 AnnotatedValue 树剥壳成裸 serde_json 值。
///
/// 手写原因：ConfigValue 未实现 serde Deserializer（confers/src/types.rs 仅
/// 提供 Serialize，且 Array/Map 的元素是带 source/priority/location 元数据的
/// AnnotatedValue 结构体序列化），无法直接反序列化为强类型；递归剥壳只保留
/// `.inner` 裸值。这是 confers 当前版本的结构性缺口，非重复造轮子。
fn annotated_to_json(v: &AnnotatedValue) -> serde_json::Value {
    config_value_to_json(&v.inner)
}

fn config_value_to_json(v: &ConfigValue) -> serde_json::Value {
    match v {
        ConfigValue::Null => serde_json::Value::Null,
        ConfigValue::Bool(b) => (*b).into(),
        ConfigValue::I64(i) => (*i).into(),
        ConfigValue::U64(u) => (*u).into(),
        ConfigValue::F64(f) => (*f).into(),
        ConfigValue::String(s) => s.as_str().into(),
        ConfigValue::Bytes(b) => serde_json::to_value(b).unwrap_or(serde_json::Value::Null),
        ConfigValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(annotated_to_json).collect())
        }
        ConfigValue::Map(m) => {
            let mut map = serde_json::Map::new();
            for (k, av) in m.iter() {
                map.insert(k.to_string(), annotated_to_json(av));
            }
            serde_json::Value::Object(map)
        }
    }
}

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
        let json = config_value_to_json(&annotated.inner);
        let config: Config = serde_json::from_value(json).map_err(|e| GateError::Config {
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
/// vendor/model id 去重是跨 Vec 条目的唯一性约束，confers::validator 的
/// ValidationRule 仅支持字段级规则（MinLength/Range/Email 等，见
/// confers/src/impl_/validator.rs:42-62），无法表达，故保留手写。
pub fn validate_config(config: &Config) -> Result<(), GateError> {
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
