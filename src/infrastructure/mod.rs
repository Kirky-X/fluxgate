// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Infrastructure Layer - Core dependency abstractions
//!
//! This module provides unified trait interfaces for cache, config, and database operations
//! following the dependency injection architecture defined in `/home/dev/di.md`.
//!
//! All traits require `Send + Sync` compliance and support three construction patterns:
//! - `new()` - Default construction with sensible defaults
//! - `builder()` - Custom configuration via builder pattern
//! - `with_dependencies()` - Full dependency injection
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │        Application Layer             │
//! └─────────────────────────────────────┘
//!                  ▲
//!                  │ uses
//!                  │
//! ┌─────────────────────────────────────┐
//! │      Feature Layer (API Layer)       │
//! └─────────────────────────────────────┘
//!                  ▲
//!                  │ depends on
//!                  │
//! ┌─────────────────────────────────────┐
//! │    Infrastructure Layer (This)       │
//! │  - Cache: oxcache wrapper           │
//! │  - Config: confers wrapper          │
//! │  - Database: dbnexus wrapper        │
//! └─────────────────────────────────────┘
//! ```

// Submodules
pub mod builder;
pub mod cache;
pub mod config;
pub mod database;
pub mod types;

// Re-export helper types
pub use types::{DatabaseSession, DbError, Row, ToSql};

// Re-export builder types
pub use builder::{CacheBuilder, ConfigBuilder, DatabaseBuilder};

// Re-export concrete implementations
pub use cache::Oxcache;
pub use config::Confers;
pub use database::Dbnexus;

/// Unified Cache trait for all caching operations.
///
/// This trait provides a simple interface for key-value caching with async support.
/// All implementations must be thread-safe (`Send + Sync`).
///
/// # Methods
///
/// - `get(&self, key: &str) -> Option<String>` - Retrieve a value by key
/// - `set(&self, key: &str, value: String)` - Store a key-value pair
/// - `delete(&self, key: &str) -> bool` - Remove a key, returns true if existed
/// - `exists(&self, key: &str) -> bool` - Check if a key exists
/// - `clear(&self)` - Remove all entries
///
/// # Examples
///
/// ```ignore
/// use std::sync::Arc;
/// use nexusflow::infrastructure::{Cache, Oxcache};
///
/// // Pattern 1: new()
/// let cache = Oxcache::new();
/// cache.set("key", "value".to_string());
/// assert_eq!(cache.get("key"), Some("value".to_string()));
///
/// // Pattern 2: with_dependencies()
/// let cache = Oxcache::with_dependencies(Arc::new(custom_backend));
/// ```
pub trait Cache: Send + Sync {
    /// Retrieve a value from the cache.
    ///
    /// Returns `Some(value)` if the key exists, `None` otherwise.
    fn get(&self, key: &str) -> Option<String>;

    /// Store a key-value pair in the cache.
    ///
    /// If the key already exists, the value is overwritten.
    fn set(&self, key: &str, value: String);

    /// Delete a key from the cache.
    ///
    /// Returns `true` if the key existed and was deleted, `false` otherwise.
    fn delete(&self, key: &str) -> bool;

    /// Check if a key exists in the cache.
    ///
    /// Returns `true` if the key exists, `false` otherwise.
    fn exists(&self, key: &str) -> bool;

    /// Clear all entries from the cache.
    fn clear(&self);
}

/// Unified Config trait for configuration management.
///
/// This trait provides type-safe accessors for configuration values.
/// All implementations must be thread-safe (`Send + Sync`).
///
/// # Methods
///
/// - `get_string(&self, key: &str) -> Option<String>` - Get a string value
/// - `get_int(&self, key: &str) -> Option<i64>` - Get an integer value
/// - `get_bool(&self, key: &str) -> Option<bool>` - Get a boolean value
///
/// # Examples
///
/// ```ignore
/// use std::sync::Arc;
/// use nexusflow::infrastructure::{Config, Confers};
///
/// // Pattern 1: new()
/// let config = Confers::new();
///
/// // Pattern 2: with_dependencies()
/// let config = Confers::with_dependencies(Arc::new(custom_loader));
///
/// // Access values
/// let port = config.get_int("server.port");
/// let enabled = config.get_bool("feature.enabled");
/// ```
pub trait Config: Send + Sync {
    /// Get a string configuration value.
    ///
    /// Returns `Some(value)` if the key exists and is a valid string, `None` otherwise.
    fn get_string(&self, key: &str) -> Option<String>;

    /// Get an integer configuration value.
    ///
    /// Returns `Some(value)` if the key exists and can be parsed as an integer, `None` otherwise.
    fn get_int(&self, key: &str) -> Option<i64>;

    /// Get a boolean configuration value.
    ///
    /// Returns `Some(value)` if the key exists and can be parsed as a boolean, `None` otherwise.
    fn get_bool(&self, key: &str) -> Option<bool>;
}

/// Unified Database trait for database operations.
///
/// This trait provides a simple interface for SQL operations including queries,
/// executions, and transactions. All implementations must be thread-safe (`Send + Sync`).
///
/// # Methods
///
/// - `query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError>` - Execute a SELECT query
/// - `execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, DbError>` - Execute INSERT/UPDATE/DELETE
/// - `begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError>` - Start a transaction
///
/// # Examples
///
/// ```ignore
/// use std::sync::Arc;
/// use nexusflow::infrastructure::{Database, Dbnexus};
///
/// // Pattern 1: new()
/// let db = Dbnexus::new("sqlite:./data/llm_proxy.db");
///
/// // Pattern 2: with_dependencies()
/// let db = Dbnexus::with_dependencies(Arc::new(custom_pool));
///
/// // Execute queries
/// let rows = db.query("SELECT * FROM tokens", &[]).unwrap();
/// ```
pub trait Database: Send + Sync {
    /// Execute a SELECT query and return results.
    fn query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError>;

    /// Execute INSERT, UPDATE, DELETE or DDL statements.
    ///
    /// Returns the number of rows affected.
    fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, DbError>;

    /// Begin a new transaction.
    fn begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError>;
}
