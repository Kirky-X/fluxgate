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

use std::time::Duration;

// Re-export concrete implementations
pub mod cache;
pub mod config;
pub mod database;

// Re-export concrete types for use in builder methods
use super::infrastructure::cache::Oxcache;
use super::infrastructure::config::Confers;
use super::infrastructure::database::Dbnexus;

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

/// Row representation for database query results.
#[derive(Debug, Clone)]
pub struct Row {
    /// Column values as strings
    pub values: Vec<String>,
    /// Column names
    pub columns: Vec<String>,
}

impl Row {
    /// Create a new row with values and columns.
    pub fn new(values: Vec<String>, columns: Vec<String>) -> Self {
        Self { values, columns }
    }

    /// Get a value by column index.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.values.get(index).map(|s| s.as_str())
    }

    /// Get a value by column name.
    pub fn get_by_name(&self, name: &str) -> Option<&str> {
        let idx = self.columns.iter().position(|c| c == name)?;
        self.values.get(idx).map(|s| s.as_str())
    }
}

/// Database error types.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Query error: {0}")]
    Query(String),

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("Execution error: {0}")]
    Execution(String),
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

/// Trait for values that can be used as SQL parameters.
pub trait ToSql: Send + Sync {
    /// Convert to SQL parameter representation.
    fn to_sql(&self) -> &dyn ToSql;
}

impl ToSql for String {
    fn to_sql(&self) -> &dyn ToSql {
        self
    }
}

impl ToSql for i64 {
    fn to_sql(&self) -> &dyn ToSql {
        self
    }
}

impl ToSql for i32 {
    fn to_sql(&self) -> &dyn ToSql {
        self
    }
}

impl ToSql for bool {
    fn to_sql(&self) -> &dyn ToSql {
        self
    }
}

/// Trait for database transaction sessions.
///
/// Represents an active transaction that can be committed or rolled back.
pub trait DatabaseSession: Send + Sync {
    /// Commit the transaction.
    fn commit(self: Box<Self>) -> Result<(), DbError>;

    /// Roll back the transaction.
    fn rollback(self: Box<Self>) -> Result<(), DbError>;

    /// Execute a query within this transaction.
    fn query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError>;

    /// Execute a statement within this transaction.
    fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, DbError>;
}

/// Builder pattern helper for Cache implementations.
#[derive(Default)]
pub struct CacheBuilder {
    ttl: Option<Duration>,
    capacity: Option<usize>,
}

impl CacheBuilder {
    /// Set the TTL (time-to-live) for cache entries.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the maximum capacity for the cache.
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Build the cache with configured options.
    pub fn build(self) -> Oxcache {
        Oxcache::builder()
            .ttl(
                self.ttl
                    .unwrap_or_else(|| std::time::Duration::from_secs(300)),
            )
            .capacity(self.capacity.unwrap_or(10000))
            .build()
    }
}

/// Builder pattern helper for Config implementations.
#[derive(Default)]
pub struct ConfigBuilder {
    file: Option<String>,
    env_prefix: Option<String>,
}

impl ConfigBuilder {
    /// Set the configuration file path.
    pub fn file(mut self, file: &str) -> Self {
        self.file = Some(file.to_string());
        self
    }

    /// Set the environment variable prefix.
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Build the config with configured options.
    pub fn build(self) -> Confers {
        Confers::builder()
            .file(self.file.as_deref().unwrap_or("config.toml"))
            .env_prefix(self.env_prefix.as_deref().unwrap_or("APP"))
            .build()
    }
}

/// Builder pattern helper for Database implementations.
#[derive(Default)]
pub struct DatabaseBuilder {
    max_connections: Option<u32>,
    timeout: Option<Duration>,
}

impl DatabaseBuilder {
    /// Set the maximum number of connections in the pool.
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = Some(max);
        self
    }

    /// Set the connection timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the database with configured options.
    pub fn build(self, connection_string: &str) -> Dbnexus {
        Dbnexus::builder()
            .max_connections(self.max_connections.unwrap_or(10))
            .timeout(
                self.timeout
                    .unwrap_or_else(|| std::time::Duration::from_secs(30)),
            )
            .build(connection_string)
    }
}
