// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
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
pub mod migration;
pub mod pre_aggregator;
pub mod repository;

pub use async_logger::{AsyncLogger, AsyncLoggerHandle};
pub use database::{redact_db_url, resolve_database_url, Database};
pub use pre_aggregator::{TokenPreAggregator, TokenPreAggregatorHandle};
pub use repository::Repository;

// 请求日志与用量契约（RequestLog/LogFilter/TokenUsageRow）已上收
// crate::storage（N2 存储缝），database 侧经 trait 对象注入 ApiState；
// 此处 re-export 维持 database::X 既有路径可用。TokenUsage 行结构已按
// 改名 TokenUsageRow，与 IR 的 protocol::TokenUsage 消歧。
pub use crate::storage::{
    CostGroupBy, CostUsageRow, KeyedUsage, LogFilter, RequestLog, TokenUsageRow,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorStatusRecord {
    pub vendor: String,
    pub status: String,
    pub last_updated: i64,
    pub window_used_tokens: i64,
    pub window_reset_at: Option<i64>,
    pub error_count: i32,
}
