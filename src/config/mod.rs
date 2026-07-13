// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};

pub mod loader;
pub mod manager;

pub use manager::ConfigManager;

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
    pub name: String,
    pub type_: String,
    pub api_key_env: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_vendor_enabled")]
    pub enabled: bool,
    #[serde(default = "Vec::new")]
    pub models: Vec<ModelConfig>,
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
        start: String,
        end: String,
    },
    #[serde(rename = "interval")]
    Interval {
        hours: u64,
    },
    #[serde(rename = "token")]
    Token {
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
    pub global_model: Option<GlobalModelConfig>,
}
