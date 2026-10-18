// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Gemini 原生入站端点（`/v1beta/models/{model}:{action}`）。
//!
//! 默认把请求转成 IR 走既有路由池；`safetySettings`/`cachedContent`/内置工具
//! 等不可降级字段触发原生直通（仅 `type_ = "gemini"` 厂商）。协议纯函数在
//! [`crate::protocol::gemini`]。

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::{FluxgateError, GateError};
use crate::i18n::{t, t_args};
use crate::protocol::gemini::types::{normalize_model, GeminiAction};
use crate::protocol::gemini::{convert_in, convert_out, GeminiStreamEncoder};
use crate::protocol::StreamChunk;
use crate::router::RoutingDecision;
use crate::ApiState;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 流式响应公共头。
const STREAM_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
];

/// 单请求横切上下文。
struct GeminiCtx {
    request_id: String,
    start_time: std::time::Instant,
    model: String,
    api_key_hash: String,
    session_key: Option<String>,
    preferred_vendor: Option<String>,
    summary: String,
}

#[axum::debug_handler]
pub async fn dispatch(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Query(_query): Query<HashMap<String, String>>,
    Path(model_action): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    match dispatch_inner(state, headers, model_action, body).await {
        Ok(response) => response,
        Err(error) => gemini_error(error),
    }
}

async fn dispatch_inner(
    state: Arc<ApiState>,
    headers: HeaderMap,
    model_action: String,
    body: Value,
) -> Result<Response, FluxgateError> {
    let (model_raw, action_raw) = model_action
        .split_once(':')
        .ok_or_else(|| validation("api-gemini-action-required"))?;
    let action = GeminiAction::parse(action_raw).ok_or_else(|| GateError::Validation {
        field: t_args(
            "api-gemini-action-unknown",
            &[("action", action_raw.to_string())],
        ),
    })?;
    let model = normalize_model(model_raw).to_string();
    if model.is_empty() {
        return Err(validation("api-gemini-model-required").into());
    }

    state.vendor_manager.metrics().record_gemini_request();
    match action {
        GeminiAction::GenerateContent => generate_content(state, headers, model, body).await,
        GeminiAction::StreamGenerateContent => {
            stream_generate_content(state, headers, model, body).await
        }
        GeminiAction::CountTokens => count_tokens(state, body),
        GeminiAction::EmbedContent => embed_content(state, headers, model, body).await,
        GeminiAction::BatchEmbedContents => batch_embed_contents(state, headers, model, body).await,
    }
}

/// `GET /v1beta/models`：启用厂商模型的公开名（abstract_id 优先）去重清单。
pub async fn list_models(State(state): State<Arc<ApiState>>) -> Response {
    Json(json!({"models": model_entries(&state)})).into_response()
}

/// `GET /v1beta/models/{model}`：单模型元数据。
pub async fn get_model(State(state): State<Arc<ApiState>>, Path(model): Path<String>) -> Response {
    let model = normalize_model(&model);
    let entries = model_entries(&state);
    match entries
        .iter()
        .find(|entry| entry["name"].as_str() == Some(&format!("models/{model}")))
    {
        Some(entry) => Json(entry.clone()).into_response(),
        None => gemini_not_found(&format!("models/{model}")),
    }
}

fn model_entries(state: &Arc<ApiState>) -> Vec<Value> {
    let config = state.config.read();
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for vendor in config.vendors.iter().filter(|vendor| vendor.enabled) {
        for model in vendor.models.iter().filter(|model| model.enabled) {
            let task = model.model_type.as_deref().unwrap_or("chat");
            // rerank 无 Gemini 对应方法，不进入 Gemini 模型清单
            if task == "rerank" {
                continue;
            }
            let public = model.abstract_id.as_deref().unwrap_or(&model.model_id);
            if !seen.insert(public.to_ascii_lowercase()) {
                continue;
            }
            let methods: &[&str] = match task {
                "embedding" => &["embedContent", "batchEmbedContents"],
                _ => &["generateContent", "streamGenerateContent", "countTokens"],
            };
            entries.push(json!({
                "name": format!("models/{public}"),
                "displayName": public,
                "supportedGenerationMethods": methods,
            }));
        }
    }
    entries
}

/// `:countTokens`：本地估算（不发上游），字符/4 + 结构开销。
fn count_tokens(state: Arc<ApiState>, body: Value) -> Result<Response, FluxgateError> {
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .filter(|contents| !contents.is_empty())
        .ok_or_else(|| validation("api-gemini-contents-invalid"))?;
    let chars = contents
        .iter()
        .map(|content| parts_text_chars(content.get("parts")))
        .sum::<usize>()
        + parts_text_chars(
            body.get("systemInstruction")
                .or_else(|| body.get("system_instruction"))
                .and_then(|system| system.get("parts")),
        )
        + body
            .get("tools")
            .and_then(|tools| serde_json::to_string(tools).ok())
            .map(|json| json.chars().count())
            .unwrap_or(0);
    let cpt = state.config.read().quota.est_chars_per_token.max(1) as usize;
    let total = chars.div_ceil(cpt).max(1);
    Ok(Json(json!({"totalTokens": total})).into_response())
}

async fn generate_content(
    state: Arc<ApiState>,
    headers: HeaderMap,
    model: String,
    body: Value,
) -> Result<Response, FluxgateError> {
    let ctx = make_ctx(&state, &headers, &model, &body, false);
    if gemini_native_only(&body) {
        return native_nonstream(state, headers, model, body, ctx).await;
    }

    let unified = convert_in::to_unified_request(&body, &model, false)?;
    let hints = RoutingHints::from_headers(&headers);
    let decision = resolve_route(
        &state,
        &model,
        &headers,
        hints,
        ctx.session_key.as_deref(),
        &ctx.api_key_hash,
        &body,
    )
    .await?;
    let (unified_response, decision) =
        dispatch_nonstream(&state, &ctx, &model, decision, unified).await?;
    let payload = convert_out::response_to_gemini(&unified_response);
    crate::api::bind_session_affinity(
        &state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    settle_nonstream(&state, &ctx, &body, &payload, &decision).await;
    log_request(&state, &ctx, &decision, Some(&payload), 200, None).await;
    Ok(with_route_headers(Json(payload).into_response(), &decision))
}

async fn stream_generate_content(
    state: Arc<ApiState>,
    headers: HeaderMap,
    model: String,
    body: Value,
) -> Result<Response, FluxgateError> {
    let ctx = make_ctx(&state, &headers, &model, &body, true);
    if gemini_native_only(&body) {
        return native_stream(state, headers, model, body, ctx).await;
    }
    let unified = convert_in::to_unified_request(&body, &model, true)?;
    let hints = RoutingHints::from_headers(&headers);
    let decision = resolve_route(
        &state,
        &model,
        &headers,
        hints,
        ctx.session_key.as_deref(),
        &ctx.api_key_hash,
        &body,
    )
    .await?;
    stream_ir(&state, &ctx, &model, body, decision, unified).await
}

/// 原生直通非流式：仅 `type_="gemini"` 厂商，body 原样。
async fn native_nonstream(
    state: Arc<ApiState>,
    _headers: HeaderMap,
    model: String,
    body: Value,
    ctx: GeminiCtx,
) -> Result<Response, FluxgateError> {
    ensure_native_vendor(&state, &ctx)?;
    let decision = state
        .router
        .route_gemini_native(&model, ctx.session_key.as_deref())
        .await?;
    let path = format!("models/{model}:generateContent");
    let payload = state
        .vendor_manager
        .execute_passthrough_request_at(&decision.vendor.id, body.clone(), &path)
        .await;
    let payload = match payload {
        Ok(payload) => payload,
        Err(error) => {
            state
                .vendor_manager
                .metrics()
                .record_gemini_upstream_failure();
            let error = FluxgateError::from(error);
            log_failure(&state, &ctx, &decision, &error).await;
            return Err(error);
        }
    };
    crate::api::bind_session_affinity(
        &state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    settle_nonstream(&state, &ctx, &body, &payload, &decision).await;
    log_request(&state, &ctx, &decision, Some(&payload), 200, None).await;
    Ok(with_route_headers(Json(payload).into_response(), &decision))
}

/// 原生直通流式：字节流透传 + usage 结算。
async fn native_stream(
    state: Arc<ApiState>,
    _headers: HeaderMap,
    model: String,
    body: Value,
    ctx: GeminiCtx,
) -> Result<Response, FluxgateError> {
    ensure_native_vendor(&state, &ctx)?;
    let decision = state
        .router
        .route_gemini_native(&model, ctx.session_key.as_deref())
        .await?;
    let path = format!("models/{model}:streamGenerateContent?alt=sse");
    let upstream = state
        .vendor_manager
        .execute_passthrough_stream_request_at(&decision.vendor.id, body.clone(), &path)
        .await;
    let upstream = match upstream {
        Ok(stream) => stream,
        Err(error) => {
            state
                .vendor_manager
                .metrics()
                .record_gemini_upstream_failure();
            let error = FluxgateError::from(error);
            log_failure(&state, &ctx, &decision, &error).await;
            return Err(error);
        }
    };
    log_request(
        &state,
        &ctx,
        &decision,
        Some(&json!({"stream": true})),
        200,
        None,
    )
    .await;
    crate::api::bind_session_affinity(
        &state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    let accounting = accounting_body(&body, &model);
    let body_stream = crate::middleware::quota::passthrough_stream_body(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        &accounting,
        "gemini",
        upstream,
    );
    Ok(with_route_headers(
        (STREAM_HEADERS, body_stream).into_response(),
        &decision,
    ))
}

/// IR 流式：IR chunk → Gemini SSE，终止帧去重与 usage 结算。
async fn stream_ir(
    state: &Arc<ApiState>,
    ctx: &GeminiCtx,
    model: &str,
    body: Value,
    decision: RoutingDecision,
    unified: crate::protocol::UnifiedRequest,
) -> Result<Response, FluxgateError> {
    #[cfg(feature = "resilience")]
    let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
        state.config.read().resilience.expose_fallback_headers,
    );

    #[cfg(feature = "resilience")]
    let outcome = if state.config.read().resilience.cover_all_paths {
        crate::api::execute_dispatch_with_fallback(
            state,
            model,
            decision.clone(),
            false,
            |d| {
                let vendor_manager = state.vendor_manager.clone();
                let unified = unified.clone();
                async move {
                    vendor_manager
                        .execute_stream_request(&d.vendor.id, unified)
                        .await
                }
            },
            &mut fallback_trail,
        )
        .await
        .map_err(|e| Box::new((e, decision)))
    } else {
        fallback_trail.record(&decision.vendor.id);
        match state
            .vendor_manager
            .execute_stream_request(&decision.vendor.id, unified)
            .await
        {
            Ok(stream) => Ok((stream, decision)),
            Err(e) => Err(Box::new((e, decision))),
        }
    };
    #[cfg(not(feature = "resilience"))]
    let outcome = match state
        .vendor_manager
        .execute_stream_request(&decision.vendor.id, unified)
        .await
    {
        Ok(stream) => Ok((stream, decision)),
        Err(e) => Err(Box::new((e, decision))),
    };

    let (chunk_stream, decision) = match outcome {
        Ok(pair) => pair,
        Err(boxed) => {
            let (error, decision) = *boxed;
            let error = FluxgateError::from(error);
            log_failure(state, ctx, &decision, &error).await;
            return Err(error);
        }
    };

    let accounting = accounting_body(&body, model);
    let settle_ctx = crate::middleware::quota::SettlementCtx::for_request(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        &accounting,
        &decision.vendor.type_,
        &decision.vendor.id,
        Some(Arc::clone(state.vendor_manager.metrics())),
    );
    let chunk_stream = crate::middleware::quota::settle_stream(chunk_stream, settle_ctx);
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    log_request(
        state,
        ctx,
        &decision,
        Some(&json!({"stream": true})),
        200,
        None,
    )
    .await;

    #[cfg(feature = "resilience")]
    let obs_headers = crate::api::fallback_observation_headers(&decision, &fallback_trail);
    #[cfg(not(feature = "resilience"))]
    let obs_headers = axum::http::HeaderMap::new();
    let body_stream = gemini_sse_stream(chunk_stream, state.vendor_manager.metrics().clone());
    Ok(crate::api::with_fallback_headers(
        (STREAM_HEADERS, body_stream).into_response(),
        &obs_headers,
    ))
}

fn gemini_sse_stream<S>(stream: S, metrics: Arc<crate::utils::metrics::GlobalMetrics>) -> Body
where
    S: futures::Stream<Item = Result<StreamChunk, GateError>> + Send + 'static,
{
    let sse_stream = async_stream::stream! {
        use futures::StreamExt as _;
        let mut encoder = GeminiStreamEncoder::new();
        let mut stream = Box::pin(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    for frame in encoder.encode(chunk) {
                        yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(sse_frame(&frame)));
                    }
                }
                Err(error) => {
                    metrics.record_gemini_upstream_failure();
                    let body = json!({"error": {
                        "code": 502,
                        "message": t_args("api-gemini-stream-failed", &[("error", error.to_string())]),
                        "status": "UNAVAILABLE",
                    }});
                    yield Ok(bytes::Bytes::from(sse_frame(&body)));
                    break;
                }
            }
        }
        for frame in encoder.finish() {
            yield Ok(bytes::Bytes::from(sse_frame(&frame)));
        }
    };
    Body::from_stream(sse_stream)
}

fn sse_frame(value: &Value) -> String {
    format!("data: {value}\n\n")
}

/// 原生直通判定：不可等价降级的 Gemini 专有字段/工具。
fn gemini_native_only(body: &Value) -> bool {
    body.get("safetySettings").is_some()
        || body.get("safety_settings").is_some()
        || body.get("cachedContent").is_some()
        || body.get("cached_content").is_some()
        || body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool.get("functionDeclarations").is_none())
            })
}

/// X-Vendor 点名必须是 Gemini 厂商；否则不发请求直接 400。
fn ensure_native_vendor(state: &Arc<ApiState>, ctx: &GeminiCtx) -> Result<(), FluxgateError> {
    let Some(vendor) = ctx.preferred_vendor.as_deref() else {
        return Ok(());
    };
    let capable = state.config.read().vendors.iter().any(|entry| {
        entry.enabled && entry.id.eq_ignore_ascii_case(vendor) && entry.type_ == "gemini"
    });
    if capable {
        Ok(())
    } else {
        Err(GateError::Validation {
            field: t_args(
                "api-gemini-native-vendor-mismatch",
                &[("vendor", vendor.to_string())],
            ),
        }
        .into())
    }
}

fn make_ctx(
    _state: &Arc<ApiState>,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    stream: bool,
) -> GeminiCtx {
    let hints = RoutingHints::from_headers(headers);
    GeminiCtx {
        request_id: uuid::Uuid::new_v4().to_string(),
        start_time: std::time::Instant::now(),
        model: model.to_string(),
        api_key_hash: crate::utils::tenant::api_key_digest(headers),
        session_key: hints.effective_session_key(body),
        preferred_vendor: hints.preferred_vendor.clone(),
        summary: summarize(body, stream),
    }
}

fn summarize(body: &Value, stream: bool) -> String {
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let parts = body
        .get("contents")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|content| {
                    content
                        .get("parts")
                        .and_then(Value::as_array)
                        .map(|parts| parts.len())
                        .unwrap_or(0)
                })
                .sum::<usize>()
        })
        .unwrap_or(0);
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.len())
        .unwrap_or(0);
    json!({"contents": contents, "parts": parts, "tools": tools, "stream": stream}).to_string()
}

fn accounting_body(body: &Value, model: &str) -> Value {
    let mut accounting = body.clone();
    if let Some(object) = accounting.as_object_mut() {
        object.insert("model".into(), Value::String(model.to_string()));
    }
    accounting
}

async fn resolve_route(
    state: &Arc<ApiState>,
    model: &str,
    headers: &HeaderMap,
    hints: RoutingHints,
    session_key: Option<&str>,
    api_key_hash: &str,
    body: &Value,
) -> Result<RoutingDecision, FluxgateError> {
    let linucb_x = if state.bandit.is_some()
        && state
            .config
            .read()
            .routing
            .selection
            .trim()
            .eq_ignore_ascii_case("linucb")
    {
        Some(crate::router::linucb_features_from_body(body))
    } else {
        None
    };
    let linucb_features = linucb_x.as_ref().map(|features| features.as_slice());
    let deadline = headers
        .get("x-deadline-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0);
    let route_ctx = crate::api::route_ctx(state, body, hints.batch);
    let decision = if hints.has_hints() {
        state
            .router
            .route_with_hints(
                model,
                hints.preferred_vendor.as_deref(),
                hints.preferred_model.as_deref(),
                hints.preferred_weight,
                Some(api_key_hash),
                linucb_features,
                deadline,
                route_ctx,
            )
            .await?
    } else {
        state
            .router
            .route_weighted(
                model,
                session_key,
                Some(api_key_hash),
                &[],
                hints.preferred_weight,
                linucb_features,
                deadline,
                route_ctx,
            )
            .await?
    };
    Ok(decision)
}

async fn dispatch_nonstream(
    state: &Arc<ApiState>,
    ctx: &GeminiCtx,
    model: &str,
    decision: RoutingDecision,
    request: crate::protocol::UnifiedRequest,
) -> Result<(crate::protocol::UnifiedResponse, RoutingDecision), FluxgateError> {
    #[cfg(feature = "resilience")]
    {
        let initial = decision.clone();
        let mut trail = crate::vendor::resilience::AttemptTrail::new(
            state.config.read().resilience.expose_fallback_headers,
        );
        return match crate::api::execute_request_with_fallback(
            state, model, decision, request, &mut trail,
        )
        .await
        {
            Ok(pair) => Ok(pair),
            Err(error) => {
                let error = FluxgateError::from(error);
                log_failure(state, ctx, &initial, &error).await;
                Err(error)
            }
        };
    }
    #[cfg(not(feature = "resilience"))]
    {
        match state
            .vendor_manager
            .execute_request(&decision.vendor.id, request)
            .await
        {
            Ok(response) => Ok((response, decision)),
            Err(error) => {
                let error = FluxgateError::from(error);
                log_failure(state, ctx, &decision, &error).await;
                Err(error)
            }
        }
    }
}

fn gemini_error(error: FluxgateError) -> Response {
    let code = crate::api::status_code_for_error(&error);
    let status = gemini_status(code);
    (
        StatusCode::from_u16(code as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({
            "error": {
                "code": code,
                "message": error.user_message(),
                "status": status,
            }
        })),
    )
        .into_response()
}

fn gemini_not_found(model: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "code": 404,
                "message": format!("Gemini model not found: {model}"),
                "status": "NOT_FOUND",
            }
        })),
    )
        .into_response()
}

fn gemini_status(code: i32) -> &'static str {
    match code {
        400 | 422 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        408 | 504 => "DEADLINE_EXCEEDED",
        429 => "RESOURCE_EXHAUSTED",
        500 => "INTERNAL",
        502 | 503 => "UNAVAILABLE",
        _ => "INTERNAL",
    }
}

async fn settle_nonstream(
    state: &Arc<ApiState>,
    ctx: &GeminiCtx,
    body: &Value,
    response: &Value,
    decision: &RoutingDecision,
) {
    let accounting = accounting_body(body, &ctx.model);
    crate::api::record_keyed_usage(
        state,
        &decision.vendor.id,
        &decision.vendor.type_,
        &decision.model.model_id,
        &ctx.api_key_hash,
        response,
    )
    .await;
    crate::middleware::quota::settle_nonstream(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        &accounting,
        response,
        &decision.vendor.type_,
        &decision.vendor.id,
        Some(state.vendor_manager.metrics()),
    );
}

async fn log_failure(
    state: &Arc<ApiState>,
    ctx: &GeminiCtx,
    decision: &RoutingDecision,
    error: &FluxgateError,
) {
    crate::api::clear_session_affinity(state, &ctx.api_key_hash, ctx.session_key.as_deref()).await;
    log_request(
        state,
        ctx,
        decision,
        None,
        crate::api::status_code_for_error(error),
        Some(error.to_string()),
    )
    .await;
}

async fn log_request(
    state: &Arc<ApiState>,
    ctx: &GeminiCtx,
    decision: &RoutingDecision,
    response: Option<&Value>,
    status_code: i32,
    error_message: Option<String>,
) {
    let summary = response
        .and_then(|value| serde_json::to_string(value).ok())
        .map(crate::api::truncate_payload);
    let messages = crate::api::truncate_payload(ctx.summary.clone());
    let (prompt, completion) = response
        .and_then(crate::protocol::parse_usage_detailed)
        .map(|(prompt, completion, _, _)| (Some(prompt as i32), Some(completion as i32)))
        .unwrap_or((None, None));
    let log = RequestLog {
        id: None,
        request_id: ctx.request_id.clone(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1beta/models/{id}".to_string(),
        input_format: "gemini".to_string(),
        abstract_model: Some(ctx.model.clone()),
        messages,
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(status_code),
        response: summary,
        error_message,
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt.zip(completion).map(|(p, c)| p + c),
        duration_ms: Some(ctx.start_time.elapsed().as_millis() as i64),
        cost_usd: None,
    };
    if let Some(logger) = state.logger.as_ref() {
        if let Err(error) = logger.log(log).await {
            tracing::warn!(
                "{}",
                t_args("api-log-push-rejected", &[("error", error.to_string())])
            );
        }
    }
}

fn with_route_headers(mut response: Response, decision: &RoutingDecision) -> Response {
    for (name, value) in [
        ("x-fluxgate-vendor", decision.vendor.id.as_str()),
        ("x-fluxgate-model", decision.model.model_id.as_str()),
    ] {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

fn parts_text_chars(parts: Option<&Value>) -> usize {
    parts
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .map(|text| text.chars().count())
                .sum()
        })
        .unwrap_or(0)
}

fn validation(key: &str) -> GateError {
    GateError::Validation { field: t(key) }
}

async fn embed_content(
    state: Arc<ApiState>,
    headers: HeaderMap,
    model: String,
    body: Value,
) -> Result<Response, FluxgateError> {
    check_body_model(&body, &model)?;
    let text = embed_text(body.get("content"))?;
    let dimensions = body
        .get("outputDimensionality")
        .and_then(Value::as_u64)
        .map(|value| value.min(u32::MAX as u64));
    let mut openai_body = json!({"model": model, "input": text});
    if let Some(dimensions) = dimensions {
        openai_body["dimensions"] = json!(dimensions);
    }
    let accounting = accounting_body(&body, &model);
    let summary = json!({"items": 1, "chars": text.chars().count()}).to_string();
    let response = crate::api::completions::execute_embeddings(
        &state,
        &headers,
        openai_body,
        Some(&accounting),
        crate::api::completions::EmbeddingLogMeta {
            endpoint: "/v1beta/models/{id}",
            input_format: "gemini",
        },
        &summary,
    )
    .await?;
    let values = first_embedding_values(&response)?;
    let mut payload = json!({"embedding": {"values": values}});
    if let Some(usage) = response.get("usage") {
        payload["usageMetadata"] = json!({
            "promptTokenCount": usage.get("prompt_tokens"),
            "totalTokenCount": usage.get("total_tokens"),
        });
    }
    Ok(Json(payload).into_response())
}

async fn batch_embed_contents(
    state: Arc<ApiState>,
    headers: HeaderMap,
    model: String,
    body: Value,
) -> Result<Response, FluxgateError> {
    let requests = body
        .get("requests")
        .and_then(Value::as_array)
        .filter(|requests| !requests.is_empty())
        .ok_or_else(|| validation("api-gemini-embed-requests-invalid"))?;
    let mut texts = Vec::with_capacity(requests.len());
    let mut dimensions: Option<u64> = None;
    let mut chars = 0usize;
    for request in requests {
        if let Some(request_model) = request.get("model").and_then(Value::as_str) {
            if normalize_model(request_model) != model {
                return Err(GateError::Validation {
                    field: t_args(
                        "api-gemini-model-mismatch",
                        &[("body", request_model.to_string()), ("path", model.clone())],
                    ),
                }
                .into());
            }
        }
        let text = embed_text(request.get("content"))?;
        chars += text.chars().count();
        if let Some(current) = request.get("outputDimensionality").and_then(Value::as_u64) {
            if let Some(expected) = dimensions {
                if expected != current {
                    return Err(validation("api-gemini-embed-dimensions-conflict").into());
                }
            }
            dimensions = Some(current);
        }
        texts.push(text);
    }
    let mut openai_body = json!({"model": model, "input": texts});
    if let Some(dimensions) = dimensions {
        openai_body["dimensions"] = json!(dimensions.min(u32::MAX as u64));
    }
    let accounting = accounting_body(&body, &model);
    let summary = json!({"items": texts.len(), "chars": chars}).to_string();
    let response = crate::api::completions::execute_embeddings(
        &state,
        &headers,
        openai_body,
        Some(&accounting),
        crate::api::completions::EmbeddingLogMeta {
            endpoint: "/v1beta/models/{id}",
            input_format: "gemini",
        },
        &summary,
    )
    .await?;
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| GateError::Protocol {
            source: t_args("api-gemini-response-invalid", &[]),
            inner: None,
        })?;
    let mut embeddings = Vec::with_capacity(data.len());
    for item in data {
        embeddings.push(json!({
            "values": embedding_values(item.get("embedding"))?
        }));
    }
    Ok(Json(json!({"embeddings": embeddings})).into_response())
}

fn check_body_model(body: &Value, model: &str) -> Result<(), FluxgateError> {
    if let Some(body_model) = body.get("model").and_then(Value::as_str) {
        if normalize_model(body_model) != model {
            return Err(GateError::Validation {
                field: t_args(
                    "api-gemini-model-mismatch",
                    &[
                        ("body", body_model.to_string()),
                        ("path", model.to_string()),
                    ],
                ),
            }
            .into());
        }
    }
    Ok(())
}

/// `content.parts[].text` 拼接；非文本 part / 空文本 → 400。
fn embed_text(content: Option<&Value>) -> Result<String, FluxgateError> {
    let parts = content
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| validation("api-gemini-embed-content-invalid"))?;
    let mut text = String::new();
    for part in parts {
        let Some(chunk) = part.get("text").and_then(Value::as_str) else {
            return Err(validation("api-gemini-embed-content-invalid").into());
        };
        text.push_str(chunk);
    }
    if text.is_empty() {
        return Err(validation("api-gemini-embed-content-invalid").into());
    }
    Ok(text)
}

fn first_embedding_values(response: &Value) -> Result<Value, GateError> {
    let item = response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .ok_or_else(|| GateError::Protocol {
            source: t_args("api-gemini-response-invalid", &[]),
            inner: None,
        })?;
    embedding_values(item.get("embedding"))
}

fn embedding_values(value: Option<&Value>) -> Result<Value, GateError> {
    match value {
        Some(Value::Array(values)) => Ok(Value::Array(values.clone())),
        Some(Value::String(encoded)) => {
            let values = decode_base64_f32(encoded).ok_or_else(|| GateError::Protocol {
                source: t_args("api-gemini-response-invalid", &[]),
                inner: None,
            })?;
            Ok(json!(values))
        }
        _ => Err(GateError::Protocol {
            source: t_args("api-gemini-response-invalid", &[]),
            inner: None,
        }),
    }
}

/// OpenAI `encoding_format=base64` 的 Float32 LE 解码（无外部依赖实现）。
fn decode_base64_f32(encoded: &str) -> Option<Vec<f32>> {
    let bytes = decode_base64(encoded)?;
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in encoded.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_only_detection() {
        assert!(gemini_native_only(&json!({"safetySettings": []})));
        assert!(gemini_native_only(&json!({"safety_settings": []})));
        assert!(gemini_native_only(
            &json!({"cachedContent": "cachedContents/x"})
        ));
        assert!(gemini_native_only(
            &json!({"cached_content": "cachedContents/x"})
        ));
        assert!(gemini_native_only(&json!({
            "tools": [{"googleSearch": {}}]
        })));
        assert!(!gemini_native_only(&json!({
            "tools": [{"functionDeclarations": [{"name": "f"}]}]
        })));
        assert!(!gemini_native_only(&json!({})));
    }

    #[test]
    fn error_body_is_gemini_shaped() {
        let response = gemini_error(
            GateError::Validation {
                field: "bad".into(),
            }
            .into(),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn status_mapping() {
        assert_eq!(gemini_status(400), "INVALID_ARGUMENT");
        assert_eq!(gemini_status(401), "UNAUTHENTICATED");
        assert_eq!(gemini_status(404), "NOT_FOUND");
        assert_eq!(gemini_status(429), "RESOURCE_EXHAUSTED");
        assert_eq!(gemini_status(502), "UNAVAILABLE");
        assert_eq!(gemini_status(504), "DEADLINE_EXCEEDED");
    }

    #[test]
    fn accounting_body_injects_model_only() {
        let body = json!({"contents": []});
        let accounting = accounting_body(&body, "gemini-2.5-pro");
        assert_eq!(accounting["model"], "gemini-2.5-pro");
        assert!(body.get("model").is_none(), "原 body 不被改写");
    }

    #[test]
    fn summary_is_bounded() {
        let body = json!({
            "contents": [
                {"parts": [{"text": "a"}, {"text": "b"}]},
                {"parts": [{"text": "c"}]}
            ],
            "tools": [{"functionDeclarations": []}]
        });
        let summary = summarize(&body, true);
        assert!(summary.contains("\"contents\":2"));
        assert!(summary.contains("\"parts\":3"));
        assert!(summary.contains("\"stream\":true"));
        assert!(!summary.contains("\"a\""), "摘要不得包含原文");
    }

    #[test]
    fn base64_embedding_decodes_float32_le() {
        let values = decode_base64_f32("AACAPwAAAMA=").expect("decode");
        assert_eq!(values, vec![1.0f32, -2.0f32]);
        assert!(decode_base64_f32("bad").is_none());
        // 非 4 字节倍数 → None
        assert!(decode_base64_f32("AAA=").is_none());
    }

    #[test]
    fn embed_text_requires_text_parts() {
        let ok = embed_text(Some(&json!({"parts": [{"text": "ab"}, {"text": "cd"}]}))).unwrap();
        assert_eq!(ok, "abcd");
        assert!(embed_text(Some(&json!({"parts": [{"inlineData": {}}]}))).is_err());
        assert!(embed_text(Some(&json!({"parts": [{"text": ""}]}))).is_err());
        assert!(embed_text(None).is_err());
    }

    #[test]
    fn embedding_values_handles_array_and_base64() {
        assert_eq!(
            embedding_values(Some(&json!([0.1, 0.2]))).unwrap(),
            json!([0.1, 0.2])
        );
        let decoded = embedding_values(Some(&json!("AACAPwAAAMA="))).unwrap();
        assert_eq!(decoded[0], 1.0);
        assert!(embedding_values(None).is_err());
    }
}
