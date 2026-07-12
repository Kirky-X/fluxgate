// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Token 使用预聚合器
//!
//! 按小时聚合 Token 使用统计，减少数据库查询压力。

use crate::database::Repository;
use crate::protocol::unified::TokenUsage;
use chrono::Timelike;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// 聚合键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AggregationKey {
    vendor: String,
    model: String,
    date: String,
    hour: u32,
}

impl AggregationKey {
    fn new(vendor: &str, model: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            vendor: vendor.to_string(),
            model: model.to_string(),
            date: now.format("%Y-%m-%d").to_string(),
            hour: now.hour() as u32,
        }
    }
}

/// 聚合值
#[derive(Debug, Default, Clone)]
struct AggregationValue {
    request_count: u64,
    total_tokens: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
}

/// Token 使用预聚合器
pub struct TokenPreAggregator {
    repository: Arc<Repository>,
    buffer: Arc<Mutex<HashMap<AggregationKey, AggregationValue>>>,
    flush_interval: Duration,
}

impl TokenPreAggregator {
    /// 创建新的预聚合器
    pub fn new(repository: Arc<Repository>, flush_interval: Duration) -> Self {
        Self {
            repository,
            buffer: Arc::new(Mutex::new(HashMap::new())),
            flush_interval,
        }
    }

    /// 记录 Token 使用
    pub async fn record(&self, vendor: &str, model: &str, usage: &TokenUsage) {
        let key = AggregationKey::new(vendor, model);
        let mut guard = self.buffer.lock().await;

        let entry = guard.entry(key).or_insert_with(Default::default);
        entry.request_count += 1;
        entry.total_tokens += usage.total_tokens as u64;
        entry.prompt_tokens += usage.prompt_tokens as u64;
        entry.completion_tokens += usage.completion_tokens as u64;
    }

    /// 记录使用（使用原始数值）
    pub async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    ) {
        let key = AggregationKey::new(vendor, model);
        let mut guard = self.buffer.lock().await;

        let entry = guard.entry(key).or_insert_with(Default::default);
        entry.request_count += 1;
        entry.total_tokens += total_tokens as u64;
        entry.prompt_tokens += prompt_tokens as u64;
        entry.completion_tokens += completion_tokens as u64;
    }

    /// 定期刷新到数据库
    pub async fn start_flush_worker(self: Arc<Self>) {
        let mut interval = tokio::time::interval(self.flush_interval);

        loop {
            interval.tick().await;
            if let Err(e) = self.flush().await {
                tracing::error!("Failed to flush token aggregation: {}", e);
            }
        }
    }

    /// 刷新缓冲区到数据库
    pub async fn flush(&self) -> Result<(), sqlx::Error> {
        let entries: Vec<(AggregationKey, AggregationValue)> = {
            let mut guard = self.buffer.lock().await;
            guard.drain().collect()
        };

        if entries.is_empty() {
            return Ok(());
        }

        for (key, value) in entries {
            // UPSERT 到数据库
            sqlx::query(
                r#"
                INSERT INTO token_usage (vendor, model, date, hour, request_count, total_tokens, prompt_tokens, completion_tokens)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(vendor, model, date, hour)
                DO UPDATE SET
                    request_count = request_count + excluded.request_count,
                    total_tokens = total_tokens + excluded.total_tokens,
                    prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                    completion_tokens = completion_tokens + excluded.completion_tokens
                "#,
            )
            .bind(&key.vendor)
            .bind(&key.model)
            .bind(&key.date)
            .bind(key.hour as i32)
            .bind(value.request_count as i64)
            .bind(value.total_tokens as i64)
            .bind(value.prompt_tokens as i64)
            .bind(value.completion_tokens as i64)
            .execute(&self.repository.pool)
            .await?;
        }

        Ok(())
    }
}

/// Token 预聚合器句柄
#[derive(Clone)]
pub struct TokenPreAggregatorHandle {
    inner: Arc<TokenPreAggregator>,
}

impl TokenPreAggregatorHandle {
    /// 创建新的句柄
    pub fn new(aggregator: Arc<TokenPreAggregator>) -> Self {
        Self { inner: aggregator }
    }

    /// 记录 Token 使用
    pub async fn record(&self, vendor: &str, model: &str, usage: &TokenUsage) {
        self.inner.record(vendor, model, usage).await;
    }

    /// 记录使用（使用原始数值）
    pub async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    ) {
        self.inner
            .record_raw(
                vendor,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
            )
            .await;
    }

    /// 启动刷新任务
    pub fn start_flush_worker(self) {
        tokio::spawn(async move {
            self.inner.clone().start_flush_worker().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aggregation_key_equality() {
        let key1 = AggregationKey {
            vendor: "openai_1".to_string(),
            model: "gpt-4".to_string(),
            date: "2024-01-01".to_string(),
            hour: 10,
        };

        let key2 = AggregationKey {
            vendor: "openai_1".to_string(),
            model: "gpt-4".to_string(),
            date: "2024-01-01".to_string(),
            hour: 10,
        };

        assert_eq!(key1, key2);
        assert_eq!(format!("{:?}", key1), format!("{:?}", key2));
    }
}
