// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, ConfigManager};
use crate::error::NexusFlowError;
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

    pub fn load(&self) -> Result<ConfigManager, NexusFlowError> {
        let path = Path::new(&self.path);

        if !path.exists() {
            return Err(NexusFlowError::Config {
                message: format!("配置文件不存在: {}", self.path),
            });
        }

        // confers::loader::load_file：路径防护（symlink 穿越/大小限制/格式探测，
        // confers/src/impl_/loader.rs:578）。LoaderConfig 默认拒绝绝对路径
        // （防不可信输入穿越）；此处路径来自操作者显式 CLI 参数（可信输入），
        // 放行绝对路径，symlink 检查与 max_size 防护保留默认开启。
        let loader_config = confers::loader::LoaderConfig::new().allow_absolute();
        let annotated = confers::loader::load_file(path, &loader_config).map_err(|e| {
            NexusFlowError::Config {
                message: format!("加载配置文件失败: {}", e),
            }
        })?;
        let json = config_value_to_json(&annotated.inner);
        let config: Config = serde_json::from_value(json).map_err(|e| NexusFlowError::Config {
            message: format!("解析配置文件失败: {}", e),
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
pub fn validate_config(config: &Config) -> Result<(), NexusFlowError> {
    let mut vendor_ids = std::collections::HashSet::new();
    for vendor in &config.vendors {
        if !vendor_ids.insert(&vendor.id) {
            return Err(NexusFlowError::Config {
                message: format!("厂商 ID 重复: {}", vendor.id),
            });
        }
    }

    for vendor in &config.vendors {
        let mut model_ids = std::collections::HashSet::new();
        for model in &vendor.models {
            if !model_ids.insert(&model.model_id) {
                return Err(NexusFlowError::Config {
                    message: format!("厂商 {} 内的模型 ID 重复: {}", vendor.id, model.model_id),
                });
            }
        }
    }

    for vendor in &config.vendors {
        if std::env::var(&vendor.api_key_env).is_err() {
            tracing::warn!(
                "厂商 {} 的 API Key 环境变量 {} 未设置",
                vendor.id,
                vendor.api_key_env
            );
        }
    }

    Ok(())
}
