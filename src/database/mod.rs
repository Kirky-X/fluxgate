// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 数据库模块
//!
//! 提供 SQLite 持久化能力，包含请求日志、Token 使用统计、厂商状态记录等数据结构，
//! 以及 Repository（数据访问）和 Database（连接管理）两个核心组件。

use serde::{Deserialize, Serialize};

pub mod async_logger;
#[allow(clippy::module_inception)]
pub mod database;
pub mod pre_aggregator;
pub mod repository;

pub use async_logger::{AsyncLogger, AsyncLoggerHandle};
pub use database::Database;
pub use pre_aggregator::{TokenPreAggregator, TokenPreAggregatorHandle};
pub use repository::Repository;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub cost_usd_sum: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: u32,
}
