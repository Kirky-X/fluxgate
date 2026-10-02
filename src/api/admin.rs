// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{Config, GlobalModelConfig, VendorConfig};
use crate::database::LogFilter;
use crate::{error::NexusFlowError, ApiState};
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(serde::Deserialize)]
pub struct LogQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: Option<u32>,
}

#[axum::debug_handler]
pub async fn logs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let filter = LogFilter {
        start_time: params.start_time,
        end_time: params.end_time,
        vendor: params.vendor,
        model: params.model,
        limit: params.limit.unwrap_or(100),
    };

    let logs = state.database.repository().query_logs(&filter).await?;
    Ok(Json(serde_json::json!(logs)))
}

#[axum::debug_handler]
pub async fn search_logs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let keyword = params.vendor.unwrap_or_default();
    let limit = params.limit.unwrap_or(100);

    let logs = state
        .database
        .repository()
        .search_logs(&keyword, limit)
        .await?;
    Ok(Json(serde_json::json!(logs)))
}

#[axum::debug_handler]
pub async fn token_stats(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let vendor = params.vendor.as_deref();
    let stats = state.database.repository().get_token_stats(vendor).await?;
    Ok(Json(serde_json::json!(stats)))
}

/// 全局指标快照 — 返回请求计数、错误率、平均延迟、各厂商细分指标，以及
/// recId=9① 的日志背压丢弃计数（dropped_logs：channel 满被丢的日志条数）
#[axum::debug_handler]
pub async fn metrics(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, NexusFlowError> {
    let snapshot = state.vendor_manager.metrics().snapshot();
    let mut value = serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
    value["dropped_logs"] =
        serde_json::json!(state.logger.as_ref().map(|l| l.dropped_logs()).unwrap_or(0));
    Ok(Json(value))
}

#[axum::debug_handler]
pub async fn vendors(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, NexusFlowError> {
    let statuses = state.vendor_manager.get_all_status();

    let vendors: Vec<Value> = statuses
        .iter()
        .map(|(id, status, queue_len)| {
            serde_json::json!({
                "id": id,
                "status": format!("{:?}", status),
                "queue_length": queue_len,
            })
        })
        .collect();

    Ok(Json(serde_json::json!(vendors)))
}

#[axum::debug_handler]
pub async fn vendor_status(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, NexusFlowError> {
    let status = state.vendor_manager.get_status(&id).await;
    let queue_len = state.vendor_manager.get_queue_length(&id);

    Ok(Json(serde_json::json!({
        "id": id,
        "status": format!("{:?}", status),
        "queue_length": queue_len,
    })))
}

#[axum::debug_handler]
pub async fn reset_vendor(
    State(_state): State<Arc<ApiState>>,
    Path(_id): Path<String>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // limiteron Governor 自动管理限流状态（令牌桶自动 refill），
    // 此端点保留以维持 API 兼容性，实际为 no-op。
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "厂商限流状态由 limiteron Governor 自动管理"
    })))
}

// Config Admin Functions
#[axum::debug_handler]
pub async fn get_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Config>, NexusFlowError> {
    let config = state.config.get_config();
    Ok(Json(config))
}

#[axum::debug_handler]
pub async fn update_config(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<Config>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 对比建站期消费的段（server/cors 在 main.rs 建站时被 sdforge 固化，
    // 运行期不可热换——axum Router 的 layer 在构造时编译固化，sdforge 无
    // 热重载中间件能力），变更时显式告知需重启，消除"静默失效"误判。
    let old = state.config.get_config();
    let mut restart_required: Vec<&'static str> = Vec::new();
    if old.server != config.server {
        restart_required.push("server");
    }
    if old.cors != config.cors {
        restart_required.push("cors");
    }
    state.config.update_config(config)?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": if restart_required.is_empty() {
            "Config updated; vendors/pricing/auth take effect immediately".to_string()
        } else {
            format!(
                "Config updated; vendors/pricing/auth take effect immediately; \
                 {} take effect after restart",
                restart_required.join(", ")
            )
        },
        "restart_required": restart_required,
    })))
}

#[axum::debug_handler]
pub async fn get_vendors(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<VendorConfig>>, NexusFlowError> {
    let config = state.config.get_config();
    Ok(Json(config.vendors))
}

#[axum::debug_handler]
pub async fn update_vendor(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(vendor): Json<VendorConfig>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 原子 RMW：读-改-校验-落盘在 ConfigManager 写锁内完成，
    // 并发与其他 update_* 端点互斥，不丢失更新
    state.config.update_config_with(|config| {
        match config.vendors.iter().position(|v| v.id == id) {
            Some(idx) => {
                config.vendors[idx] = vendor;
                Ok(())
            }
            None => Err(NexusFlowError::Config {
                message: format!("Vendor {} not found", id),
            }),
        }
    })?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Vendor updated"
    })))
}

#[axum::debug_handler]
pub async fn get_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<GlobalModelConfig>>, NexusFlowError> {
    let config = state.config.get_config();
    // Config only has Option<GlobalModelConfig> (singular)
    // But API expects Vec for extensibility or maybe misunderstanding?
    // Let's assume we return a Vec containing the single global model if it exists
    let models = if let Some(gm) = config.global_model {
        vec![gm]
    } else {
        vec![]
    };
    Ok(Json(models))
}

#[axum::debug_handler]
pub async fn update_model(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(model): Json<GlobalModelConfig>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // Config 只有单例 global_model: Option<GlobalModelConfig>（无 ID 字段），
    // 约定 id == "global" 更新全局模型策略
    if id == "global" {
        // 原子 RMW（同 update_vendor）：读-改-校验-落盘在写锁内完成
        state.config.update_config_with(|config| {
            config.global_model = Some(model);
            Ok(())
        })?;
        Ok(Json(serde_json::json!({
            "success": true,
            "message": "Global model config updated"
        })))
    } else {
        Err(NexusFlowError::Config {
            message: "Only 'global' model config is supported".into(),
        })
    }
}
