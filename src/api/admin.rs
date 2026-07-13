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
    let status = state.vendor_manager.get_status(&id);
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
    state.config.update_config(config)?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Config updated"
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
    let mut config = state.config.get_config();
    if let Some(idx) = config.vendors.iter().position(|v| v.id == id) {
        config.vendors[idx] = vendor;
        state.config.update_config(config)?;
        Ok(Json(serde_json::json!({
            "success": true,
            "message": "Vendor updated"
        })))
    } else {
        Err(NexusFlowError::Config {
            message: format!("Vendor {} not found", id),
        })
    }
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
    let mut config = state.config.get_config();
    // Currently only one global model supported?
    // PRD says "Global Model Mapping" but Config struct has `global_model: Option<GlobalModelConfig>`
    // The `GlobalModelConfig` struct doesn't have an ID, it just has enabled/strategy.
    // Wait, let's check config/mod.rs again.

    // Config struct:
    // pub global_model: Option<GlobalModelConfig>,

    // GlobalModelConfig struct:
    // pub enabled: bool,
    // pub strategy: String,
    // It DOES NOT have model_id.

    // However, VendorConfig has models: Vec<ModelConfig>
    // ModelConfig has model_id.

    // The previous code tried to access `config.global_models` (plural) which doesn't exist.
    // And tried to filter by `model_id` which GlobalModelConfig doesn't have.

    // It seems `update_model` was intended to update Vendor Models?
    // Or maybe Global Model Settings?

    // If it's for Vendor Models, we need vendor_id AND model_id.
    // But the path is just `Path(id)`.

    // If it's for Global Model Config, then `id` is irrelevant unless it's a singleton resource "global".

    // Let's assume `update_model` is meant to update a specific model configuration within a vendor.
    // But which vendor?
    // The API signature `update_model` suggests we are updating a model by ID.
    // But model IDs might not be unique across vendors?
    // Actually, usually they are unique per vendor.

    // Let's look at `get_models`. It returns `Vec<GlobalModelConfig>`.
    // But `GlobalModelConfig` seems to be the "Global Strategy" config, not a list of models.

    // Maybe `get_models` should return ALL models from ALL vendors?
    // If so, the return type should be `Vec<ModelConfig>` (plus vendor info?).

    // Let's look at `get_vendors`. It returns `Vec<VendorConfig>`, which includes models.

    // Re-reading PRD or intent...
    // The user wants to "Fix src/api/admin.rs to use correct Config field for global_model".
    // It seems the Admin API was generated assuming a list of global models, but the Config only has one.

    // If I change `get_models` to return `Option<GlobalModelConfig>` it changes the API contract.
    // But `Vec` is safer.

    // For `update_model`, if we are updating the "Global Model Strategy", the ID might be "global"?

    if id == "global" {
        config.global_model = Some(model);
        state.config.update_config(config)?;
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
