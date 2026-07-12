// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! OpenAI API handlers migrated to sdforge
//!
//! This module contains OpenAI-compatible endpoints migrated to sdforge.

use sdforge::prelude::*;
use sdforge::streaming::{create_stream_channel, StreamResponse};
use serde_json::Value;
use futures_util::StreamExt as FutStreamExt;
use crate::{
    api::global_state::get_global_state,
    router::RoutingDecision,
    database::RequestLog,
};

/// OpenAI Chat Completions API with SSE streaming support
pub async fn chat_completions(body: sdforge::axum::extract::Json<Value>) -> Result<impl IntoResponse, ApiError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let state = get_global_state();

    let model = body.0.get("model")
        .and_then(|v: &serde_json::Value| v.as_str())
        .ok_or_else(|| ApiError::InvalidInput {
            message: "Missing 'model' field".to_string(),
            field: Some("model".to_string()),
            value: None,
        })?;

    let stream = body.0.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    let decision: RoutingDecision = state.router.route(model).await
        .map_err(|_| ApiError::service_unavailable("Router", None))?;

    let unified_request = state.openai_converter.to_unified(body.0.clone())
        .map_err(|e| ApiError::internal_error("Failed to convert request", uuid::Uuid::new_v4().to_string()))?;

    let (tx, stream_response) = create_stream_channel::<Value>(100);

    let state_clone = state.clone();
    let decision_clone = decision.clone();
    let body_clone = body.0.clone();
    let request_id_clone = request_id.clone();
    
    tokio::spawn(async move {
        if stream {
            let stream_response_result = state_clone.vendor_manager.execute_stream_request(
                &decision_clone.vendor.id,
                unified_request
            ).await;
            
            match stream_response_result {
                Ok(mut stream) => {
                    let mut log_sent = false;
                    
                    while let Some(result) = FutStreamExt::next(&mut stream).await {
                        match result {
                            Ok(data) => {
                                let json_value: Value = match serde_json::from_slice(&data) {
                                    Ok(v) => v,
                                    Err(_) => {
                                        serde_json::json!({
                                            "raw": String::from_utf8_lossy(&data).to_string()
                                        })
                                    }
                                };
                                let _ = tx.send(Ok(json_value)).await;
                                
                                if !log_sent {
                                    let duration_ms = start_time.elapsed().as_millis() as i64;
                                    let _ = log_chat_request_stream(&state_clone, &request_id_clone, "openai", &decision_clone, &body_clone, duration_ms).await;
                                    log_sent = true;
                                }
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                let _ = tx.send(Err(error_msg)).await;
                                break;
                            }
                        }
                    }
                    
                    let _ = tx.send(Ok(Value::Null)).await;
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    let _ = tx.send(Err(error_msg)).await;
                }
            }
        } else {
            let unified_response_result = state_clone.vendor_manager.execute_request(
                &decision_clone.vendor.id,
                unified_request
            ).await;
            
            match unified_response_result {
                Ok(unified_response) => {
                    let response_result = state_clone.openai_converter.from_unified(unified_response);
                    
                    match response_result {
                        Ok(response) => {
                            let duration_ms = start_time.elapsed().as_millis() as i64;
                            let _ = log_chat_request(&state_clone, &request_id_clone, "openai", &decision_clone, &body_clone, &response, duration_ms).await;
                            let _ = tx.send(Ok(response)).await;
                        }
                        Err(_) => {
                            let _ = tx.send(Err("Failed to convert response".to_string())).await;
                        }
                    }
                }
                Err(_) => {
                    let _ = tx.send(Err("Failed to execute request".to_string())).await;
                }
            }
        }
        
        let _ = tx.send(Ok(Value::Null)).await;
    });
    
    Ok(stream_response)
}

async fn log_chat_request_stream(
    state: &std::sync::Arc<crate::ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    duration_ms: i64,
) {
    let _ = log_chat_request(state, request_id, input_format, decision, request, &Value::Null, duration_ms).await;
}

async fn log_chat_request(
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
        endpoint: "/v1/chat/completions".to_string(),
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

/// OpenAI Embeddings API
pub async fn embeddings(body: sdforge::axum::extract::Json<Value>) -> Result<Value, ApiError> {
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

    if decision.vendor.type_ != "openai" {
        return Err(ApiError::invalid_input(
            "Embeddings only supported for OpenAI vendors",
            Some("vendor".to_string()),
            Some(serde_json::json!(decision.vendor.type_)),
        ));
    }

    let api_key = std::env::var(&decision.vendor.api_key_env)
        .map_err(|_| ApiError::internal_error(
            &format!("API key env {} not set", decision.vendor.api_key_env),
            uuid::Uuid::new_v4().to_string(),
        ))?;

    let openai_body = crate::protocol::embedding_converter::embedding_request_to_openai(body.0.clone())
        .map_err(|_| ApiError::internal_error("Failed to convert embedding request", uuid::Uuid::new_v4().to_string()))?;

    let api_base = if decision.vendor.api_base.is_empty() {
        "https://api.openai.com/v1"
    } else {
        &decision.vendor.api_base
    };

    let url = format!("{}/embeddings", api_base);

    let response = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&openai_body)
        .send()
        .await
        .map_err(|_| ApiError::service_unavailable("OpenAI API", None))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(ApiError::internal_error(
            &format!("OpenAI Embeddings API error: {}", error_text),
            uuid::Uuid::new_v4().to_string(),
        ));
    }

    let bytes = response.bytes().await
        .map_err(|e| ApiError::internal_error(
            &format!("Failed to read response: {}", e),
            uuid::Uuid::new_v4().to_string(),
        ))?;

    let response_json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::internal_error(
            &format!("Invalid JSON: {}", e),
            uuid::Uuid::new_v4().to_string(),
        ))?;
    
    let duration_ms = start_time.elapsed().as_millis() as i64;
    let input_str = body.0.get("input")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();
    let _ = state.database.repository().log_request(&RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/embeddings".to_string(),
        input_format: "openai".to_string(),
        abstract_model: Some(model.to_string()),
        messages: input_str,
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(200),
        response: serde_json::to_string(&response_json).ok(),
        error_message: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        duration_ms: Some(duration_ms),
    }).await;
    
    Ok(response_json)
}
