// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Infrastructure helper types.
//!
//! This module collects low-level helper types shared by the infrastructure
//! traits and their concrete implementations:
//! - [`Row`] - database query result row representation
//! - [`DbError`] - database error enum
//! - [`ToSql`] - SQL parameter trait
//! - [`DatabaseSession`] - transaction session trait

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
