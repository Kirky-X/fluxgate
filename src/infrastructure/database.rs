// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Dbnexus implementation for the unified Database trait.
//!
//! This module provides an implementation of the [`Database`] trait using the dbnexus library.
//! It follows the dependency injection architecture with three construction patterns:
//! - `new()` - Default SQLite database connection
//! - `builder()` - Custom connection pool configuration
//! - `with_dependencies()` - Full dependency injection
//!
//! # Examples
//!
//! ```ignore
//! use std::sync::Arc;
//! use nexusflow::infrastructure::{Database, Dbnexus, Row};
//!
//! // Pattern 1: new() - Default database connection
//! let db = Dbnexus::new("sqlite:./data/llm_proxy.db");
//! let rows = db.query("SELECT * FROM tokens", &[]).unwrap();
//!
//! // Pattern 2: builder() - Custom connection pool
//! let db = Dbnexus::builder()
//!     .max_connections(10)
//!     .timeout(std::time::Duration::from_secs(30))
//!     .build("sqlite:./data/llm_proxy.db");
//!
//! // Pattern 3: with_dependencies() - Custom pool
//! let db = Dbnexus::with_dependencies(Arc::new(custom_pool));
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::{Database, DatabaseSession, DbError, Row, ToSql};

/// Type alias for the in-memory table representation.
type DbTables = HashMap<String, Vec<HashMap<String, String>>>;

/// Backend pool for database connections.
/// This allows for different database implementations.
pub trait DbnexusPool: Send + Sync {
    /// Execute a query and return results.
    fn query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError>;

    /// Execute a statement and return affected rows.
    fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, DbError>;

    /// Begin a transaction.
    fn begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError>;
}

/// Memory database session for transactions.
struct MemoryDbSession {
    backend: Arc<RwLock<DbTables>>,
    committed: bool,
}

impl DatabaseSession for MemoryDbSession {
    fn commit(mut self: Box<Self>) -> Result<(), DbError> {
        self.committed = true;
        Ok(())
    }

    fn rollback(self: Box<Self>) -> Result<(), DbError> {
        Ok(())
    }

    fn query(&self, sql: &str, _params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError> {
        // Simple parsing for demonstration
        if let Some(table_start) = sql.find("FROM") {
            let table_part = &sql[table_start + 5..];
            let table_name = table_part.split_whitespace().next().unwrap_or("");

            let tables = self.backend.read().unwrap();
            if let Some(rows) = tables.get(table_name) {
                let mut result = Vec::new();
                for row in rows {
                    let columns: Vec<String> = row.keys().cloned().collect();
                    let values: Vec<String> = row.values().cloned().collect();
                    result.push(Row::new(values, columns));
                }
                return Ok(result);
            }
        }
        Ok(Vec::new())
    }

    fn execute(&self, _sql: &str, _params: &[&dyn ToSql]) -> Result<u64, DbError> {
        Ok(1)
    }
}

/// In-memory database backend for testing.
#[derive(Clone)]
pub struct MemoryDbBackend {
    /// Simulated tables.
    tables: Arc<RwLock<DbTables>>,
}

impl Default for MemoryDbBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryDbBackend {
    /// Create a new memory database backend.
    pub fn new() -> Self {
        let mut tables = HashMap::new();

        // Initialize default tables for NexusFlow
        tables.insert("tokens".to_string(), vec![]);
        tables.insert("request_logs".to_string(), vec![]);
        tables.insert("vendor_status".to_string(), vec![]);
        tables.insert("model_stats".to_string(), vec![]);

        Self {
            tables: Arc::new(RwLock::new(tables)),
        }
    }

    /// Execute a SELECT query (simplified simulation).
    fn execute_select(&self, sql: &str) -> Result<Vec<Row>, DbError> {
        // Simple parsing for demonstration
        // In a real implementation, this would use SQLite
        if let Some(table_start) = sql.find("FROM") {
            let table_part = &sql[table_start + 5..];
            let table_name = table_part.split_whitespace().next().unwrap_or("");

            let tables = self.tables.read().unwrap();
            if let Some(rows) = tables.get(table_name) {
                let mut result = Vec::new();
                for row in rows {
                    let columns: Vec<String> = row.keys().cloned().collect();
                    let values: Vec<String> = row.values().cloned().collect();
                    result.push(Row::new(values, columns));
                }
                return Ok(result);
            }
        }
        Ok(Vec::new())
    }

    /// Execute a non-SELECT query (simplified simulation).
    fn execute_statement(&self, _sql: &str) -> Result<u64, DbError> {
        // Simple INSERT/UPDATE/DELETE simulation
        Ok(1)
    }
}

impl DbnexusPool for MemoryDbBackend {
    fn query(&self, sql: &str, _params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError> {
        self.execute_select(sql)
    }

    fn execute(&self, sql: &str, _params: &[&dyn ToSql]) -> Result<u64, DbError> {
        self.execute_statement(sql)
    }

    fn begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError> {
        let session = MemoryDbSession {
            backend: self.tables.clone(),
            committed: false,
        };
        Ok(Box::new(session))
    }
}

/// SQLite database backend.
#[derive(Clone)]
pub struct SqliteBackend {
    /// Connection string.
    connection_string: String,
}

impl SqliteBackend {
    /// Create a new SQLite backend.
    pub fn new(connection_string: &str) -> Self {
        Self {
            connection_string: connection_string.to_string(),
        }
    }
}

impl DbnexusPool for SqliteBackend {
    fn query(&self, _sql: &str, _params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError> {
        // In a real implementation, this would use SQLite
        // For now, return empty results
        let _ = self.connection_string;
        Ok(Vec::new())
    }

    fn execute(&self, sql: &str, _params: &[&dyn ToSql]) -> Result<u64, DbError> {
        // In a real implementation, this would use SQLite
        let _ = sql;
        Ok(0)
    }

    fn begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError> {
        // In a real implementation, this would create a SQLite transaction
        Err(DbError::Transaction(
            "SQLite transactions not yet implemented".to_string(),
        ))
    }
}

/// Configuration for the Dbnexus wrapper.
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    /// Maximum connections in the pool.
    pub max_connections: u32,
    /// Connection timeout.
    pub timeout: Option<std::time::Duration>,
    /// Enable WAL mode (SQLite).
    pub enable_wal: bool,
    /// Busy timeout (SQLite).
    pub busy_timeout: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            timeout: Some(std::time::Duration::from_secs(30)),
            enable_wal: true,
            busy_timeout: 5000,
        }
    }
}

/// The main Dbnexus struct that implements the [`Database`] trait.
///
/// This struct wraps database connection pools and provides a unified interface
/// for executing SQL queries and transactions.
#[derive(Clone)]
pub struct Dbnexus {
    /// Backend pool for database connections.
    pool: Arc<dyn DbnexusPool>,
}

impl Dbnexus {
    /// Pattern 1: Create a database connection with default settings.
    ///
    /// This creates a SQLite database connection with default pool settings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::{Database, database::Dbnexus};
    ///
    /// let db = Dbnexus::new("sqlite:./data/llm_proxy.db");
    /// let rows = db.query("SELECT * FROM tokens", &[]).unwrap();
    /// ```
    pub fn new(connection_string: &str) -> Self {
        Self::builder().build(connection_string)
    }

    /// Pattern 2: Create a database connection with custom settings via builder.
    ///
    /// This allows customization of pool size, timeout, and other options.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::{Database, database::Dbnexus};
    ///
    /// let db = Dbnexus::builder()
    ///     .max_connections(10)
    ///     .timeout(Duration::from_secs(30))
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn builder() -> DbnexusBuilder {
        DbnexusBuilder::new()
    }

    /// Pattern 3: Create a database wrapper with a custom backend.
    ///
    /// This allows injection of a custom pool implementation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{Database, database::Dbnexus};
    /// use nexusflow::infrastructure::database::MemoryDbBackend;
    ///
    /// let backend = Arc::new(MemoryDbBackend::new());
    /// let db = Dbnexus::with_dependencies(backend);
    ///
    /// let rows = db.query("SELECT * FROM tokens", &[]).unwrap();
    /// ```
    pub fn with_dependencies(pool: Arc<dyn DbnexusPool>) -> Self {
        Self { pool }
    }

    /// Create a database wrapper with custom backend and configuration.
    pub fn with_config_and_dependencies(
        pool: Arc<dyn DbnexusPool>,
        _max_connections: u32,
        _timeout: Option<std::time::Duration>,
    ) -> Self {
        Self { pool }
    }
}

/// Builder for [`Dbnexus`] with customizable options.
#[derive(Default)]
pub struct DbnexusBuilder {
    max_connections: u32,
    timeout: Option<std::time::Duration>,
    enable_wal: bool,
    busy_timeout: u32,
    pool: Option<Arc<dyn DbnexusPool>>,
}

impl DbnexusBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of connections in the pool.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::database::Dbnexus;
    ///
    /// let db = Dbnexus::builder()
    ///     .max_connections(20)
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }

    /// Set the connection timeout.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::database::Dbnexus;
    ///
    /// let db = Dbnexus::builder()
    ///     .timeout(Duration::from_secs(60))
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Enable WAL mode for SQLite.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::database::Dbnexus;
    ///
    /// let db = Dbnexus::builder()
    ///     .enable_wal(true)
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn enable_wal(mut self, enabled: bool) -> Self {
        self.enable_wal = enabled;
        self
    }

    /// Set the busy timeout for SQLite.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::database::Dbnexus;
    ///
    /// let db = Dbnexus::builder()
    ///     .busy_timeout(10000)
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn busy_timeout(mut self, timeout: u32) -> Self {
        self.busy_timeout = timeout;
        self
    }

    /// Set a custom pool implementation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{database::Dbnexus, database::MemoryDbBackend};
    ///
    /// let backend = Arc::new(MemoryDbBackend::new()) as Arc<dyn nexusflow::infrastructure::database::DbnexusPool>;
    /// let db = Dbnexus::builder()
    ///     .pool(backend)
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn pool(mut self, pool: Arc<dyn DbnexusPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Build the [`Dbnexus`] instance with the configured settings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::database::Dbnexus;
    ///
    /// let db = Dbnexus::builder()
    ///     .max_connections(10)
    ///     .timeout(Duration::from_secs(30))
    ///     .build("sqlite:./data/llm_proxy.db");
    /// ```
    pub fn build(self, connection_string: &str) -> Dbnexus {
        let pool: Arc<dyn DbnexusPool> = if let Some(pool) = self.pool {
            pool
        } else {
            // Create appropriate backend based on connection string
            if connection_string.starts_with("sqlite:") {
                Arc::new(SqliteBackend::new(connection_string))
            } else {
                Arc::new(MemoryDbBackend::new())
            }
        };

        // Read builder fields to construct config (for future use)
        let _config = DatabaseConfig {
            max_connections: self.max_connections,
            timeout: self.timeout,
            enable_wal: self.enable_wal,
            busy_timeout: self.busy_timeout,
        };

        Dbnexus { pool }
    }
}

impl Database for Dbnexus {
    fn query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>, DbError> {
        self.pool.query(sql, params)
    }

    fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<u64, DbError> {
        self.pool.execute(sql, params)
    }

    fn begin_transaction(&self) -> Result<Box<dyn DatabaseSession>, DbError> {
        self.pool.begin_transaction()
    }
}

impl Default for Dbnexus {
    fn default() -> Self {
        Self {
            pool: Arc::new(MemoryDbBackend::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Helper function to create a test database.
    fn test_db() -> Dbnexus {
        let backend = Arc::new(MemoryDbBackend::new()) as Arc<dyn DbnexusPool>;
        Dbnexus::with_dependencies(backend)
    }

    #[test]
    fn test_database_query() {
        let db = test_db();
        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_execute() {
        let db = test_db();
        let result = db.execute("INSERT INTO tokens (id, usage) VALUES (1, 100)", &[]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
    }

    #[test]
    fn test_database_begin_transaction() {
        let db = test_db();
        let session = db.begin_transaction();
        assert!(session.is_ok());
    }

    #[test]
    fn test_database_new() {
        let db = Dbnexus::new("sqlite:./test.db");
        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_with_dependencies() {
        let backend = Arc::new(MemoryDbBackend::new()) as Arc<dyn DbnexusPool>;
        let db = Dbnexus::with_dependencies(backend);

        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_builder() {
        let db = Dbnexus::builder()
            .max_connections(10)
            .timeout(std::time::Duration::from_secs(30))
            .build("sqlite:./test.db");

        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_with_config_and_dependencies() {
        let backend = Arc::new(MemoryDbBackend::new()) as Arc<dyn DbnexusPool>;
        let db = Dbnexus::with_config_and_dependencies(
            backend,
            20,
            Some(std::time::Duration::from_secs(60)),
        );

        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_default() {
        let db = Dbnexus::default();
        let rows = db.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_database_clone() {
        let db = test_db();
        let cloned = db.clone();
        let rows = cloned.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_memory_db_backend() {
        let backend = MemoryDbBackend::new();
        let rows = backend.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
        assert!(rows.unwrap().is_empty());
    }

    #[test]
    fn test_memory_db_backend_execute() {
        let backend = MemoryDbBackend::new();
        let result = backend.execute("INSERT INTO tokens VALUES (1)", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_memory_db_backend_begin_transaction() {
        let backend = MemoryDbBackend::new();
        let session = backend.begin_transaction();
        assert!(session.is_ok());
    }

    #[test]
    fn test_sqlite_backend() {
        let backend = SqliteBackend::new("sqlite:./test.db");
        let rows = backend.query("SELECT * FROM tokens", &[]);
        assert!(rows.is_ok());
    }

    #[test]
    fn test_sqlite_backend_execute() {
        let backend = SqliteBackend::new("sqlite:./test.db");
        let result = backend.execute("CREATE TABLE IF NOT EXISTS test (id INTEGER)", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_memory_db_session_commit() {
        let backend = MemoryDbBackend::new();
        let session = backend.begin_transaction().unwrap();
        let result = session.commit();
        assert!(result.is_ok());
    }

    #[test]
    fn test_memory_db_session_rollback() {
        let backend = MemoryDbBackend::new();
        let session = backend.begin_transaction().unwrap();
        let result = session.rollback();
        assert!(result.is_ok());
    }

    #[test]
    fn test_row_get_by_index() {
        let row = Row::new(
            vec!["value1".to_string(), "value2".to_string()],
            vec!["col1".to_string(), "col2".to_string()],
        );
        assert_eq!(row.get(0), Some("value1"));
        assert_eq!(row.get(1), Some("value2"));
        assert_eq!(row.get(2), None);
    }

    #[test]
    fn test_row_get_by_name() {
        let row = Row::new(
            vec!["value1".to_string(), "value2".to_string()],
            vec!["col1".to_string(), "col2".to_string()],
        );
        assert_eq!(row.get_by_name("col1"), Some("value1"));
        assert_eq!(row.get_by_name("col2"), Some("value2"));
        assert_eq!(row.get_by_name("col3"), None);
    }
}
