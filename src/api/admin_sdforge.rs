// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Admin API handlers migrated to sdforge
//!
//! This module contains admin endpoints that have been migrated from axum to sdforge's
//! #[service_api] macro system. Handlers use the global state to access ApiState.

use sdforge::prelude::*;
use serde::{Deserialize, Serialize};
use crate::{database::LogFilter, api::global_state::get_global_state};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: Option<u32>,
}

/// Query request logs with optional filters
#[service_api(
    name = "logs",
    version = "v1",
    path = "/admin/logs",
    method = "GET",
    tool_name = "logs",
    tool_name = "logs",
    description = "Query request logs with optional filters"
)]
pub async fn logs(query: LogQuery) -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    let filter = LogFilter {
        start_time: query.start_time,
        end_time: query.end_time,
        vendor: query.vendor,
        model: query.model,
        limit: query.limit.unwrap_or(100),
    };

    let logs: Vec<crate::database::RequestLog> = state.database.repository().query_logs(&filter).await
        .map_err(|_| ApiError::internal_error("Failed to query logs", uuid::Uuid::new_v4().to_string()))?;

    Ok(serde_json::json!(logs))
}

/// Search logs by keyword
#[service_api(
    name = "search_logs",
    version = "v1",
    path = "/admin/logs/search",
    method = "GET",
    description = "Search request logs by keyword"
)]
pub async fn search_logs(query: LogQuery) -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    let keyword = query.vendor.unwrap_or_default();
    let limit = query.limit.unwrap_or(100);

    let logs: Vec<crate::database::RequestLog> = state.database.repository().search_logs(&keyword, limit).await
        .map_err(|_| ApiError::internal_error("Failed to search logs", uuid::Uuid::new_v4().to_string()))?;

    Ok(serde_json::json!(logs))
}

/// Get token usage statistics
#[service_api(
    name = "token_stats",
    version = "v1",
    path = "/admin/stats/tokens",
    method = "GET",
    description = "Get token usage statistics by vendor"
)]
pub async fn token_stats(query: LogQuery) -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    let vendor = query.vendor.as_deref();
    let stats: Vec<crate::database::TokenUsage> = state.database.repository().get_token_stats(vendor).await
        .map_err(|_| ApiError::internal_error("Failed to get token stats", uuid::Uuid::new_v4().to_string()))?;

    Ok(serde_json::json!(stats))
}

/// Get all vendor statuses
#[service_api(
    name = "vendors",
    version = "v1",
    path = "/admin/vendors",
    method = "GET",
    description = "Get status of all vendors"
)]
pub async fn vendors() -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    let statuses = state.vendor_manager.get_all_status();

    let vendors: Vec<serde_json::Value> = statuses
        .iter()
        .map(|(id, status, queue_len)| serde_json::json!({
            "id": id,
            "status": format!("{:?}", status),
            "queue_length": queue_len,
        }))
        .collect();

    Ok(serde_json::json!(vendors))
}

/// Get vendor status by ID
#[service_api(
    name = "vendor_status",
    version = "v1",
    path = "/admin/vendors/:id/status",
    method = "GET",
    description = "Get status of a specific vendor"
)]
pub async fn vendor_status(id: String) -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    let status = state.vendor_manager.get_status(&id);
    let queue_len = state.vendor_manager.get_queue_length(&id);

    Ok(serde_json::json!({
        "id": id,
        "status": format!("{:?}", status),
        "queue_length": queue_len,
    }))
}

/// Reset vendor status
#[service_api(
    name = "reset_vendor",
    version = "v1",
    path = "/admin/vendors/:id/reset",
    method = "POST",
    description = "Reset vendor error status"
)]
pub async fn reset_vendor(id: String) -> Result<serde_json::Value, ApiError> {
    let state = get_global_state();

    state.vendor_manager.reset_status(&id);

    Ok(serde_json::json!({
        "success": true,
        "message": "厂商状态已重置"
    }))
}

use crate::config::VendorConfig;

/// Get all vendor configurations
#[service_api(
    name = "get_vendors",
    version = "v1",
    path = "/admin/vendors",
    method = "GET",
    description = "Get all vendor configurations"
)]
pub async fn get_vendors() -> Result<Vec<VendorConfig>, ApiError> {
    let state = get_global_state();

    let config = state.config.get_config();
    Ok(config.vendors)
}
