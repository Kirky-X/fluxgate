use axum::{Json, extract::State, response::IntoResponse, http::HeaderMap};
use serde_json::{Value, json};
use std::sync::Arc;
use crate::{ApiState, error::ProxyError, router::RoutingDecision};
use crate::database::RequestLog;
use futures::StreamExt;

#[axum::debug_handler]
pub async fn messages(
    State(state): State<Arc<ApiState>>,
    _headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, ProxyError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body.get("model")
        .and_then(|v| v.as_str())
        .ok_or(ProxyError::Validation { field: "model".into() })?;

    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 路由决策
    let decision = state.router.route(model).await?;

    // 检查是否可以使用零拷贝透传（入站格式 Anthropic == 出站格式 Anthropic）
    let use_passthrough = decision.vendor.type_ == "anthropic";

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
            log_request(&state, &request_id, "anthropic", &decision, &body, &json!({"stream": true}), duration_ms).await;

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
        log_request(&state, &request_id, "anthropic", &decision, &body, &response, duration_ms).await;

        return Ok(([("Content-Type", "application/json")], Json(response)).into_response());
    }

    // 转换路径：Anthropic → Unified → 目标厂商
    let unified_request = state.anthropic_converter.to_unified(body.clone())?;

    if stream {
        let stream_response = state.vendor_manager.execute_stream_request(
            &decision.vendor.id,
            unified_request
        ).await?;

        let body_stream = axum::body::Body::from_stream(stream_response.map(|res| {
            res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        }));

        // 记录流式请求日志
        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_request(&state, &request_id, "anthropic", &decision, &body, &json!({"stream": true}), duration_ms).await;

        return Ok((
            [
                ("Content-Type", "text/event-stream"),
                ("Cache-Control", "no-cache"),
                ("Connection", "keep-alive"),
            ],
            body_stream
        ).into_response());
    }

    // 通过 VendorManager 执行请求
    let unified_response = state.vendor_manager.execute_request(
        &decision.vendor.id,
        unified_request
    ).await?;

    // 将统一响应转换回 Anthropic 格式
    let response = state.anthropic_converter.from_unified(unified_response)?;

    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    log_request(&state, &request_id, "anthropic", &decision, &body, &response, duration_ms).await;

    Ok(([("Content-Type", "application/json")], Json(response)).into_response())
}

async fn log_request(
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
