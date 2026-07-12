// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// Smart LLM Proxy - Core Library

pub mod api;
pub mod config;
pub mod database;
pub mod error;
pub mod infrastructure;
pub mod middleware;
pub mod protocol;
pub mod router;
pub mod utils;
pub mod vendor; // New: Infrastructure layer for DI-compliant components

pub use api::ApiState;
pub use config::{Config, ConfigManager, DatabaseConfig, ModelConfig, ServerConfig, VendorConfig};
pub use database::Database;
pub use error::NexusFlowError;
pub use middleware::ApiAuthState;
pub use vendor::VendorManager;

// Re-export infrastructure traits and supporting types only (interfaces, not implementations).
// Concrete implementations (Oxcache/Confers/Dbnexus) are intentionally NOT re-exported here:
// callers should depend on the traits (Cache/Config/Database) and construct implementations
// via the infrastructure module or its builder API.
// Import directly from infrastructure module to avoid private trait import errors
pub use infrastructure::Cache;
pub use infrastructure::Config as InfrastructureConfig;
pub use infrastructure::Database as InfrastructureDatabase;
pub use infrastructure::DbError;
pub use infrastructure::Row;
