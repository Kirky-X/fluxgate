// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Token 使用预聚合器
//!
//! 按小时聚合 Token 使用统计，减少数据库查询压力。

use crate::database::Repository;
use crate::protocol::TokenUsage;
use chrono::Timelike;
use sea_orm::ConnectionTrait;
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
            hour: now.hour(),
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
    cost_usd_sum: f64,
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

    /// 记录 Token 使用（含成本，单位 USD）
    pub async fn record(&self, vendor: &str, model: &str, cost_usd: f64, usage: &TokenUsage) {
        let key = AggregationKey::new(vendor, model);
        let mut guard = self.buffer.lock().await;

        let entry = guard.entry(key).or_insert_with(Default::default);
        entry.request_count += 1;
        entry.total_tokens += usage.total_tokens as u64;
        entry.prompt_tokens += usage.prompt_tokens as u64;
        entry.completion_tokens += usage.completion_tokens as u64;
        entry.cost_usd_sum += cost_usd;
    }

    /// 记录使用（使用原始数值，含成本，单位 USD）
    pub async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
    ) {
        let key = AggregationKey::new(vendor, model);
        let mut guard = self.buffer.lock().await;

        let entry = guard.entry(key).or_insert_with(Default::default);
        entry.request_count += 1;
        entry.total_tokens += total_tokens as u64;
        entry.prompt_tokens += prompt_tokens as u64;
        entry.completion_tokens += completion_tokens as u64;
        entry.cost_usd_sum += cost_usd;
    }

    /// 定期刷新到数据库
    ///
    /// recId=9⑤：增加停机退出条件 —— `shutdown` 置 true 后补一次最终
    /// flush 再退出（原实现 loop 无退出，停机时缓冲内的聚合数据丢失）。
    /// main.rs 在 serve 结束后置位并做最终 flush。
    pub async fn start_flush_worker(self: Arc<Self>, shutdown: Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::Ordering;
        let mut interval = tokio::time::interval(self.flush_interval);

        loop {
            interval.tick().await;
            if shutdown.load(Ordering::Relaxed) {
                // 停机：补最终 flush 后退出
                if let Err(e) = self.flush().await {
                    tracing::error!("Final token aggregation flush failed: {}", e);
                }
                break;
            }
            if let Err(e) = self.flush().await {
                tracing::error!("Failed to flush token aggregation: {}", e);
            }
        }
    }

    /// 刷新缓冲区到数据库
    pub async fn flush(&self) -> Result<(), dbnexus::DbError> {
        let entries: Vec<(AggregationKey, AggregationValue)> = {
            let mut guard = self.buffer.lock().await;
            guard.drain().collect()
        };

        if entries.is_empty() {
            return Ok(());
        }

        let sql = r#"
            INSERT INTO token_usage (vendor, model, date, hour, request_count, total_tokens, prompt_tokens, completion_tokens, cost_usd_sum)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(vendor, model, date, hour)
            DO UPDATE SET
                request_count = request_count + excluded.request_count,
                total_tokens = total_tokens + excluded.total_tokens,
                prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                completion_tokens = completion_tokens + excluded.completion_tokens,
                cost_usd_sum = cost_usd_sum + excluded.cost_usd_sum
        "#;

        for (key, value) in entries {
            let values: Vec<sea_orm::Value> = vec![
                key.vendor.as_str().into(),
                key.model.as_str().into(),
                key.date.as_str().into(),
                (key.hour as i32).into(),
                (value.request_count as i64).into(),
                (value.total_tokens as i64).into(),
                (value.prompt_tokens as i64).into(),
                (value.completion_tokens as i64).into(),
                value.cost_usd_sum.into(),
            ];

            let stmt = sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                sql,
                values,
            );

            let session = self.repository.pool.get_session("admin").await?;
            let conn = session.connection()?;
            conn.execute_raw(stmt).await?;
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

    /// 记录 Token 使用（含成本，单位 USD）
    pub async fn record(&self, vendor: &str, model: &str, cost_usd: f64, usage: &TokenUsage) {
        self.inner.record(vendor, model, cost_usd, usage).await;
    }

    /// 记录使用（使用原始数值，含成本，单位 USD）
    pub async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
    ) {
        self.inner
            .record_raw(
                vendor,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd,
            )
            .await;
    }

    /// 启动刷新任务（recId=9⑤：带停机标志，置位后补最终 flush 并退出）
    pub fn start_flush_worker(self, shutdown: Arc<std::sync::atomic::AtomicBool>) {
        tokio::spawn(async move {
            self.inner.clone().start_flush_worker(shutdown).await;
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
