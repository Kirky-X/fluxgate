// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// Smart LLM Proxy - Core Library

// config 单 struct（13 段）+ ConfigManager/ConfigLoader/DynamicField 整体
// 进 llm-gate（design.md D1.1：拆段会分裂 POST /admin/config 热更新语义）；
// 门面 re-export 保持 nexusflow::config 路径可用
pub use llm_gate::config;
pub mod error;
// i18n 引擎整体进 llm-gate（Fluent 目录随迁）；此处门面 re-export，
// crate 内外统一经 nexusflow::i18n（= llm_gate::i18n）访问
pub use llm_gate::i18n;
pub mod protocol;
pub mod utils;

#[cfg(feature = "database")]
pub mod database;

#[cfg(feature = "vendor-management")]
pub mod vendor;

#[cfg(feature = "infrastructure")]
pub mod infrastructure;

#[cfg(feature = "router")]
pub mod router;

#[cfg(feature = "web-server")]
pub mod api;

#[cfg(feature = "web-server")]
pub mod middleware;

#[cfg(all(feature = "web-server", feature = "logging"))]
pub mod server;

#[cfg(feature = "sdk")]
pub mod sdk;

pub use config::{
    Config, ConfigManager, DatabaseConfig, ModelConfig, PricingConfig, ServerConfig, VendorConfig,
};
pub use error::{GateError, NexusFlowError};

#[cfg(feature = "web-server")]
pub use api::ApiState;

#[cfg(feature = "database")]
pub use database::Database;

#[cfg(feature = "web-server")]
pub use middleware::ApiAuthState;

#[cfg(feature = "vendor-management")]
pub use vendor::VendorManager;

#[cfg(all(
    feature = "infrastructure",
    feature = "database",
    feature = "vendor-management",
    feature = "logging"
))]
pub use infrastructure::di;
