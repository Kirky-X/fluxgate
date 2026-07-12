// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// Smart LLM Proxy - Core Library

pub mod config;
pub mod api;
pub mod protocol;
pub mod router;
pub mod vendor;
pub mod database;
pub mod utils;
pub mod error;
pub mod middleware;
pub mod infrastructure; // New: Infrastructure layer for DI-compliant components

pub use config::{ConfigManager, Config, ServerConfig, DatabaseConfig, VendorConfig, ModelConfig};
pub use error::NexusFlowError;
pub use api::ApiState;
pub use vendor::VendorManager;
pub use database::Database;
pub use middleware::ApiAuthState;

// Re-export infrastructure traits and supporting types only (interfaces, not implementations).
// Concrete implementations (Oxcache/Confers/Dbnexus) are intentionally NOT re-exported here:
// callers should depend on the traits (Cache/Config/Database) and construct implementations
// via the infrastructure module or its builder API.
// Import directly from infrastructure module to avoid private trait import errors
pub use infrastructure::Cache;
pub use infrastructure::Config as InfrastructureConfig;
pub use infrastructure::Database as InfrastructureDatabase;
pub use infrastructure::Row;
pub use infrastructure::DbError;
