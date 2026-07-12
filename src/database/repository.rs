// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::{LogFilter, RequestLog, TokenUsage, VendorStatusRecord};

#[derive(Clone)]
pub struct Repository {
    pub pool: sqlx::sqlite::SqlitePool,
}

impl Repository {
    pub fn new(pool: sqlx::sqlite::SqlitePool) -> Self {
        Self { pool }
    }

    #[allow(dead_code)]
    pub async fn initialize(&self) -> Result<(), sqlx::Error> {
        let schema = include_str!("schema.sql");
        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(&self.pool).await?;
            }
        }
        Ok(())
    }

    pub async fn log_request(&self, log: &RequestLog) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        )
        .bind(&log.request_id)
        .bind(log.timestamp)
        .bind(&log.endpoint)
        .bind(&log.input_format)
        .bind(&log.abstract_model)
        .bind(&log.messages)
        .bind(&log.actual_vendor)
        .bind(&log.actual_model)
        .bind(&log.routing_strategy)
        .bind(log.status_code)
        .bind(&log.response)
        .bind(&log.error_message)
        .bind(log.prompt_tokens)
        .bind(log.completion_tokens)
        .bind(log.total_tokens)
        .bind(log.duration_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn query_logs(&self, filter: &LogFilter) -> Result<Vec<RequestLog>, sqlx::Error> {
        let mut query = String::from("SELECT * FROM request_logs WHERE 1=1");

        if filter.start_time.is_some() {
            query.push_str(" AND timestamp >= ?");
        }

        if filter.vendor.is_some() {
            query.push_str(" AND actual_vendor = ?");
        }

        if filter.model.is_some() {
            query.push_str(" AND actual_model = ?");
        }

        query.push_str(" ORDER BY timestamp DESC");

        if filter.limit > 0 {
            query.push_str(&format!(" LIMIT {}", filter.limit));
        }

        // Build query with bindings
        let mut sql_query = sqlx::query_as::<_, RequestLog>(&query);

        if let Some(start) = filter.start_time {
            sql_query = sql_query.bind(start);
        }

        if let Some(ref vendor) = filter.vendor {
            sql_query = sql_query.bind(vendor.as_str());
        }

        if let Some(ref model) = filter.model {
            sql_query = sql_query.bind(model.as_str());
        }

        let rows = sql_query.fetch_all(&self.pool).await?;
        Ok(rows)
    }

    pub async fn search_logs(
        &self,
        keyword: &str,
        limit: u32,
    ) -> Result<Vec<RequestLog>, sqlx::Error> {
        let pattern = format!("%{}%", keyword);
        let rows = sqlx::query_as::<_, RequestLog>(
            r#"
            SELECT * FROM request_logs
            WHERE messages LIKE ? OR error_message LIKE ?
            ORDER BY timestamp DESC
            LIMIT ?
        "#,
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    #[allow(dead_code)]
    pub async fn get_token_stats(
        &self,
        vendor: Option<&str>,
    ) -> Result<Vec<TokenUsage>, sqlx::Error> {
        if let Some(v) = vendor {
            let rows = sqlx::query_as::<_, TokenUsage>(
                "SELECT * FROM token_usage WHERE vendor = ? ORDER BY date DESC, hour DESC",
            )
            .bind(v)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows)
        } else {
            let rows = sqlx::query_as::<_, TokenUsage>(
                "SELECT * FROM token_usage ORDER BY date DESC, hour DESC",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows)
        }
    }

    #[allow(dead_code)]
    pub async fn get_all_vendor_status(&self) -> Result<Vec<VendorStatusRecord>, sqlx::Error> {
        let rows =
            sqlx::query_as::<_, VendorStatusRecord>("SELECT * FROM vendor_status ORDER BY vendor")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }
}
