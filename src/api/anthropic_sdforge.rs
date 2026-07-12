// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Anthropic API handlers migrated to sdforge
//!
//! This module contains Anthropic-compatible endpoints migrated to sdforge.

use sdforge::prelude::*;
use serde_json::Value;
use crate::{
    api::global_state::get_global_state,
    router::RoutingDecision,
    database::RequestLog,
};

/// Anthropic Messages API
pub async fn messages(body: sdforge::axum::extract::Json<Value>) -> Result<Value, ApiError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let state = get_global_state();

    let model = body.0.get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput {
            message: "Missing 'model' field".to_string(),
            field: Some("model".to_string()),
            value: None,
        })?;

    let decision: RoutingDecision = state.router.route(model).await
        .map_err(|_| ApiError::service_unavailable("Router", None))?;

    let unified_request = state.anthropic_converter.to_unified(body.0.clone())
        .map_err(|_| ApiError::internal_error("Failed to convert request", uuid::Uuid::new_v4().to_string()))?;

    let unified_response = state.vendor_manager.execute_request(
        &decision.vendor.id,
        unified_request
    ).await.map_err(|_| ApiError::service_unavailable(&decision.vendor.id, None))?;

    let response = state.anthropic_converter.from_unified(unified_response)
        .map_err(|_| ApiError::internal_error("Failed to convert response", uuid::Uuid::new_v4().to_string()))?;

    let duration_ms = start_time.elapsed().as_millis() as i64;
    let _ = log_request(&state, &request_id, "anthropic", &decision, &body.0, &response, duration_ms).await;

    Ok(response)
}

async fn log_request(
    state: &std::sync::Arc<crate::ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    response: &Value,
    duration_ms: i64,
) {
    let messages = request.get("messages")
        .map(|m| serde_json::to_string(m).unwrap_or_default())
        .unwrap_or_default();
    let response_str = serde_json::to_string(response).ok();

    let _ = state.database.repository().log_request(&RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/messages".to_string(),
        input_format: input_format.to_string(),
        abstract_model: request.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
        messages,
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(200),
        response: response_str,
        error_message: None,
        prompt_tokens: request.get("max_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
        completion_tokens: None,
        total_tokens: None,
        duration_ms: Some(duration_ms),
    }).await;
}
