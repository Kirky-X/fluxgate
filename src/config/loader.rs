// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, ConfigManager};
use crate::error::ProxyError;
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

    pub fn load(&self) -> Result<ConfigManager, ProxyError> {
        let path = Path::new(&self.path);

        if !path.exists() {
            return Err(ProxyError::Config {
                message: format!("配置文件不存在: {}", self.path),
            });
        }

        let content = std::fs::read_to_string(path).map_err(|e| ProxyError::Config {
            message: format!("读取配置文件失败: {}", e),
        })?;

        let config: Config = toml::from_str(&content).map_err(|e| ProxyError::Config {
            message: format!("解析配置文件失败: {}", e),
        })?;

        self.validate(&config)?;

        Ok(ConfigManager::new(config))
    }

    fn validate(&self, config: &Config) -> Result<(), ProxyError> {
        let mut vendor_ids = std::collections::HashSet::new();
        for vendor in &config.vendors {
            if !vendor_ids.insert(&vendor.id) {
                return Err(ProxyError::Config {
                    message: format!("厂商 ID 重复: {}", vendor.id),
                });
            }
        }

        for vendor in &config.vendors {
            let mut model_ids = std::collections::HashSet::new();
            for model in &vendor.models {
                if !model_ids.insert(&model.model_id) {
                    return Err(ProxyError::Config {
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
}
