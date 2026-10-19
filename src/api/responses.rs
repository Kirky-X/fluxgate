// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAI Responses API 入站端点（`/v1/responses`）。
//!
//! 本模块承载：请求解析与状态回放、IR/原生双路径派发、语义化 SSE 编码、
//! 有状态存储与后台任务。协议转换纯函数在 [`crate::protocol::responses`]。

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::{FluxgateError, GateError};
use crate::protocol::responses::{
    now_secs, parse_request, queued_response, to_responses_response, to_unified_request,
    ParsedResponsesRequest, ResponsesResponse, ResponsesStreamEncoder,
};
use crate::protocol::{StreamChunk, UnifiedRequest, UnifiedResponse};
use crate::router::RoutingDecision;
use crate::storage::ResponseStoreSink;
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// 流式响应公共头。
const STREAM_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
];

/// Responses 运行时：有状态存储（`response_store`）与后台任务注册表。
/// `ApiState.responses = None` 时端点仍可用，但有状态能力降级为 no-op。
#[derive(Clone)]
pub struct ResponsesRuntime {
    pub store: Arc<dyn ResponseStoreSink>,
    pub jobs: Arc<ResponsesBackgroundJobs>,
}

impl ResponsesRuntime {
    pub fn new(store: Arc<dyn ResponseStoreSink>) -> Self {
        Self {
            store,
            jobs: Arc::new(ResponsesBackgroundJobs::default()),
        }
    }
}

/// 后台任务注册表：abort 句柄 + 活跃计数（`max_background_jobs` 热读，
/// 用 CAS 计数而非固定容量信号量）。
#[derive(Default)]
pub struct ResponsesBackgroundJobs {
    handles: std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
    active: std::sync::atomic::AtomicUsize,
}

impl ResponsesBackgroundJobs {
    /// 尝试占用一个后台槽位；超限返回 false（调用方映射 429）。
    pub fn try_begin(&self, max_jobs: usize) -> bool {
        use std::sync::atomic::Ordering;
        let max_jobs = max_jobs.max(1);
        let mut current = self.active.load(Ordering::Relaxed);
        loop {
            if current >= max_jobs {
                return false;
            }
            match self.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// 释放槽位（任务结束/启动失败）。
    pub fn end(&self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }

    pub fn register(&self, id: &str, handle: tokio::task::AbortHandle) {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), handle);
    }

    pub fn remove(&self, id: &str) {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    /// 取消并 abort；未登记则 no-op。
    pub fn cancel(&self, id: &str) {
        if let Some(handle) = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id)
        {
            handle.abort();
        }
    }

    pub fn active_count(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl std::fmt::Debug for ResponsesRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponsesRuntime").finish_non_exhaustive()
    }
}

/// 请求级横切束（日志/存储/配额共用）。
struct ResponsesCtx {
    request_id: String,
    start_time: std::time::Instant,
    session_key: Option<String>,
    api_key_hash: String,
    /// 生效 store（`[responses].store_enabled=false` 时已降级）
    store: bool,
}

#[axum::debug_handler]
pub async fn create_response(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, FluxgateError> {
    let responses_config = state.config.read().responses.clone();
    if !responses_config.enabled {
        return Ok(not_found(crate::i18n::t("api-responses-disabled")));
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();
    let parsed = parse_request(&body)?;
    let api_key_hash = crate::utils::tenant::api_key_digest(&headers);
    let hints = RoutingHints::from_headers(&headers);
    let session_key = hints.effective_session_key(&body);

    let effective_store = parsed.store() && responses_config.store_enabled;
    let store_wired = effective_store && state.responses.is_some();
    let ctx = ResponsesCtx {
        request_id,
        start_time,
        session_key,
        api_key_hash,
        store: store_wired,
    };
    let mut merged_items = match resolve_history(&state, &parsed, &ctx.api_key_hash).await {
        Ok(items) => items,
        Err(HistoryError::NotFound(message)) => return Ok(not_found(message)),
        Err(HistoryError::Invalid(field)) => {
            return Err(FluxgateError::from(GateError::Validation { field }))
        }
    };
    if parsed.request.truncation.as_deref() == Some("auto") {
        truncate_for_context(&state, &parsed.request.model, &mut merged_items);
    }
    // 原生能力（内置工具/文件/音频/prompt）：只允许 api_style="responses"
    // 候选，原样打 /responses
    if parsed.native_only.is_some() {
        if parsed.request.background {
            return Err(FluxgateError::from(GateError::Validation {
                field: "background: native-only requests are not supported in background mode"
                    .to_string(),
            }));
        }
        return native_response(
            &state,
            &ctx,
            &parsed,
            &merged_items,
            &body,
            hints,
            &responses_config,
            effective_store,
        )
        .await;
    }
    if parsed.request.background {
        return background_response(
            &state,
            &ctx,
            &parsed,
            &merged_items,
            &body,
            hints,
            &responses_config,
            effective_store,
        )
        .await;
    }

    let unified_request = to_unified_request(&parsed, &merged_items)?;
    let decision = resolve_route(
        &state,
        &parsed.request.model,
        &headers,
        hints,
        ctx.session_key.as_deref(),
        &ctx.api_key_hash,
        &body,
    )
    .await?;

    if parsed.request.stream {
        return stream_response(
            &state,
            &ctx,
            &parsed,
            &body,
            decision,
            unified_request,
            &responses_config,
            effective_store,
        )
        .await;
    }

    // 非流式：派发 → 转回 → 存储 → 日志 → 结算
    let log_decision = decision.clone();
    let (unified_response, decision) =
        match dispatch_request(&state, &parsed.request.model, decision, unified_request).await {
            Ok(pair) => pair,
            Err(error) => {
                state
                    .vendor_manager
                    .metrics()
                    .record_responses_upstream_failure();
                log_responses_failure(&state, &ctx, &log_decision, &parsed, &error).await;
                return Err(error);
            }
        };
    let response_id = format!("resp_{}", uuid::Uuid::new_v4());
    let responses_response = to_responses_response(
        &response_id,
        &unified_response,
        &parsed.request,
        effective_store,
    );
    let response_value = serde_json::to_value(&responses_response).unwrap_or(Value::Null);

    persist_response(
        &state,
        &ctx,
        &parsed,
        &responses_response,
        &responses_config,
    )
    .await;

    log_responses_request(
        &state,
        &ctx,
        &decision,
        &parsed,
        Some(&response_value),
        200,
        None,
    )
    .await;
    settle_nonstream(&state, &ctx, &body, &response_value, &decision).await;
    crate::api::bind_session_affinity(
        &state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    let response = (Json(response_value)).into_response();
    state.vendor_manager.metrics().record_responses_request();
    Ok(with_route_headers(response, &decision))
}

/// 流式路径：IR chunk 流 → 语义化 SSE；终止快照异步落库。
#[allow(clippy::too_many_arguments)]
async fn stream_response(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    parsed: &ParsedResponsesRequest,
    body: &Value,
    decision: RoutingDecision,
    unified_request: UnifiedRequest,
    responses_config: &crate::config::ResponsesSection,
    effective_store: bool,
) -> Result<Response, FluxgateError> {
    let chunk_stream = state
        .vendor_manager
        .execute_stream_request(&decision.vendor.id, unified_request)
        .await;
    let chunk_stream = match chunk_stream {
        Ok(stream) => stream,
        Err(error) => {
            state
                .vendor_manager
                .metrics()
                .record_responses_upstream_failure();
            let error = FluxgateError::from(error);
            log_responses_failure(state, ctx, &decision, parsed, &error).await;
            return Err(error);
        }
    };
    state.vendor_manager.metrics().record_responses_request();

    let settlement = crate::middleware::quota::SettlementCtx::for_request(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &decision.vendor.type_,
        &decision.vendor.id,
        Some(Arc::clone(state.vendor_manager.metrics())),
    );
    let chunk_stream = crate::middleware::quota::settle_stream(chunk_stream, settlement);
    // 转换流用量记账：IR `StreamChunk::Usage` 已结构化——占位日志行终止补写
    // 与 keyed 分账（分账 model 维度取上游 model_id，与非流式同口径）。
    let chunk_stream = crate::api::stream_usage::record_converted_stream(
        state.clone(),
        ctx.request_id.clone(),
        crate::api::stream_usage::StreamProtocol::Responses,
        decision.vendor.id.clone(),
        decision.vendor.type_.clone(),
        decision.model.model_id.clone(),
        ctx.api_key_hash.clone(),
        chunk_stream,
    );

    let mut template = queued_response(&format!("resp_{}", uuid::Uuid::new_v4()), &parsed.request);
    template.status = "in_progress".to_string();
    template.store = effective_store;
    let encoder = ResponsesStreamEncoder::new(template);

    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    log_responses_request(state, ctx, &decision, parsed, None, 200, None).await;

    let state_for_store = Arc::clone(state);
    let parsed_for_store = parsed.clone();
    let responses_for_store = responses_config.clone();
    let tenant = ctx.api_key_hash.clone();
    let store_enabled = ctx.store;
    let body_stream = responses_sse_stream(
        encoder,
        chunk_stream,
        Arc::clone(state.vendor_manager.metrics()),
        move |final_response| {
            if !store_enabled {
                return;
            }
            let state = Arc::clone(&state_for_store);
            let parsed = parsed_for_store.clone();
            let config = responses_for_store.clone();
            let tenant = tenant.clone();
            let response = final_response;
            tokio::spawn(async move {
                persist_stream_response(&state, &tenant, &parsed, &response, &config).await;
            });
        },
    );

    Ok((STREAM_HEADERS, body_stream).into_response())
}

/// IR chunk 流 → Responses 语义事件 SSE；错误以 `response.failed` 终止。
fn responses_sse_stream<S, F>(
    mut encoder: ResponsesStreamEncoder,
    chunk_stream: S,
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
    on_final: F,
) -> axum::body::Body
where
    S: futures::Stream<Item = Result<StreamChunk, GateError>> + Send + 'static,
    F: FnOnce(ResponsesResponse) + Send + 'static,
{
    let sse_stream = async_stream::stream! {
        use futures::StreamExt as _;
        for frame in encoder.start() {
            yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(frame));
        }
        let mut chunk_stream = Box::pin(chunk_stream);
        let mut failed = false;
        while let Some(item) = chunk_stream.next().await {
            match item {
                Ok(chunk) => {
                    for frame in encoder.on_chunk(&chunk) {
                        yield Ok(bytes::Bytes::from(frame));
                    }
                }
                Err(error) => {
                    failed = true;
                    metrics.record_responses_upstream_failure();
                    let safe_text = crate::i18n::t_args(
                        "api-responses-stream-failed",
                        &[("error", error.to_string())],
                    );
                    for frame in encoder.fail(&safe_text) {
                        yield Ok(bytes::Bytes::from(frame));
                    }
                    break;
                }
            }
        }
        if !failed {
            let frames = encoder.finish();
            if let Some(final_response) = encoder.final_response().cloned() {
                on_final(final_response);
            }
            for frame in frames {
                yield Ok(bytes::Bytes::from(frame));
            }
        }
    };
    axum::body::Body::from_stream(sse_stream)
}

/// 原生直通路径：能力校验 → 路由（native 过滤）→ 请求体改写 → `/responses`。
#[allow(clippy::too_many_arguments)]
async fn native_response(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    parsed: &ParsedResponsesRequest,
    merged_items: &[Value],
    body: &Value,
    hints: RoutingHints,
    responses_config: &crate::config::ResponsesSection,
    effective_store: bool,
) -> Result<Response, FluxgateError> {
    validate_native_hints(state, &hints)?;
    let decision = if hints.has_hints() {
        let route_ctx = crate::api::route_ctx(state, body, hints.batch);
        state
            .router
            .route_with_hints(
                &parsed.request.model,
                hints.preferred_vendor.as_deref(),
                hints.preferred_model.as_deref(),
                hints.preferred_weight,
                Some(&ctx.api_key_hash),
                None,
                None,
                route_ctx,
            )
            .await?
    } else {
        state
            .router
            .route_responses_native(&parsed.request.model, ctx.session_key.as_deref())
            .await?
    };
    if !decision.vendor.is_responses_style() {
        return Err(FluxgateError::from(GateError::Protocol {
            source: crate::i18n::t_args(
                "api-responses-native-hint-vendor",
                &[("vendor", decision.vendor.id.clone())],
            ),
            inner: None,
        }));
    }

    let native_body = rewrite_native_body(body, &parsed.request.model, merged_items);
    if parsed.request.stream {
        let byte_stream = state
            .vendor_manager
            .execute_passthrough_stream_request_at(&decision.vendor.id, native_body, "responses")
            .await;
        let byte_stream = match byte_stream {
            Ok(stream) => stream,
            Err(error) => {
                state
                    .vendor_manager
                    .metrics()
                    .record_responses_upstream_failure();
                let error = FluxgateError::from(error);
                log_responses_failure(state, ctx, &decision, parsed, &error).await;
                return Err(error);
            }
        };
        state.vendor_manager.metrics().record_responses_request();
        let state_for_store = Arc::clone(state);
        let parsed_for_store = parsed.clone();
        let config_for_store = responses_config.clone();
        let tenant = ctx.api_key_hash.clone();
        let observed = observe_native_completed(byte_stream, move |response| {
            if !effective_store {
                return;
            }
            let state = Arc::clone(&state_for_store);
            let parsed = parsed_for_store.clone();
            let config = config_for_store.clone();
            let tenant = tenant.clone();
            tokio::spawn(async move {
                persist_stream_response(&state, &tenant, &parsed, &response, &config).await;
            });
        });
        // 观测回调同时服务流式用量记账（占位日志行终止补写 + keyed 分账）。
        let recorder = crate::api::stream_usage::StreamUsageRecorder::new(
            state.clone(),
            ctx.request_id.clone(),
            crate::api::stream_usage::StreamProtocol::Responses,
            decision.vendor.id.clone(),
            decision.vendor.type_.clone(),
            decision.model.model_id.clone(),
            ctx.api_key_hash.clone(),
        );
        let observer = recorder.clone();
        let body_stream = crate::middleware::quota::passthrough_stream_body(
            &state.config,
            state.quota.as_ref(),
            &ctx.api_key_hash,
            body,
            &decision.vendor.type_,
            observed,
            move |observation| match observation {
                crate::api::sse::ReframeObservation::Frame {
                    data: Some(data), ..
                } => observer.observe_frame(data),
                crate::api::sse::ReframeObservation::End { .. } => observer.finish(),
                _ => {}
            },
        );
        crate::api::bind_session_affinity(
            state,
            &ctx.api_key_hash,
            ctx.session_key.as_deref(),
            &decision.vendor.id,
        )
        .await;
        log_responses_request(state, ctx, &decision, parsed, None, 200, None).await;
        return Ok((STREAM_HEADERS, body_stream).into_response());
    }

    let upstream = state
        .vendor_manager
        .execute_passthrough_request_at(&decision.vendor.id, native_body, "responses")
        .await;
    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(error) => {
            state
                .vendor_manager
                .metrics()
                .record_responses_upstream_failure();
            let error = FluxgateError::from(error);
            log_responses_failure(state, ctx, &decision, parsed, &error).await;
            return Err(error);
        }
    };
    let mut response: ResponsesResponse = match serde_json::from_value(upstream.clone()) {
        Ok(response) => response,
        Err(error) => {
            // 上游已 2xx（已计费）：形状非法也入分账与失败日志
            settle_nonstream(state, ctx, body, &upstream, &decision).await;
            state
                .vendor_manager
                .metrics()
                .record_responses_upstream_failure();
            let error = FluxgateError::from(GateError::Protocol {
                source: format!("responses upstream response: {error}"),
                inner: None,
            });
            log_responses_failure(state, ctx, &decision, parsed, &error).await;
            return Err(error);
        }
    };
    response.store = effective_store;
    state.vendor_manager.metrics().record_responses_request();
    let response_value = serde_json::to_value(&response).unwrap_or(Value::Null);

    persist_response(state, ctx, parsed, &response, responses_config).await;
    log_responses_request(
        state,
        ctx,
        &decision,
        parsed,
        Some(&response_value),
        200,
        None,
    )
    .await;
    settle_nonstream(state, ctx, body, &response_value, &decision).await;
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    Ok(with_route_headers(
        Json(response_value).into_response(),
        &decision,
    ))
}

/// X-Vendor/X-Model 必须指向 Responses 原生厂商；不匹配 → 502 且不发请求。
fn validate_native_hints(state: &Arc<ApiState>, hints: &RoutingHints) -> Result<(), FluxgateError> {
    if let Some(vendor_id) = hints.preferred_vendor.as_deref() {
        let capable = state
            .config
            .get_vendor_ci(vendor_id)
            .is_some_and(|vendor| vendor.enabled && vendor.is_responses_style());
        if !capable {
            return Err(FluxgateError::from(GateError::Protocol {
                source: crate::i18n::t_args(
                    "api-responses-native-hint-vendor",
                    &[("vendor", vendor_id.to_string())],
                ),
                inner: None,
            }));
        }
    }
    if let Some(model) = hints.preferred_model.as_deref() {
        let capable = state
            .config
            .resolve_model(model)
            .iter()
            .any(|(vendor, _)| vendor.is_responses_style());
        if !capable {
            return Err(FluxgateError::from(GateError::Protocol {
                source: crate::i18n::t_args(
                    "api-responses-native-hint-model",
                    &[("model", model.to_string())],
                ),
                inner: None,
            }));
        }
    }
    Ok(())
}

/// 原生请求体改写：历史回放后的 items、剥离状态字段、上游 store=false。
fn rewrite_native_body(body: &Value, model: &str, items: &[Value]) -> Value {
    let mut native = body.clone();
    if let Some(object) = native.as_object_mut() {
        object.insert("model".to_string(), json!(model));
        object.insert("input".to_string(), Value::Array(items.to_vec()));
        object.remove("previous_response_id");
        object.remove("conversation");
        object.insert("store".to_string(), json!(false));
    }
    native
}

/// 原生流观测：`response.completed`/`response.incomplete` 时回调完整响应；
/// 字节原样透传（随后由配额/分帧层再解析，双解析仅原生流路径）。
fn observe_native_completed<S, F>(
    stream: S,
    mut on_response: F,
) -> impl futures::Stream<Item = Result<bytes::Bytes, GateError>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, GateError>>,
    F: FnMut(ResponsesResponse) + Send + 'static,
{
    use futures::StreamExt as _;
    let mut parser = crate::protocol::SseFrameParser::new();
    stream.map(move |item| {
        if let Ok(bytes) = &item {
            parser.feed(bytes);
            while let Some(frame) = parser.next_frame() {
                let Some(data) = frame.data.as_deref() else {
                    continue;
                };
                let Some(event) = crate::protocol::responses::StreamEvent::from_data(data) else {
                    continue;
                };
                if matches!(
                    event.event_type.as_str(),
                    "response.completed" | "response.incomplete"
                ) {
                    if let Some(response) = event.response {
                        on_response(response);
                    }
                }
            }
        }
        item
    })
}

/// 后台任务路径：入队即返回 `queued`，任务完成后回写状态/结果。
#[allow(clippy::too_many_arguments)]
async fn background_response(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    parsed: &ParsedResponsesRequest,
    merged_items: &[Value],
    body: &Value,
    hints: RoutingHints,
    responses_config: &crate::config::ResponsesSection,
    effective_store: bool,
) -> Result<Response, FluxgateError> {
    if !responses_config.background_enabled {
        return Err(FluxgateError::from(GateError::Validation {
            field: "background: disabled by [responses].background_enabled".to_string(),
        }));
    }
    if !effective_store {
        return Err(FluxgateError::from(GateError::Validation {
            field: "background requires store=true".to_string(),
        }));
    }
    let Some(runtime) = state.responses.as_ref() else {
        return Err(FluxgateError::from(GateError::Validation {
            field: "background: state store is not wired".to_string(),
        }));
    };
    if !runtime.jobs.try_begin(responses_config.max_background_jobs) {
        return Err(FluxgateError::from(GateError::RateLimited {
            vendor: "responses-background".to_string(),
        }));
    }

    let response_id = format!("resp_{}", uuid::Uuid::new_v4());
    let mut queued = queued_response(&response_id, &parsed.request);
    queued.store = true;
    let response_json = serde_json::to_string(&queued).unwrap_or_default();
    let now = now_secs() as i64;
    let row = crate::storage::StoredResponse {
        id: response_id.clone(),
        tenant_hash: ctx.api_key_hash.clone(),
        conversation_id: parsed.request.conversation.clone(),
        parent_response_id: parsed.request.previous_response_id.clone(),
        model: parsed.request.model.clone(),
        status: "queued".to_string(),
        store: true,
        background: true,
        created_at: now,
        expires_at: now + (responses_config.retention_hours as i64) * 3600,
        input_items: serde_json::to_string(&parsed.current_items).unwrap_or_else(|_| "[]".into()),
        output_items: "[]".to_string(),
        response_json,
    };
    if let Err(error) = runtime.store.insert_response(&row).await {
        runtime.jobs.end();
        state
            .vendor_manager
            .metrics()
            .record_responses_store_failure();
        return Err(FluxgateError::from(GateError::Config {
            message: error.to_string(),
        }));
    }
    state
        .vendor_manager
        .metrics()
        .record_responses_background_job();

    let state_for_job = Arc::clone(state);
    let jobs = Arc::clone(&runtime.jobs);
    let parsed_for_job = parsed.clone();
    let merged_for_job = merged_items.to_vec();
    let body_for_job = body.clone();
    let tenant = ctx.api_key_hash.clone();
    let id_for_job = response_id.clone();
    let handle = tokio::spawn(async move {
        let outcome = run_background_job(
            &state_for_job,
            &parsed_for_job,
            &merged_for_job,
            &body_for_job,
            &tenant,
            &id_for_job,
            hints,
        )
        .await;
        let (status, value) = match outcome {
            Ok(response) => (
                response.status.clone(),
                serde_json::to_string(&response).unwrap_or_else(|_| "{}".into()),
            ),
            Err(error) => {
                let mut failed = queued_response(&id_for_job, &parsed_for_job.request);
                failed.status = "failed".to_string();
                failed.error = Some(crate::protocol::responses::ResponseError {
                    code: Some("background_failed".to_string()),
                    message: error.to_string(),
                });
                (
                    "failed".to_string(),
                    serde_json::to_string(&failed).unwrap_or_else(|_| "{}".into()),
                )
            }
        };
        if let Some(runtime) = state_for_job.responses.as_ref() {
            let _ = runtime
                .store
                .set_response_status(&id_for_job, &tenant, &status, &value)
                .await;
        }
        jobs.end();
        jobs.remove(&id_for_job);
    });
    runtime.jobs.register(&response_id, handle.abort_handle());

    Ok(Json(serde_json::to_value(&queued).unwrap_or(Value::Null)).into_response())
}

/// 后台任务执行体：IR 路由 → 上游 → Responses 响应；成功/失败均由调用方回写。
async fn run_background_job(
    state: &Arc<ApiState>,
    parsed: &ParsedResponsesRequest,
    merged_items: &[Value],
    body: &Value,
    tenant: &str,
    response_id: &str,
    hints: RoutingHints,
) -> Result<ResponsesResponse, FluxgateError> {
    let unified_request = to_unified_request(parsed, merged_items)?;
    let route_ctx = crate::api::route_ctx(state, body, hints.batch);
    let decision = if hints.has_hints() {
        state
            .router
            .route_with_hints(
                &parsed.request.model,
                hints.preferred_vendor.as_deref(),
                hints.preferred_model.as_deref(),
                hints.preferred_weight,
                Some(tenant),
                None,
                None,
                route_ctx,
            )
            .await?
    } else {
        state
            .router
            .route_weighted(
                &parsed.request.model,
                None,
                Some(tenant),
                &[],
                None,
                None,
                None,
                route_ctx,
            )
            .await?
    };
    let start = std::time::Instant::now();
    let ctx = ResponsesCtx {
        request_id: uuid::Uuid::new_v4().to_string(),
        start_time: start,
        session_key: None,
        api_key_hash: tenant.to_string(),
        store: true,
    };
    let unified_response = match state
        .vendor_manager
        .execute_request(&decision.vendor.id, unified_request)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            state
                .vendor_manager
                .metrics()
                .record_responses_upstream_failure();
            let error = FluxgateError::from(error);
            log_responses_failure(state, &ctx, &decision, parsed, &error).await;
            return Err(error);
        }
    };
    let response = to_responses_response(response_id, &unified_response, &parsed.request, true);
    let value = serde_json::to_value(&response).unwrap_or(Value::Null);
    log_responses_request(state, &ctx, &decision, parsed, Some(&value), 200, None).await;
    settle_nonstream(state, &ctx, body, &value, &decision).await;
    crate::api::bind_session_affinity(state, tenant, None, &decision.vendor.id).await;
    state.vendor_manager.metrics().record_responses_request();
    Ok(response)
}

/// 非流式派发（resilience 开启时复用换路 fallback）。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn dispatch_request(
    state: &Arc<ApiState>,
    model: &str,
    decision: RoutingDecision,
    request: UnifiedRequest,
) -> Result<(UnifiedResponse, RoutingDecision), FluxgateError> {
    #[cfg(feature = "resilience")]
    {
        let mut trail = crate::vendor::resilience::AttemptTrail::new(
            state.config.read().resilience.expose_fallback_headers,
        );
        if state.config.read().resilience.cover_all_paths {
            return crate::api::execute_request_with_fallback(
                state,
                model,
                decision,
                request,
                crate::vendor::manager::VendorCallOptions::default(),
                &mut trail,
            )
            .await
            .map_err(FluxgateError::from);
        }
        let response = state
            .vendor_manager
            .execute_request(&decision.vendor.id, request)
            .await
            .map_err(FluxgateError::from)?;
        Ok((response, decision))
    }
    #[cfg(not(feature = "resilience"))]
    {
        let response = state
            .vendor_manager
            .execute_request(&decision.vendor.id, request)
            .await
            .map_err(FluxgateError::from)?;
        Ok((response, decision))
    }
}

/// 路由解析（X-Vendor/X-Model hints 直连 vs 亲和 weighted）。
async fn resolve_route(
    state: &Arc<ApiState>,
    model: &str,
    _headers: &HeaderMap,
    hints: RoutingHints,
    session_key: Option<&str>,
    api_key_hash: &str,
    body: &Value,
) -> Result<RoutingDecision, FluxgateError> {
    let ctx = crate::api::route_ctx(state, body, hints.batch);
    let decision = if hints.has_hints() {
        state
            .router
            .route_with_hints(
                model,
                hints.preferred_vendor.as_deref(),
                hints.preferred_model.as_deref(),
                hints.preferred_weight,
                Some(api_key_hash),
                None,
                None,
                ctx,
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
                None,
                None,
                ctx,
            )
            .await?
    };
    Ok(decision)
}

/// `POST /v1/responses/input_tokens`：近似输入 token 估算（字符/4 + 结构开销）。
#[axum::debug_handler]
pub async fn input_tokens(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, FluxgateError> {
    if body.get("model").and_then(Value::as_str).is_none() {
        return Err(FluxgateError::from(GateError::Validation {
            field: "model".to_string(),
        }));
    }
    let input = body.get("input").ok_or_else(|| GateError::Validation {
        field: "input".to_string(),
    })?;
    let empty = match input {
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Null => true,
        _ => false,
    };
    if empty {
        return Err(FluxgateError::from(GateError::Validation {
            field: "input: must not be empty".to_string(),
        }));
    }
    let chars_per_token = state.config.read().quota.est_chars_per_token.max(1);
    let instructions = body
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input_text = serde_json::to_string(input).unwrap_or_default();
    let tools_text = body
        .get("tools")
        .map(|tools| tools.to_string())
        .unwrap_or_default();
    let input_tokens = crate::utils::estimate_text_tokens_with(instructions, chars_per_token)
        + crate::utils::estimate_text_tokens_with(&input_text, chars_per_token)
        + crate::utils::estimate_text_tokens_with(&tools_text, chars_per_token);
    Ok(Json(json!({
        "object": "response.input_tokens",
        "input_tokens": input_tokens.max(1),
    })))
}

/// `GET /v1/responses/{id}`：租户隔离检索已存响应。
#[axum::debug_handler]
pub async fn get_response(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, FluxgateError> {
    if !state.config.read().responses.enabled {
        return Ok(not_found(crate::i18n::t("api-responses-disabled")));
    }
    let tenant = crate::utils::tenant::api_key_digest(&headers);
    let Some(runtime) = state.responses.as_ref() else {
        return Ok(not_found(crate::i18n::t("api-responses-store-unavailable")));
    };
    let now = now_secs() as i64;
    let row = runtime
        .store
        .get_response(&id, &tenant, now)
        .await
        .map_err(|error| GateError::Config {
            message: error.to_string(),
        })?;
    match row {
        Some(row) => {
            let value: Value = serde_json::from_str(&row.response_json).unwrap_or(Value::Null);
            Ok(Json(value).into_response())
        }
        None => Ok(not_found(crate::i18n::t_args(
            "api-responses-not-found",
            &[("id", id)],
        ))),
    }
}

/// `DELETE /v1/responses/{id}`：删除已存响应（并中止运行中的后台任务）。
#[axum::debug_handler]
pub async fn delete_response(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, FluxgateError> {
    if !state.config.read().responses.enabled {
        return Ok(not_found(crate::i18n::t("api-responses-disabled")));
    }
    let tenant = crate::utils::tenant::api_key_digest(&headers);
    let Some(runtime) = state.responses.as_ref() else {
        return Ok(not_found(crate::i18n::t("api-responses-store-unavailable")));
    };
    let now = now_secs() as i64;
    // 先 abort 运行中的任务（未登记 = no-op），再删行
    runtime.jobs.cancel(&id);
    let deleted = runtime
        .store
        .delete_response(&id, &tenant, now)
        .await
        .map_err(|error| GateError::Config {
            message: error.to_string(),
        })?;
    if !deleted {
        return Ok(not_found(crate::i18n::t_args(
            "api-responses-not-found",
            &[("id", id)],
        )));
    }
    Ok(Json(json!({
        "id": id,
        "object": "response.deleted",
        "deleted": true,
    }))
    .into_response())
}

/// `POST /v1/responses/{id}/cancel`：仅后台任务可取消；返回 cancelled 响应对象。
#[axum::debug_handler]
pub async fn cancel_response(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, FluxgateError> {
    if !state.config.read().responses.enabled {
        return Ok(not_found(crate::i18n::t("api-responses-disabled")));
    }
    let tenant = crate::utils::tenant::api_key_digest(&headers);
    let Some(runtime) = state.responses.as_ref() else {
        return Ok(not_found(crate::i18n::t("api-responses-store-unavailable")));
    };
    let now = now_secs() as i64;
    let row = runtime
        .store
        .get_response(&id, &tenant, now)
        .await
        .map_err(|error| GateError::Config {
            message: error.to_string(),
        })?;
    let Some(row) = row else {
        return Ok(not_found(crate::i18n::t_args(
            "api-responses-not-found",
            &[("id", id)],
        )));
    };
    if !row.background {
        return Err(FluxgateError::from(GateError::Validation {
            field: "id: only background responses can be cancelled".to_string(),
        }));
    }
    // abort 任务后回写 cancelled（abort 后任务体不再回写，避免竞态覆盖）
    runtime.jobs.cancel(&id);
    let mut value: Value = serde_json::from_str(&row.response_json).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("status".to_string(), json!("cancelled"));
    }
    runtime
        .store
        .set_response_status(&id, &tenant, "cancelled", &value.to_string())
        .await
        .map_err(|error| GateError::Config {
            message: error.to_string(),
        })?;
    Ok(Json(value).into_response())
}

/// 历史解析失败分类：404（不存在/越权/过期）与 400（歧义/超限）。
#[derive(Debug)]
enum HistoryError {
    NotFound(String),
    Invalid(String),
}

/// 解析 `previous_response_id` / `conversation` / `item_reference`，
/// 返回「历史 items + 当前回合 items」的合并序列。
async fn resolve_history(
    state: &Arc<ApiState>,
    parsed: &ParsedResponsesRequest,
    tenant_hash: &str,
) -> Result<Vec<Value>, HistoryError> {
    let previous = parsed.request.previous_response_id.as_deref();
    let conversation = parsed.request.conversation.as_deref();
    if previous.is_some() && conversation.is_some() {
        return Err(HistoryError::Invalid(
            "previous_response_id/conversation: mutually exclusive".to_string(),
        ));
    }
    if previous.is_some() || conversation.is_some() {
        let Some(runtime) = state.responses.as_ref() else {
            return Err(HistoryError::NotFound(crate::i18n::t(
                "api-responses-store-unavailable",
            )));
        };
        let now = now_secs() as i64;
        let mut history: Vec<Value> = Vec::new();
        if let Some(previous) = previous {
            let mut cursor = Some(previous.to_string());
            let mut seen = std::collections::HashSet::new();
            let mut levels: Vec<Vec<Value>> = Vec::new();
            while let Some(id) = cursor {
                if !seen.insert(id.clone()) {
                    return Err(HistoryError::Invalid(format!(
                        "previous_response_id: cycle detected at {id:?}"
                    )));
                }
                if seen.len() > 100 {
                    return Err(HistoryError::Invalid(
                        "previous_response_id: chain depth exceeds 100".to_string(),
                    ));
                }
                let row = runtime
                    .store
                    .get_response(&id, tenant_hash, now)
                    .await
                    .map_err(|error| HistoryError::NotFound(error.to_string()))?
                    .ok_or_else(|| {
                        HistoryError::NotFound(crate::i18n::t_args(
                            "api-responses-not-found",
                            &[("id", id.clone())],
                        ))
                    })?;
                let mut level = parse_items_json(&row.input_items);
                level.extend(parse_items_json(&row.output_items));
                levels.push(level);
                cursor = row.parent_response_id;
            }
            for level in levels.into_iter().rev() {
                history.extend(level);
            }
        }
        if let Some(conversation) = conversation {
            let rows = runtime
                .store
                .list_conversation(conversation, tenant_hash, now)
                .await
                .map_err(|error| HistoryError::NotFound(error.to_string()))?;
            for row in rows {
                history.extend(parse_items_json(&row.input_items));
                history.extend(parse_items_json(&row.output_items));
            }
        }
        history.extend(parsed.current_items.iter().cloned());
        resolve_item_references(&mut history)?;
        Ok(history)
    } else {
        let mut items = parsed.current_items.clone();
        resolve_item_references(&mut items)?;
        Ok(items)
    }
}

fn parse_items_json(text: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(text).unwrap_or_default()
}

/// `item_reference` 在合并序列内按 `id` 替换；找不到 → 400。
fn resolve_item_references(items: &mut [Value]) -> Result<(), HistoryError> {
    let mut by_id: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for item in items.iter() {
        if item.get("type").and_then(Value::as_str) == Some("item_reference") {
            continue;
        }
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            by_id.insert(id.to_string(), item.clone());
        }
    }
    for item in items.iter_mut() {
        if item.get("type").and_then(Value::as_str) != Some("item_reference") {
            continue;
        }
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let resolved = by_id.get(&id).cloned().ok_or_else(|| {
            HistoryError::Invalid(format!(
                "item_reference: id {id:?} not found in replayed history"
            ))
        })?;
        *item = resolved;
    }
    Ok(())
}

/// `truncation="auto"`：按目录 context_window 粗粒度丢弃最旧 items
///（字符/4 估算；未登记模型不裁剪）。
fn truncate_for_context(state: &Arc<ApiState>, model: &str, items: &mut Vec<Value>) {
    let context_window = state
        .config
        .read()
        .model_catalog
        .iter()
        .find(|entry| entry.model_id.eq_ignore_ascii_case(model))
        .map(|entry| entry.context_window)
        .unwrap_or(0);
    if context_window == 0 {
        return;
    }
    let budget = (context_window as usize).saturating_mul(4);
    let size = |value: &Value| {
        serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or(0)
    };
    let mut total: usize = items.iter().map(size).sum();
    while total > budget && items.len() > 1 {
        let removed = items.remove(0);
        total = total.saturating_sub(size(&removed));
    }
}

/// 非流式成功响应落库（失败只 warn，不改变已返回的响应）。
async fn persist_response(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    parsed: &ParsedResponsesRequest,
    response: &ResponsesResponse,
    config: &crate::config::ResponsesSection,
) {
    if !ctx.store {
        return;
    }
    let Some(runtime) = state.responses.as_ref() else {
        return;
    };
    let now = now_secs() as i64;
    let row = build_stored_row(ctx, parsed, response, config);
    if let Some(row) = row {
        match runtime.store.insert_response(&row).await {
            Ok(()) => state.vendor_manager.metrics().record_responses_stored(),
            Err(error) => {
                state
                    .vendor_manager
                    .metrics()
                    .record_responses_store_failure();
                tracing::warn!(
                    error = %error,
                    "{}",
                    crate::i18n::t_args(
                        "api-responses-store-failed",
                        &[("error", error.to_string())]
                    )
                );
            }
        }
    }
    // 机会性清理过期行（上限 100，失败仅 debug）
    if let Err(error) = runtime.store.purge_expired_responses(now, 100).await {
        tracing::debug!(
            error = %error,
            "{}",
            crate::i18n::t_args(
                "api-responses-purge-failed",
                &[("error", error.to_string())]
            )
        );
    }
}

/// 流式终止快照落库（spawn 任务内调用）。
async fn persist_stream_response(
    state: &Arc<ApiState>,
    tenant_hash: &str,
    parsed: &ParsedResponsesRequest,
    response: &ResponsesResponse,
    config: &crate::config::ResponsesSection,
) {
    let Some(runtime) = state.responses.as_ref() else {
        return;
    };
    let ctx = ResponsesCtx {
        request_id: String::new(),
        start_time: std::time::Instant::now(),
        session_key: None,
        api_key_hash: tenant_hash.to_string(),
        store: true,
    };
    let now = now_secs() as i64;
    if let Some(row) = build_stored_row(&ctx, parsed, response, config) {
        match runtime.store.insert_response(&row).await {
            Ok(()) => state.vendor_manager.metrics().record_responses_stored(),
            Err(error) => {
                state
                    .vendor_manager
                    .metrics()
                    .record_responses_store_failure();
                tracing::warn!(
                    error = %error,
                    "{}",
                    crate::i18n::t_args(
                        "api-responses-store-failed",
                        &[("error", error.to_string())]
                    )
                );
            }
        }
    }
    if let Err(error) = runtime.store.purge_expired_responses(now, 100).await {
        tracing::debug!(
            error = %error,
            "{}",
            crate::i18n::t_args(
                "api-responses-purge-failed",
                &[("error", error.to_string())]
            )
        );
    }
}

fn build_stored_row(
    ctx: &ResponsesCtx,
    parsed: &ParsedResponsesRequest,
    response: &ResponsesResponse,
    config: &crate::config::ResponsesSection,
) -> Option<crate::storage::StoredResponse> {
    let input_items = serde_json::to_string(&parsed.current_items).unwrap_or_else(|_| "[]".into());
    let output_items = serde_json::to_string(&response.output).unwrap_or_else(|_| "[]".into());
    let response_json = serde_json::to_string(response).unwrap_or_else(|_| "{}".into());
    let limit = config.max_response_bytes as usize;
    if input_items.len() > limit || output_items.len() > limit || response_json.len() > limit {
        tracing::warn!(
            response_id = %response.id,
            limit,
            "{}",
            crate::i18n::t("api-responses-store-too-large")
        );
        return None;
    }
    // 存储时间用网关收流时刻（上游 created 仅作响应回显；用上游值会让
    // 回放 mock/历史数据因 expires_at 已过期而立即不可见）
    let created_at = now_secs() as i64;
    Some(crate::storage::StoredResponse {
        id: response.id.clone(),
        tenant_hash: ctx.api_key_hash.clone(),
        conversation_id: parsed.request.conversation.clone(),
        parent_response_id: parsed.request.previous_response_id.clone(),
        model: response.model.clone(),
        status: response.status.clone(),
        store: true,
        background: parsed.request.background,
        created_at,
        expires_at: created_at + (config.retention_hours as i64) * 3600,
        input_items,
        output_items,
        response_json,
    })
}

/// 请求日志（`endpoint=/v1/responses`、`input_format=responses`）。
async fn log_responses_request(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    decision: &RoutingDecision,
    parsed: &ParsedResponsesRequest,
    response: Option<&Value>,
    status_code: i32,
    error_message: Option<String>,
) {
    let messages = crate::api::truncate_payload(
        serde_json::to_string(&parsed.current_items).unwrap_or_default(),
    );
    let response_str = response
        .and_then(|value| serde_json::to_string(value).ok())
        .map(crate::api::truncate_payload);
    let usage = response.and_then(|value| value.get("usage"));
    let read = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_i64)
            .map(|value| value as i32)
    };
    let log = RequestLog {
        id: None,
        request_id: ctx.request_id.clone(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/responses".to_string(),
        input_format: "responses".to_string(),
        abstract_model: Some(parsed.request.model.clone()),
        messages,
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(status_code),
        response: response_str,
        error_message,
        prompt_tokens: read("input_tokens"),
        completion_tokens: read("output_tokens"),
        total_tokens: read("total_tokens"),
        duration_ms: Some(ctx.start_time.elapsed().as_millis() as i64),
        cost_usd: None,
    };
    if let Some(logger) = state.logger.as_ref() {
        if let Err(error) = logger.log(log).await {
            tracing::warn!(
                "{}",
                crate::i18n::t_args("api-log-push-rejected", &[("error", error.to_string())])
            );
        }
    }
}

/// 失败请求落库（`response = None`，status/error 取真实值）。
async fn log_responses_failure(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    decision: &RoutingDecision,
    parsed: &ParsedResponsesRequest,
    error: &FluxgateError,
) {
    log_responses_request(
        state,
        ctx,
        decision,
        parsed,
        None,
        crate::api::status_code_for_error(error),
        Some(error.to_string()),
    )
    .await;
}

/// 分账与配额回补（Responses usage 为 `input_tokens` 形态）。
async fn settle_nonstream(
    state: &Arc<ApiState>,
    ctx: &ResponsesCtx,
    body: &Value,
    response: &Value,
    decision: &RoutingDecision,
) {
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
        body,
        response,
        &decision.vendor.type_,
        &decision.vendor.id,
        Some(state.vendor_manager.metrics()),
    );
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

fn not_found(message: String) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"message": message, "type": "not_found"}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_reference_resolves_from_merged_history() {
        let mut items = vec![
            serde_json::json!({"type": "message", "id": "msg_1", "role": "user", "content": "hi"}),
            serde_json::json!({"type": "item_reference", "id": "msg_1"}),
        ];
        resolve_item_references(&mut items).expect("可解析");
        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["content"], "hi");
    }

    #[test]
    fn missing_item_reference_is_rejected() {
        let mut items = vec![serde_json::json!({"type": "item_reference", "id": "missing"})];
        let error = resolve_item_references(&mut items).expect_err("缺失必须拒绝");
        match error {
            HistoryError::Invalid(field) => assert!(field.contains("missing"), "{field}"),
            HistoryError::NotFound(message) => panic!("应为 400 而非 404: {message}"),
        }
    }

    #[test]
    fn stored_items_json_tolerates_garbage() {
        assert!(parse_items_json("not json").is_empty());
        assert_eq!(parse_items_json(r#"[{"a":1}]"#).len(), 1);
    }
}
