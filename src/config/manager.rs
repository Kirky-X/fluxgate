// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{Config, VendorConfig, ModelConfig, GlobalModelConfig};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, RwLock};
use crate::error::NexusFlowError;

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

    pub fn update_config(&self, new_config: Config) -> Result<(), NexusFlowError> {
        // Save to file (assuming file based config, but for now just update memory)
        // In real impl, we should write to config.toml

        // Simple file write
        let content = toml::to_string(&new_config)
            .map_err(|e| NexusFlowError::Config { message: format!("Failed to serialize config: {}", e) })?;

        // Assuming we know the config path. For now hardcode or just update memory.
        // But to be persistent we should write it.
        // Let's try writing to "config.toml" in CWD

        std::fs::write("config.toml", content)
            .map_err(|e| NexusFlowError::Config { message: format!("Failed to write config file: {}", e) })?;

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
