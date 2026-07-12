// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use axum::{Json, extract::State, response::IntoResponse, http::HeaderMap};
use serde_json::{Value, json};
use std::sync::Arc;
use crate::{ApiState, error::ProxyError, router::RoutingDecision};
use crate::database::RequestLog;
use crate::protocol::embedding_converter::embedding_request_to_openai;
use crate::api::routing_hints::RoutingHints;
use futures::StreamExt;

#[axum::debug_handler]
pub async fn chat_completions(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, ProxyError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body.get("model")
        .and_then(|v| v.as_str())
        .ok_or(ProxyError::Validation { field: "model".into() })?;

    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 提取路由提示
    let hints = RoutingHints::from_headers(&headers);

    // 路由决策（支持 X-Vendor 和 X-Model 请求头）
    let decision = if hints.has_hints() {
        state.router.route_with_hints(
            model,
            hints.preferred_vendor.as_deref(),
            hints.preferred_model.as_deref(),
        ).await?
    } else {
        state.router.route(model).await?
    };

    // 检查是否可以使用零拷贝透传（入站格式 OpenAI == 出站格式 OpenAI）
    let use_passthrough = decision.vendor.type_ == "openai";

    if use_passthrough {
        // 零拷贝路径：直接透传请求体
        if stream {
            let stream_response = state.vendor_manager.execute_passthrough_stream_request(
                &decision.vendor.id,
                body.clone(),
            ).await?;

            let body_stream = axum::body::Body::from_stream(stream_response.map(|res| {
                res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            }));

            let duration_ms = start_time.elapsed().as_millis() as i64;
            log_chat_request(&state, &request_id, "openai", &decision, &body, &json!({"stream": true}), duration_ms).await;

            return Ok((
                [
                    ("Content-Type", "text/event-stream"),
                    ("Cache-Control", "no-cache"),
                    ("Connection", "keep-alive"),
                ],
                body_stream
            ).into_response());
        }

        // 普通请求 - 零拷贝透传
        let response = state.vendor_manager.execute_passthrough_request(
            &decision.vendor.id,
            body.clone(),
        ).await?;

        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_chat_request(&state, &request_id, "openai", &decision, &body, &response, duration_ms).await;

        return Ok(([("Content-Type", "application/json")], Json(response)).into_response());
    }

    // 转换路径：OpenAI → Unified → 目标厂商
    let unified_request = state.openai_converter.to_unified(body.clone())?;

    if stream {
        let stream_response = state.vendor_manager.execute_stream_request(
            &decision.vendor.id,
            unified_request
        ).await?;

        let body_stream = axum::body::Body::from_stream(stream_response.map(|res| {
            res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        }));

        // 记录流式请求日志 (简化)
        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_chat_request(&state, &request_id, "openai", &decision, &body, &json!({"stream": true}), duration_ms).await;

        return Ok((
            [
                ("Content-Type", "text/event-stream"),
                ("Cache-Control", "no-cache"),
                ("Connection", "keep-alive"),
            ],
            body_stream
        ).into_response());
    }

    // 普通请求 - 通过 UnifiedRequest
    let unified_response = state.vendor_manager.execute_request(
        &decision.vendor.id,
        unified_request
    ).await?;

    // 将统一响应转换回 OpenAI 格式
    let response = state.openai_converter.from_unified(unified_response)?;

    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    log_chat_request(&state, &request_id, "openai", &decision, &body, &response, duration_ms).await;

    Ok(([("Content-Type", "application/json")], Json(response)).into_response())
}

async fn log_chat_request(
    state: &Arc<ApiState>,
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

#[axum::debug_handler]
pub async fn embeddings(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, ProxyError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();
    
    // 路由决策 - embedding 使用特殊路由
    let model = body.get("model")
        .and_then(|v| v.as_str())
        .ok_or(ProxyError::Validation { field: "model".into() })?;
    
    let decision = state.router.route(model).await?;
    
    // 只支持 OpenAI vendor
    if decision.vendor.type_ != "openai" {
        return Err(ProxyError::Protocol {
            source: "Embeddings only supported for OpenAI vendors".into(),
            inner: None,
        });
    }
    
    // 获取 API key
    let api_key = std::env::var(&decision.vendor.api_key_env)
        .map_err(|_| ProxyError::Config { message: format!("API key env {} not set", decision.vendor.api_key_env) })?;
    
    // 转换为 OpenAI 格式
    let openai_body = embedding_request_to_openai(body.clone())?;
    
    // 调用 OpenAI API
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
        .map_err(|e| ProxyError::VendorUnavailable { vendor: decision.vendor.id.clone() })?;
    
    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(ProxyError::Protocol { 
            source: format!("OpenAI Embeddings API error: {}", error_text), 
            inner: None 
        });
    }
    
    let bytes = response.bytes().await
        .map_err(|e| ProxyError::Protocol { source: format!("Failed to read response: {}", e), inner: None })?;
    
    let response_json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ProxyError::Protocol { source: format!("Invalid JSON: {}", e), inner: None })?;
    
    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    let input_str = body.get("input")
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
    
    Ok(Json(response_json))
}
