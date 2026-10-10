// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Token 使用预聚合器
//!
//! 按小时聚合 Token 使用统计，减少数据库查询压力。

use crate::database::Repository;
use crate::protocol::TokenUsage;
use crate::storage::KeyedUsage;
use chrono::Timelike;
use sea_orm::ConnectionTrait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// 聚合键（V2：分账维度含 api_key_hash）
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AggregationKey {
    vendor: String,
    model: String,
    api_key_hash: String,
    date: String,
    hour: u32,
}

impl AggregationKey {
    fn new(vendor: &str, model: &str, api_key_hash: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            vendor: vendor.to_string(),
            model: model.to_string(),
            api_key_hash: api_key_hash.to_string(),
            date: now.format("%Y-%m-%d").to_string(),
            hour: now.hour(),
        }
    }
}

/// 聚合值（V2：四类 token 与分项成本）
#[derive(Debug, Default, Clone)]
struct AggregationValue {
    request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    cache_creation_cost: f64,
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

    /// 记录分账用量（V2：四类 token 与分项成本，含 api_key_hash）。
    pub async fn record_keyed(&self, usage: &KeyedUsage) {
        let key = AggregationKey::new(&usage.vendor, &usage.model, &usage.api_key_hash);
        let mut guard = self.buffer.lock().await;

        let entry = guard.entry(key).or_insert_with(Default::default);
        entry.request_count += 1;
        entry.input_tokens += usage.prompt_tokens as u64;
        entry.output_tokens += usage.completion_tokens as u64;
        entry.cache_read_tokens += usage.cache_read_tokens as u64;
        entry.cache_creation_tokens += usage.cache_creation_tokens as u64;
        entry.input_cost += usage.input_cost;
        entry.output_cost += usage.output_cost;
        entry.cache_read_cost += usage.cache_read_cost;
        entry.cache_creation_cost += usage.cache_creation_cost;
    }

    /// 记录 Token 使用（含成本，单位 USD；V1 兼容面：hash 空串、cache 零、
    /// 组合成本记 input。存量调用方迁移后移除）。
    pub async fn record(&self, vendor: &str, model: &str, cost_usd: f64, usage: &TokenUsage) {
        self.record_keyed(&KeyedUsage {
            vendor: vendor.to_string(),
            model: model.to_string(),
            api_key_hash: String::new(),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_read_tokens: usage.cache_read_tokens.min(u32::MAX as u64) as u32,
            cache_creation_tokens: usage.cache_creation_tokens.min(u32::MAX as u64) as u32,
            input_cost: cost_usd,
            output_cost: 0.0,
            cache_read_cost: 0.0,
            cache_creation_cost: 0.0,
        })
        .await;
    }

    /// 记录使用（使用原始数值，含成本，单位 USD；V1 兼容面，同上）。
    pub async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
    ) {
        let _ = total_tokens;
        self.record_keyed(&KeyedUsage {
            vendor: vendor.to_string(),
            model: model.to_string(),
            api_key_hash: String::new(),
            prompt_tokens,
            completion_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            input_cost: cost_usd,
            output_cost: 0.0,
            cache_read_cost: 0.0,
            cache_creation_cost: 0.0,
        })
        .await;
    }

    /// 定期刷新到数据库
    ///
    /// 增加停机退出条件 —— `shutdown` 置 true 后补一次最终
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
                    tracing::error!(
                        "{}",
                        crate::i18n::t_args(
                            "database-preagg-final-flush-failed",
                            &[("error", e.to_string())]
                        )
                    );
                }
                break;
            }
            if let Err(e) = self.flush().await {
                tracing::error!(
                    "{}",
                    crate::i18n::t_args(
                        "database-preagg-flush-failed",
                        &[("error", e.to_string())]
                    )
                );
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

        let session = self.repository.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = upsert_token_usage_sql(backend);

        for (key, value) in entries {
            let values: Vec<sea_orm::Value> = vec![
                key.vendor.as_str().into(),
                key.model.as_str().into(),
                key.api_key_hash.as_str().into(),
                key.date.as_str().into(),
                (key.hour as i32).into(),
                (value.request_count as i64).into(),
                (value.input_tokens as i64).into(),
                (value.output_tokens as i64).into(),
                (value.cache_read_tokens as i64).into(),
                (value.cache_creation_tokens as i64).into(),
                value.input_cost.into(),
                value.output_cost.into(),
                value.cache_read_cost.into(),
                value.cache_creation_cost.into(),
            ];

            let stmt = sea_orm::Statement::from_sql_and_values(backend, sql, values);
            conn.execute_raw(stmt).await?;
        }

        Ok(())
    }
}

/// token_usage upsert 语句（V2，纯函数）：聚合键含 api_key_hash，四类
/// token 与分项成本各自累加。
/// sqlite（3.24+）与 postgres 共用 `ON CONFLICT DO UPDATE`（`excluded` 引用
/// 新行）；mysql 走 `ON DUPLICATE KEY UPDATE` + `VALUES(col)`。
fn upsert_token_usage_sql(backend: sea_orm::DatabaseBackend) -> &'static str {
    match backend {
        sea_orm::DatabaseBackend::MySql => {
            r#"
            INSERT INTO token_usage (vendor, model, api_key_hash, date, hour, request_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, input_cost, output_cost, cache_read_cost, cache_creation_cost)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON DUPLICATE KEY UPDATE
                request_count = request_count + VALUES(request_count),
                input_tokens = input_tokens + VALUES(input_tokens),
                output_tokens = output_tokens + VALUES(output_tokens),
                cache_read_tokens = cache_read_tokens + VALUES(cache_read_tokens),
                cache_creation_tokens = cache_creation_tokens + VALUES(cache_creation_tokens),
                input_cost = input_cost + VALUES(input_cost),
                output_cost = output_cost + VALUES(output_cost),
                cache_read_cost = cache_read_cost + VALUES(cache_read_cost),
                cache_creation_cost = cache_creation_cost + VALUES(cache_creation_cost)
        "#
        }
        // DatabaseBackend 带 #[non_exhaustive]：其余（sqlite/postgres）ON CONFLICT 通用
        _ => {
            r#"
            INSERT INTO token_usage (vendor, model, api_key_hash, date, hour, request_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, input_cost, output_cost, cache_read_cost, cache_creation_cost)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(vendor, model, api_key_hash, date, hour)
            DO UPDATE SET
                request_count = request_count + excluded.request_count,
                input_tokens = input_tokens + excluded.input_tokens,
                output_tokens = output_tokens + excluded.output_tokens,
                cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
                cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
                input_cost = input_cost + excluded.input_cost,
                output_cost = output_cost + excluded.output_cost,
                cache_read_cost = cache_read_cost + excluded.cache_read_cost,
                cache_creation_cost = cache_creation_cost + excluded.cache_creation_cost
        "#
        }
    }
}

#[cfg(test)]
mod upsert_sql_tests {
    use super::upsert_token_usage_sql;
    use sea_orm::DatabaseBackend;

    /// sqlite/pg 同走 ON CONFLICT（excluded 引用新行），mysql 走
    /// ON DUPLICATE KEY UPDATE；V2 冲突键含 api_key_hash，四类 token 与
    /// 分项成本各自累加。
    #[test]
    fn upsert_dialect_dispatch() {
        for backend in [DatabaseBackend::Sqlite, DatabaseBackend::Postgres] {
            let sql = upsert_token_usage_sql(backend);
            assert!(
                sql.contains("ON CONFLICT(vendor, model, api_key_hash, date, hour)"),
                "{backend:?}"
            );
            assert!(sql.contains("excluded.request_count"), "{backend:?}");
        }
        let sql = upsert_token_usage_sql(DatabaseBackend::MySql);
        assert!(sql.contains("ON DUPLICATE KEY UPDATE"));
        assert!(sql.contains("VALUES(request_count)"));
        // 八个数值列全部自增累加
        for col in [
            "request_count",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "input_cost",
            "output_cost",
            "cache_read_cost",
            "cache_creation_cost",
        ] {
            assert!(sql.contains(&format!("+ VALUES({col})")), "{col}");
        }
        assert!(!sql.contains("ON CONFLICT"));
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

    /// 启动刷新任务（带停机标志，置位后补最终 flush 并退出）
    pub fn start_flush_worker(self, shutdown: Arc<std::sync::atomic::AtomicBool>) {
        tokio::spawn(async move {
            self.inner.clone().start_flush_worker(shutdown).await;
        });
    }
}

#[async_trait::async_trait]
impl crate::storage::UsageSink for TokenPreAggregator {
    async fn record_keyed(
        &self,
        usage: &crate::storage::KeyedUsage,
    ) -> Result<(), crate::error::GateError> {
        TokenPreAggregator::record_keyed(self, usage).await;
        Ok(())
    }

    async fn query_cost_usage(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        group_by: crate::storage::CostGroupBy,
        api_key_hash: Option<&str>,
    ) -> Result<Vec<crate::storage::CostUsageRow>, crate::error::GateError> {
        self.repository
            .query_cost_usage(from, to, group_by, api_key_hash)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("cost usage query failed: {e}"),
            })
    }

    async fn flush(&self) -> Result<(), crate::error::GateError> {
        TokenPreAggregator::flush(self)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("token usage flush failed: {e}"),
            })
    }

    async fn get_token_stats(
        &self,
        vendor: Option<&str>,
    ) -> Result<Vec<crate::storage::TokenUsageRow>, crate::error::GateError> {
        self.repository
            .get_token_stats(vendor)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("token stats query failed: {e}"),
            })
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
            api_key_hash: String::new(),
            date: "2024-01-01".to_string(),
            hour: 10,
        };

        let key2 = AggregationKey {
            vendor: "openai_1".to_string(),
            model: "gpt-4".to_string(),
            api_key_hash: String::new(),
            date: "2024-01-01".to_string(),
            hour: 10,
        };

        assert_eq!(key1, key2);
        assert_eq!(format!("{:?}", key1), format!("{:?}", key2));
    }
}
