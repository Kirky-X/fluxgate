// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};

pub mod async_logger;
pub mod pre_aggregator;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLog {
    pub id: Option<i64>,
    pub request_id: String,
    pub timestamp: i64,
    pub endpoint: String,
    pub input_format: String,
    pub abstract_model: Option<String>,
    pub messages: String,
    pub actual_vendor: String,
    pub actual_model: String,
    pub routing_strategy: String,
    pub status_code: Option<i32>,
    pub response: Option<String>,
    pub error_message: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenUsage {
    pub id: Option<i64>,
    pub vendor: String,
    pub model: String,
    pub date: String,
    pub hour: i32,
    pub request_count: i64,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct VendorStatusRecord {
    pub vendor: String,
    pub status: String,
    pub last_updated: i64,
    pub window_used_tokens: i64,
    pub window_reset_at: Option<i64>,
    pub error_count: i32,
}

#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub start_time: Option<i64>,
    #[allow(dead_code)]
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: u32,
}

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
        sqlx::query(r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#)
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
        .execute(&self.pool).await?;
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
    
    pub async fn search_logs(&self, keyword: &str, limit: u32) -> Result<Vec<RequestLog>, sqlx::Error> {
        let pattern = format!("%{}%", keyword);
        let rows = sqlx::query_as::<_, RequestLog>(r#"
            SELECT * FROM request_logs
            WHERE messages LIKE ? OR error_message LIKE ?
            ORDER BY timestamp DESC
            LIMIT ?
        "#)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool).await?;
        Ok(rows)
    }
    
    #[allow(dead_code)]
    pub async fn get_token_stats(&self, vendor: Option<&str>) -> Result<Vec<TokenUsage>, sqlx::Error> {
        if let Some(v) = vendor {
            let rows = sqlx::query_as::<_, TokenUsage>(
                "SELECT * FROM token_usage WHERE vendor = ? ORDER BY date DESC, hour DESC"
            ).bind(v).fetch_all(&self.pool).await?;
            Ok(rows)
        } else {
            let rows = sqlx::query_as::<_, TokenUsage>(
                "SELECT * FROM token_usage ORDER BY date DESC, hour DESC"
            ).fetch_all(&self.pool).await?;
            Ok(rows)
        }
    }
    
    #[allow(dead_code)]
    pub async fn get_all_vendor_status(&self) -> Result<Vec<VendorStatusRecord>, sqlx::Error> {
        let rows = sqlx::query_as::<_, VendorStatusRecord>(
            "SELECT * FROM vendor_status ORDER BY vendor"
        ).fetch_all(&self.pool).await?;
        Ok(rows)
    }
}

use crate::error::NexusFlowError;

#[derive(Clone)]
pub struct Database {
    #[allow(dead_code)]
    pub pool: sqlx::sqlite::SqlitePool,
    pub repository: std::sync::Arc<Repository>,
}

impl Database {
    pub async fn new(path: &str, pool_size: u32) -> Result<Self, NexusFlowError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| NexusFlowError::Database { message: format!("创建数据库目录失败: {}", e) })?;
        }
        
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(pool_size)
            .connect(path)
            .await
            .map_err(|e| NexusFlowError::Database { message: format!("连接数据库失败: {}", e) })?;
        
        // 初始化 schema
        let schema = include_str!("schema.sql");
        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(&pool).await
                    .map_err(|e| NexusFlowError::Database { message: format!("初始化 schema 失败: {}", e) })?;
            }
        }
        
        let repository = std::sync::Arc::new(Repository::new(pool.clone()));
        
        Ok(Self { pool, repository })
    }
    
    pub fn repository(&self) -> &Repository {
        &self.repository
    }
    
    #[allow(dead_code)]
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
