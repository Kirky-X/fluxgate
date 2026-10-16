// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

// Smart LLM Proxy - Core Library

// config 单 struct 承载全部段：拆段会分裂 POST /admin/config 热更新语义
pub mod config;
pub mod error;
// i18n 引擎：crate 内外统一经 fluxgate::i18n 访问
pub mod i18n;
// utils：metrics / retry / cost_tracker
pub mod utils;
// protocol：IR 与双协议转换器
pub mod protocol;
// 存储边界缝（RequestLogSink/UsageSink + 契约类型）定义于本模块，实现留在 database 模块
pub mod storage;

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

#[cfg(feature = "vault")]
pub mod vault;

pub use config::{
    Config, ConfigManager, DatabaseConfig, DocsSection, ModelConfig, PricingConfig, ServerConfig,
    VendorConfig,
};
pub use error::{FluxgateError, GateError};

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
