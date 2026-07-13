// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::{LogFilter, RequestLog, TokenUsage, VendorStatusRecord};
use dbnexus::{DbError, DbPool};
use sea_orm::{ConnectionTrait, DatabaseBackend, QueryResult, Statement, Value};

#[derive(Clone)]
pub struct Repository {
    pub pool: DbPool,
}

impl Repository {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub async fn initialize(&self) -> Result<(), DbError> {
        let schema = include_str!("schema.sql");
        let session = self.pool.get_session("admin").await?;
        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                session.execute_raw_ddl(trimmed).await?;
            }
        }
        Ok(())
    }

    pub async fn log_request(&self, log: &RequestLog) -> Result<(), DbError> {
        let sql = r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#;

        let values: Vec<Value> = vec![
            log.request_id.as_str().into(),
            log.timestamp.into(),
            log.endpoint.as_str().into(),
            log.input_format.as_str().into(),
            log.abstract_model.as_deref().into(),
            log.messages.as_str().into(),
            log.actual_vendor.as_str().into(),
            log.actual_model.as_str().into(),
            log.routing_strategy.as_str().into(),
            log.status_code.into(),
            log.response.as_deref().into(),
            log.error_message.as_deref().into(),
            log.prompt_tokens.into(),
            log.completion_tokens.into(),
            log.total_tokens.into(),
            log.duration_ms.into(),
        ];

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        conn.execute_raw(stmt).await?;
        Ok(())
    }

    pub async fn query_logs(&self, filter: &LogFilter) -> Result<Vec<RequestLog>, DbError> {
        let mut query = String::from("SELECT * FROM request_logs WHERE 1=1");
        let mut values: Vec<Value> = Vec::new();

        if let Some(start) = filter.start_time {
            query.push_str(" AND timestamp >= ?");
            values.push(start.into());
        }

        if let Some(ref vendor) = filter.vendor {
            query.push_str(" AND actual_vendor = ?");
            values.push(vendor.as_str().into());
        }

        if let Some(ref model) = filter.model {
            query.push_str(" AND actual_model = ?");
            values.push(model.as_str().into());
        }

        query.push_str(" ORDER BY timestamp DESC");

        if filter.limit > 0 {
            query.push_str(" LIMIT ?");
            values.push((filter.limit as i64).into());
        }

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, &query, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    pub async fn search_logs(
        &self,
        keyword: &str,
        limit: u32,
    ) -> Result<Vec<RequestLog>, DbError> {
        let pattern = format!("%{}%", keyword);
        let sql = r#"
            SELECT * FROM request_logs
            WHERE messages LIKE ? OR error_message LIKE ?
            ORDER BY timestamp DESC
            LIMIT ?
        "#;

        let values: Vec<Value> = vec![
            pattern.as_str().into(),
            pattern.as_str().into(),
            (limit as i64).into(),
        ];

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    pub async fn get_token_stats(
        &self,
        vendor: Option<&str>,
    ) -> Result<Vec<TokenUsage>, DbError> {
        let (sql, values): (String, Vec<Value>) = if let Some(v) = vendor {
            (
                "SELECT * FROM token_usage WHERE vendor = ? ORDER BY date DESC, hour DESC".to_string(),
                vec![v.into()],
            )
        } else {
            (
                "SELECT * FROM token_usage ORDER BY date DESC, hour DESC".to_string(),
                Vec::new(),
            )
        };

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, &sql, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_token_usage).collect())
    }

    pub async fn get_all_vendor_status(&self) -> Result<Vec<VendorStatusRecord>, DbError> {
        let sql = "SELECT * FROM vendor_status ORDER BY vendor";
        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, Vec::new());
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_vendor_status).collect())
    }
}

fn map_request_log(row: &QueryResult) -> RequestLog {
    RequestLog {
        id: row.try_get::<Option<i64>>("", "id").unwrap_or(None),
        request_id: row.try_get("", "request_id").unwrap_or_default(),
        timestamp: row.try_get("", "timestamp").unwrap_or_default(),
        endpoint: row.try_get("", "endpoint").unwrap_or_default(),
        input_format: row.try_get("", "input_format").unwrap_or_default(),
        abstract_model: row.try_get::<Option<String>>("", "abstract_model").unwrap_or(None),
        messages: row.try_get("", "messages").unwrap_or_default(),
        actual_vendor: row.try_get("", "actual_vendor").unwrap_or_default(),
        actual_model: row.try_get("", "actual_model").unwrap_or_default(),
        routing_strategy: row.try_get("", "routing_strategy").unwrap_or_default(),
        status_code: row.try_get::<Option<i32>>("", "status_code").unwrap_or(None),
        response: row.try_get::<Option<String>>("", "response").unwrap_or(None),
        error_message: row.try_get::<Option<String>>("", "error_message").unwrap_or(None),
        prompt_tokens: row.try_get::<Option<i32>>("", "prompt_tokens").unwrap_or(None),
        completion_tokens: row.try_get::<Option<i32>>("", "completion_tokens").unwrap_or(None),
        total_tokens: row.try_get::<Option<i32>>("", "total_tokens").unwrap_or(None),
        duration_ms: row.try_get::<Option<i64>>("", "duration_ms").unwrap_or(None),
    }
}

fn map_token_usage(row: &QueryResult) -> TokenUsage {
    TokenUsage {
        id: row.try_get::<Option<i64>>("", "id").unwrap_or(None),
        vendor: row.try_get("", "vendor").unwrap_or_default(),
        model: row.try_get("", "model").unwrap_or_default(),
        date: row.try_get("", "date").unwrap_or_default(),
        hour: row.try_get("", "hour").unwrap_or_default(),
        request_count: row.try_get("", "request_count").unwrap_or_default(),
        total_tokens: row.try_get("", "total_tokens").unwrap_or_default(),
        prompt_tokens: row.try_get("", "prompt_tokens").unwrap_or_default(),
        completion_tokens: row.try_get("", "completion_tokens").unwrap_or_default(),
    }
}

fn map_vendor_status(row: &QueryResult) -> VendorStatusRecord {
    VendorStatusRecord {
        vendor: row.try_get("", "vendor").unwrap_or_default(),
        status: row.try_get("", "status").unwrap_or_default(),
        last_updated: row.try_get("", "last_updated").unwrap_or_default(),
        window_used_tokens: row.try_get("", "window_used_tokens").unwrap_or_default(),
        window_reset_at: row.try_get::<Option<i64>>("", "window_reset_at").unwrap_or(None),
        error_count: row.try_get("", "error_count").unwrap_or_default(),
    }
}
