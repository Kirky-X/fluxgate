// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, RwLock};
use crate::error::ProxyError;

pub mod loader;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "0.0.0.0".into()
}

fn default_port() -> u16 {
    8080
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
}

fn default_db_path() -> String {
    "./data/llm_proxy.db".into()
}

fn default_pool_size() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VendorConfig {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    pub type_: String,
    pub api_key_env: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_vendor_enabled")]
    pub enabled: bool,
    #[serde(default = "Vec::new")]
    pub models: Vec<ModelConfig>,
    #[allow(dead_code)]
    pub rate_limit: RateLimitConfig,
    pub concurrency: ConcurrencyConfig,
}

fn default_api_base() -> String {
    String::new()
}

fn default_vendor_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    pub model_id: String,
    #[serde(default)]
    pub abstract_id: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_model_enabled")]
    pub enabled: bool,
}

fn default_weight() -> f64 {
    1.0
}

fn default_model_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum RateLimitConfig {
    #[serde(rename = "time_window")]
    TimeWindow {
        #[allow(dead_code)]
        start: String,
        #[allow(dead_code)]
        end: String,
    },
    #[serde(rename = "interval")]
    Interval {
        #[allow(dead_code)]
        hours: u64,
    },
    #[serde(rename = "token")]
    Token {
        #[allow(dead_code)]
        max_tokens: u64,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConcurrencyConfig {
    #[serde(default = "default_rps")]
    pub requests_per_second: u32,
}

fn default_rps() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GlobalModelConfig {
    #[serde(default = "default_global_enabled")]
    #[allow(dead_code)]
    pub enabled: bool,
    #[serde(default = "default_strategy")]
    pub strategy: String,
}

fn default_global_enabled() -> bool {
    true
}

fn default_strategy() -> String {
    "weighted".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub vendors: Vec<VendorConfig>,
    #[allow(dead_code)]
    pub global_model: Option<GlobalModelConfig>,
}

#[derive(Clone)]
pub struct ConfigManager {
    config: Arc<RwLock<Config>>,
}

impl ConfigManager {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
        }
    }

    pub fn read(&self) -> impl Deref<Target = Config> + '_ {
        self.config.read().unwrap()
    }
    
    pub fn get_config(&self) -> Config {
        self.read().clone()
    }
    
    pub fn update_config(&self, new_config: Config) -> Result<(), ProxyError> {
        // Save to file (assuming file based config, but for now just update memory)
        // In real impl, we should write to config.toml
        
        // Simple file write
        let content = toml::to_string(&new_config)
            .map_err(|e| ProxyError::Config { message: format!("Failed to serialize config: {}", e) })?;
            
        // Assuming we know the config path. For now hardcode or just update memory.
        // Let's just update memory for this exercise as file path is not stored in ConfigManager
        // But to be persistent we should write it.
        // Let's try writing to "config.toml" in CWD
        
        std::fs::write("config.toml", content)
            .map_err(|e| ProxyError::Config { message: format!("Failed to write config file: {}", e) })?;
        
        let mut guard = self.config.write().unwrap();
        *guard = new_config;
        Ok(())
    }

    pub fn get_vendor(&self, vendor_id: &str) -> Option<VendorConfig> {
        self.read()
            .vendors
            .iter()
            .find(|v| v.id == vendor_id)
            .cloned()
    }

    pub fn resolve_model(&self, abstract_id: &str) -> Vec<(VendorConfig, ModelConfig)> {
        let normalized = abstract_id.to_lowercase();
        let config = self.read();

        let mut results = Vec::new();

        for vendor in &config.vendors {
            if !vendor.enabled {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                let model_abstract = model
                    .abstract_id
                    .as_ref()
                    .unwrap_or(&model.model_id)
                    .to_lowercase();

                if model_abstract == normalized {
                    results.push((vendor.clone(), model.clone()));
                }
            }
        }

        results
    }

    pub fn get_enabled_vendors(&self) -> Vec<VendorConfig> {
        self.read()
            .vendors
            .iter()
            .filter(|v| v.enabled)
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub fn is_global_model_enabled(&self) -> bool {
        let config = self.read();
        config
            .global_model
            .as_ref()
            .map(|g| g.enabled)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn get_all_model_mappings(&self) -> HashMap<String, Vec<(String, String)>> {
        let config = self.read();
        let mut mappings: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for vendor in &config.vendors {
            if !vendor.enabled {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                let abstract_id = model
                    .abstract_id
                    .as_ref()
                    .unwrap_or(&model.model_id)
                    .to_lowercase();

                mappings
                    .entry(abstract_id)
                    .or_default()
                    .push((vendor.id.clone(), model.model_id.clone()));
            }
        }

        mappings
    }
}
