// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::GateError;
use crate::i18n::t_args;
use crate::{error::FluxgateError, router::RoutingDecision, ApiState};
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::api::response_cache::{CacheFlow, FinalRoute};

/// 流式响应公共头（透传与转换两条流式路径共用）。
const STREAM_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
];

/// 单请求横切参数束 — 路由解析后一次构造，供派发失败收尾、缓存早退与
/// 落库路径共用；LinUCB 奖励臂键在 log_request 内归一，不随 ctx 穿线
struct AnthropicDispatchCtx {
    request_id: String,
    start_time: std::time::Instant,
    session_key: Option<String>,
    api_key_hash: String,
    // 难度档位标签（None = 定档未发生；仅 [routing.difficulty] 命中 groups 时有值）
    difficulty_tier: Option<&'static str>,
    // N2 词法压缩是否发生（X-Compressed 响应头来源）
    lexcompressed: bool,
    // N1 batch 标记（X-Batch-Routed 响应头来源）
    batch: bool,
    // 调用级厂商选项（anthropic-beta 转发等）
    call_options: crate::vendor::manager::VendorCallOptions,
}

#[axum::debug_handler]
pub async fn messages(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<impl IntoResponse, FluxgateError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            FluxgateError::from(GateError::Validation {
                field: "model".into(),
            })
        })?;

    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 会话亲和键（X-Session-Id 优先，messages 前缀哈希兜底）。
    // Anthropic 入站保持既有无 hints 路由语义，仅接入会话亲和；
    // X-Weight 请求级权重一并传入（[routing].
    // allow_request_weight 门控在 Router 侧热读判定，关闭时忽略）。
    // N2 词法压缩：改写先于定档/缓存键/前缀哈希
    let lexcompressed =
        crate::middleware::lexcompress::apply_to_body(&state.config, &headers, &mut body);

    let hints = RoutingHints::from_headers(&headers);
    // 调用级厂商选项：anthropic-beta 由 manager 按厂商类型与开关门控
    let call_options = crate::vendor::manager::VendorCallOptions {
        anthropic_beta: hints.anthropic_beta.clone(),
    };
    let request_batch = hints.batch;
    let session_key = hints.effective_session_key(&body);
    // 亲和键租户维度（与响应缓存同源）
    let api_key_hash = crate::utils::tenant::api_key_digest(&headers);

    // 难度定档（[routing.difficulty]）：anthropic 入站无 hints 直连语义，
    // groups 命中即定档改写（bypass=false）。None 接线 = 历史行为
    let (difficulty_tier, routed_model) =
        crate::router::difficulty::resolve_tier(state.difficulty.as_deref(), &model, &body, false)
            .await;

    // 路由决策
    let decision = resolve_route(
        &state,
        &routed_model,
        &headers,
        hints,
        session_key.as_deref(),
        &api_key_hash,
        &body,
    )
    .await?;

    let ctx = AnthropicDispatchCtx {
        request_id,
        start_time,
        session_key,
        api_key_hash,
        difficulty_tier,
        lexcompressed,
        batch: request_batch && decision.vendor.supports_batch,
        call_options,
    };

    // 检查是否可以使用零拷贝透传（入站格式 Anthropic == 出站格式 Anthropic）。
    // 透传路径不做工具 schema 上限校验（字节保真是透传的设计承诺），
    // 兜底 = axum 体限 + auth + 限流（与 completions.rs 同口径）。
    let use_passthrough = matches!(
        decision.vendor.protocol(),
        crate::config::VendorProtocol::Anthropic
    );

    if use_passthrough {
        // 零拷贝路径：直接透传请求体
        if stream {
            return chat_passthrough_stream(&state, &ctx, &model, &body, decision).await;
        }
        return chat_passthrough_json(&state, &ctx, &headers, &model, &body, decision).await;
    }

    // 转换路径：Anthropic → Unified → 目标厂商
    let unified_request = state.anthropic_converter.to_unified(body.clone())?;

    if stream {
        return chat_convert_stream(&state, &ctx, &model, &body, decision, unified_request).await;
    }
    chat_convert_json(
        &state,
        &ctx,
        &headers,
        &model,
        &body,
        decision,
        unified_request,
    )
    .await
}

/// 透传流式路径（二期/3 阶段一/11/10(3)）：派发 → 首绑定亲和 →
/// 分帧转发 → 流式日志 → SSE 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_passthrough_stream(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, FluxgateError> {
    // cover_all_paths 开启时「首字节前换路」——
    // Err（均发生在响应头前，manager check_stream_status 早退）排除
    // 重派；Ok（流已建立）即交出 body，流中错误仍以 SSE error 事件
    // 终止不再换路。关闭时单发直连（decision 零克隆，历史语义）。
    // 尝试链经 fallback_trail 出站（开关关闭时为空表，零行为变化）。
    #[cfg(feature = "resilience")]
    let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
        state.config.read().resilience.expose_fallback_headers,
    );
    #[cfg(feature = "resilience")]
    let outcome = if state.config.read().resilience.cover_all_paths {
        // decision.clone() 进循环（原样）：失败收尾仍引用初始决策
        crate::api::execute_dispatch_with_fallback(
            state,
            model,
            decision.clone(),
            true,
            |d| {
                let vendor_manager = state.vendor_manager.clone();
                let body = body.clone();
                let call_options = ctx.call_options.clone();
                async move {
                    vendor_manager
                        .execute_passthrough_stream_request_with_options(
                            &d.vendor.id,
                            body,
                            call_options,
                        )
                        .await
                }
            },
            &mut fallback_trail,
        )
        .await
        .map_err(|e| Box::new((e, decision)))
    } else {
        fallback_trail.record(&decision.vendor.id);
        dispatch_direct(
            state
                .vendor_manager
                .execute_passthrough_stream_request_with_options(
                    &decision.vendor.id,
                    body.clone(),
                    ctx.call_options.clone(),
                )
                .await,
            decision,
        )
    };
    #[cfg(not(feature = "resilience"))]
    let outcome = dispatch_direct(
        state
            .vendor_manager
            .execute_passthrough_stream_request_with_options(
                &decision.vendor.id,
                body.clone(),
                ctx.call_options.clone(),
            )
            .await,
        decision,
    );
    let (stream_response, decision) = finish_dispatch(state, ctx, body, outcome).await?;

    // 派发成功即回写亲和绑定
    // 首绑定收敛（tenant_scoped 开启时 bind_if_absent）
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // 阶段一：透传流过分帧器逐帧原样转发；经分帧后上游
    // 网络中断以 SSE error 帧显性终止，不伪装成正常流结束。
    // 受配额约束的 key 才挂帧观测并在正常 EOF 回补，
    // 其余形态零包装（透传字节逐字节不变）。三级早退判定在 quota 模块单点。
    // 观测回调同时服务流式用量记账（占位日志行终止补写 + keyed 分账）。
    let recorder = crate::api::stream_usage::StreamUsageRecorder::new(
        state.clone(),
        ctx.request_id.clone(),
        crate::api::stream_usage::StreamProtocol::Anthropic,
        decision.vendor.id.clone(),
        decision.vendor.type_.clone(),
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        ctx.api_key_hash.clone(),
    );
    let observer = recorder.clone();
    let body_stream = crate::middleware::quota::passthrough_stream_body(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &decision.vendor.type_,
        stream_response,
        move |observation| match observation {
            crate::api::sse::ReframeObservation::Frame {
                data: Some(data), ..
            } => observer.observe_frame(data),
            crate::api::sse::ReframeObservation::End { .. } => observer.finish(),
            _ => {}
        },
    );

    log_stream_dispatch(state, ctx, &decision, body).await;

    // 流式在响应头阶段写入（派发已完成，链已知）；开关关闭为空表。
    #[cfg(feature = "resilience")]
    let obs_headers = crate::api::fallback_observation_headers(&decision, &fallback_trail);
    #[cfg(not(feature = "resilience"))]
    let obs_headers = axum::http::HeaderMap::new();
    let obs_headers = crate::api::extend_obs_with_tier(obs_headers, ctx.difficulty_tier);
    let obs_headers = crate::api::extend_obs_with_flags(obs_headers, ctx.batch, ctx.lexcompressed);
    Ok(crate::api::with_fallback_headers(
        (STREAM_HEADERS, body_stream).into_response(),
        &obs_headers,
    ))
}

/// 透传非流式路径（一期/12/11/10(3)）：get_or 闭包流（派发 + 换路 +
/// 重绑定 + 最终键回写）→ HIT 早退（0 结算）→ 成功日志 → X-Cache 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_passthrough_json(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, FluxgateError> {
    // leader 闭包 = 上游派发：resilience 换路循环 + finish_dispatch 失败
    // 收尾 + 亲和重绑定 + 观测头。闭包成功返回的 FinalDispatch 即
    // **最终成功者**（major#2：回写键/日志/结算以其为准，初始候选仅入
    // single-flight 去重键）。
    let dispatch = {
        let state = state.clone();
        let body = body.clone();
        let model = model.to_string();
        let initial_decision = decision.clone();
        move || {
            let state = state.clone();
            let body = body.clone();
            let model = model.clone();
            let initial_decision = initial_decision.clone();
            async move {
                // cover_all_paths 开启时包换路 fallback 循环（resilience
                // feature）——主 vendor 耗尽/429/5xx 时排除后重路由，
                // actual_vendor 记最终成功者；关闭时单发直连。
                // 尝试链经 fallback_trail 出站（开关关闭时为空表，
                // 零行为变化）。
                #[cfg(feature = "resilience")]
                let outcome = {
                    let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
                        state.config.read().resilience.expose_fallback_headers,
                    );
                    if state.config.read().resilience.cover_all_paths {
                        // decision.clone() 进循环（原样）：失败收尾仍引用初始决策
                        crate::api::execute_dispatch_with_fallback(
                            &state,
                            &model,
                            initial_decision.clone(),
                            true,
                            |d| {
                                let vendor_manager = state.vendor_manager.clone();
                                let body = body.clone();
                                let call_options = ctx.call_options.clone();
                                async move {
                                    vendor_manager
                                        .execute_passthrough_request_with_options(
                                            &d.vendor.id,
                                            body,
                                            call_options,
                                        )
                                        .await
                                }
                            },
                            &mut fallback_trail,
                        )
                        .await
                        .map(|(response, d)| {
                            (
                                (
                                    response,
                                    crate::api::fallback_observation_headers(&d, &fallback_trail),
                                ),
                                d,
                            )
                        })
                        .map_err(|e| Box::new((e, initial_decision)))
                    } else {
                        fallback_trail.record(&initial_decision.vendor.id);
                        dispatch_direct(
                            state
                                .vendor_manager
                                .execute_passthrough_request_with_options(
                                    &initial_decision.vendor.id,
                                    body.clone(),
                                    ctx.call_options.clone(),
                                )
                                .await
                                .map(|v| {
                                    (
                                        v,
                                        crate::api::fallback_observation_headers(
                                            &initial_decision,
                                            &fallback_trail,
                                        ),
                                    )
                                }),
                            initial_decision,
                        )
                    }
                };
                #[cfg(not(feature = "resilience"))]
                let outcome = dispatch_direct(
                    state
                        .vendor_manager
                        .execute_passthrough_request_with_options(
                            &initial_decision.vendor.id,
                            body.clone(),
                            ctx.call_options.clone(),
                        )
                        .await
                        .map(|v| (v, axum::http::HeaderMap::new())),
                    initial_decision,
                );
                // 失败收尾（亲和解除 + 失败落库）与 finish_dispatch 同源
                let ((response, obs_headers), decision) =
                    finish_dispatch(&state, ctx, &body, outcome).await?;
                // 执行成功回写亲和绑定（换路成功时 decision 已是最终
                // 成功者——失败迁移绑定语义）
                crate::api::rebind_session_affinity(
                    &state,
                    &ctx.api_key_hash,
                    ctx.session_key.as_deref(),
                    &decision.vendor.id,
                )
                .await;
                Ok((
                    response,
                    FinalDispatch {
                        decision,
                        obs_headers,
                    },
                ))
            }
        }
    };

    // get_or 闭包流（未接线 = 不参与缓存，闭包直派发）
    let flow = match state.response_cache.as_ref() {
        Some(cache) => {
            cache
                .get_or_response(
                    headers,
                    &decision.vendor.id,
                    &decision.model.model_id,
                    body,
                    dispatch,
                )
                .await?
        }
        None => {
            let (value, final_route) = dispatch().await?;
            CacheFlow {
                value,
                final_route: Some(final_route),
                computed: false,
                participated: false,
                semantic: false,
            }
        }
    };

    // HIT（快速路径/follower 复用）：无上游往返，实际用量按 0 结算回补
    // 入口预扣——否则同一请求的缓存重复命中会持续虚耗配额；
    // 成功日志用初始路由决策（复用响应来源请求的维度）。
    if flow.participated && !flow.computed {
        crate::middleware::quota::settle_nonstream_cached(
            &state.config,
            state.quota.as_ref(),
            &ctx.api_key_hash,
            body,
        );
        log_success_dispatch(state, ctx, &decision, body, &flow.value).await;
        let response = (
            [("Content-Type", "application/json"), ("X-Cache", "HIT")],
            Json(flow.value),
        )
            .into_response();
        return Ok(crate::api::with_difficulty_tier_response(
            response,
            ctx.difficulty_tier,
        ));
    }

    // leader / 自愈回源 / 未参与：以最终成功者收尾（computed → X-Cache: MISS）
    let final_route = flow
        .final_route
        .unwrap_or_else(|| panic!("{}", crate::i18n::t("api-final-route-missing")));
    log_success_dispatch(state, ctx, &final_route.decision, body, &flow.value).await;

    // 非流式响应后回补（有 usage 才结算；无 usage 保持预扣）。
    // vendor 协议透传：cache 包含语义与分账侧同一判定。
    crate::middleware::quota::settle_nonstream(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &flow.value,
        &final_route.decision.vendor.type_,
        &final_route.decision.vendor.id,
        Some(state.vendor_manager.metrics()),
    );

    // 缓存参与（Miss 回写完成）→ 响应带 X-Cache: MISS；
    // 回退观测头叠加（开关关闭为空表，零行为变化）。
    Ok(crate::api::with_fallback_headers(
        json_response_with_cache_flag_semantic(flow.value, flow.participated, flow.semantic),
        &crate::api::extend_obs_with_flags(
            crate::api::extend_obs_with_tier(final_route.obs_headers.clone(), ctx.difficulty_tier),
            ctx.batch,
            ctx.lexcompressed,
        ),
    ))
}

/// 转换流式路径（二期/3 阶段二/14②/11/10(3)）：派发 → 首绑定
/// 亲和 → IR→Anthropic 出站事件流（encoder 状态机）→ 流式日志 → SSE 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_convert_stream(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, FluxgateError> {
    // 阶段二：manager 返回分帧 + 反向解析后的 IR chunk 流
    // cover_all_paths 开启时「首字节前换路」（转换路径
    // UnifiedRequest 任意 vendor type 均合法，不按协议过滤）；关闭时
    // 单发直连（unified_request move，历史零克隆语义）
    // 尝试链经 fallback_trail 出站（开关关闭时为空表，零行为变化）。
    #[cfg(feature = "resilience")]
    let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
        state.config.read().resilience.expose_fallback_headers,
    );
    #[cfg(feature = "resilience")]
    let outcome = if state.config.read().resilience.cover_all_paths {
        // decision.clone() 进循环（原样）：失败收尾仍引用初始决策；
        // 换路循环逐 attempt 克隆请求体（原 fallback 语义）
        crate::api::execute_dispatch_with_fallback(
            state,
            model,
            decision.clone(),
            false,
            |d| {
                let vendor_manager = state.vendor_manager.clone();
                let unified_request = unified_request.clone();
                let call_options = ctx.call_options.clone();
                async move {
                    vendor_manager
                        .execute_stream_request_with_options(
                            &d.vendor.id,
                            unified_request,
                            call_options,
                        )
                        .await
                }
            },
            &mut fallback_trail,
        )
        .await
        .map_err(|e| Box::new((e, decision)))
    } else {
        fallback_trail.record(&decision.vendor.id);
        dispatch_direct(
            state
                .vendor_manager
                .execute_stream_request_with_options(
                    &decision.vendor.id,
                    unified_request,
                    ctx.call_options.clone(),
                )
                .await,
            decision,
        )
    };
    #[cfg(not(feature = "resilience"))]
    let outcome = dispatch_direct(
        state
            .vendor_manager
            .execute_stream_request_with_options(
                &decision.vendor.id,
                unified_request,
                ctx.call_options.clone(),
            )
            .await,
        decision,
    );
    let (chunk_stream, decision) = finish_dispatch(state, ctx, body, outcome).await?;

    // 转换流结束回补（捕获末个 Usage chunk 含 cache 两档；无 Usage 帧
    // 保持预扣）。三级早退（未接线/关闭/无限额租户）返回 None → 零包装
    // 直通，不克隆请求体与档源（H2：默认配置零开销）。
    let settle_ctx = crate::middleware::quota::SettlementCtx::for_request(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &decision.vendor.type_,
        &decision.vendor.id,
        Some(std::sync::Arc::clone(state.vendor_manager.metrics())),
    );
    let chunk_stream = crate::middleware::quota::settle_stream(chunk_stream, settle_ctx);

    // 派发成功即回写亲和绑定
    // 首绑定收敛（tenant_scoped 开启时 bind_if_absent）
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // IR → Anthropic 出站事件：AnthropicStreamEncoder 状态机补齐
    // message_start/content_block_start/…_delta/…_stop/message_stop
    // 生命周期（存量 stream_chunk_to_external 的单事件输出不成合法流）。
    // 每事件经 SSE 编码器加 `data:` 前缀（评审 blocker：裸 JSON 行
    // 不构成 SSE 事件，标准客户端收到零事件）。
    // 错误发带语义的 SSE error 事件后提前终止（不补
    // message_stop，客户端可分辨成败）
    let body_stream = anthropic_sse_stream(chunk_stream);

    log_stream_dispatch(state, ctx, &decision, body).await;

    // 流式在响应头阶段写入（派发已完成，链已知）；开关关闭为空表。
    #[cfg(feature = "resilience")]
    let obs_headers = crate::api::fallback_observation_headers(&decision, &fallback_trail);
    #[cfg(not(feature = "resilience"))]
    let obs_headers = axum::http::HeaderMap::new();
    let obs_headers = crate::api::extend_obs_with_tier(obs_headers, ctx.difficulty_tier);
    let obs_headers = crate::api::extend_obs_with_flags(obs_headers, ctx.batch, ctx.lexcompressed);
    Ok(crate::api::with_fallback_headers(
        (STREAM_HEADERS, body_stream).into_response(),
        &obs_headers,
    ))
}

/// 转换非流式路径：get_or 闭包流（派发恒走换路循环 + 统一响应回转 +
/// 重绑定）→ HIT 早退（0 结算）→ 成功日志 → X-Cache 响应。
async fn chat_convert_json(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, FluxgateError> {
    // leader 闭包 = 派发 + 回转：resilience 恒走换路 fallback 循环
    // （Throttled 原因追踪 + Retry-After 提示语义）+ 失败收尾（dispatch_
    // unified_request 内单点）+ 亲和重绑定；最终成功者随闭包返回
    // （major#2：回写键/日志/结算以其为准）。
    let dispatch = {
        let state = state.clone();
        let body = body.clone();
        let decision = decision.clone();
        move || {
            let state = state.clone();
            let body = body.clone();
            let decision = decision.clone();
            let unified_request = unified_request.clone();
            async move {
                let (unified_response, decision, obs_headers) =
                    dispatch_unified_request(&state, ctx, model, &body, decision, unified_request)
                        .await?;
                // 将统一响应转换回 Anthropic 格式
                let response = state
                    .anthropic_converter
                    .response_to_external(unified_response)?;
                // 执行成功回写亲和绑定（resilience 换路成功时 decision 已是
                // 最终成功者——失败迁移绑定语义）
                crate::api::rebind_session_affinity(
                    &state,
                    &ctx.api_key_hash,
                    ctx.session_key.as_deref(),
                    &decision.vendor.id,
                )
                .await;
                Ok((
                    response,
                    FinalDispatch {
                        decision,
                        obs_headers,
                    },
                ))
            }
        }
    };

    // get_or 闭包流（未接线 = 不参与缓存，闭包直派发）
    let flow = match state.response_cache.as_ref() {
        Some(cache) => {
            cache
                .get_or_response(
                    headers,
                    &decision.vendor.id,
                    &decision.model.model_id,
                    body,
                    dispatch,
                )
                .await?
        }
        None => {
            let (value, final_route) = dispatch().await?;
            CacheFlow {
                value,
                final_route: Some(final_route),
                computed: false,
                participated: false,
                semantic: false,
            }
        }
    };

    // HIT（快速路径/follower 复用）：0 结算回补 + 初始决策成功日志
    if flow.participated && !flow.computed {
        crate::middleware::quota::settle_nonstream_cached(
            &state.config,
            state.quota.as_ref(),
            &ctx.api_key_hash,
            body,
        );
        log_success_dispatch(state, ctx, &decision, body, &flow.value).await;
        let response = (
            [("Content-Type", "application/json"), ("X-Cache", "HIT")],
            Json(flow.value),
        )
            .into_response();
        return Ok(crate::api::with_difficulty_tier_response(
            response,
            ctx.difficulty_tier,
        ));
    }

    // leader / 自愈回源 / 未参与：以最终成功者收尾（computed → X-Cache: MISS）
    let final_route = flow
        .final_route
        .unwrap_or_else(|| panic!("{}", crate::i18n::t("api-final-route-missing")));
    log_success_dispatch(state, ctx, &final_route.decision, body, &flow.value).await;

    // 非流式响应后回补（转换路径 usage 恒有，必结算）。
    crate::middleware::quota::settle_nonstream(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &flow.value,
        &final_route.decision.vendor.type_,
        &final_route.decision.vendor.id,
        Some(state.vendor_manager.metrics()),
    );

    // 缓存参与 → 响应带 X-Cache 头；回退观测头叠加（开关关闭为空表）。
    Ok(crate::api::with_fallback_headers(
        json_response_with_cache_flag_semantic(flow.value, flow.participated, flow.semantic),
        &crate::api::extend_obs_with_flags(
            crate::api::extend_obs_with_tier(final_route.obs_headers.clone(), ctx.difficulty_tier),
            ctx.batch,
            ctx.lexcompressed,
        ),
    ))
}

/// 路由解析 — 无 hints 直连路由（仅接会话亲和与请求级权重）；LinUCB
/// 特征仅 linucb 档且接线时计算；x-deadline-ms 头由 lps_deadline_gate 门控
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
    // x-deadline-ms 头（语义同 openai chat；门控在 Router 侧）
    let lps_deadline_ms = headers
        .get("x-deadline-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
    // 前缀亲和 + 长上下文价 ctx：enabled 时算稳定前缀哈希与估 token 数
    let ctx = crate::api::route_ctx(state, body, hints.batch);
    let decision = state
        .router
        .route_weighted(
            model,
            session_key,
            Some(api_key_hash),
            &[],
            hints.preferred_weight,
            linucb_x.as_ref().map(|a| a.as_slice()),
            lps_deadline_ms,
            ctx,
        )
        .await?;
    Ok(decision)
}

/// 派发失败收尾单点：亲和解除 + 失败落库；换路耗尽与单发失败同语义。
/// 错误经调用方包装后透传
async fn on_dispatch_failure(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    body: &Value,
    decision: &RoutingDecision,
    error: FluxgateError,
) -> FluxgateError {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    crate::api::clear_session_affinity(state, &ctx.api_key_hash, ctx.session_key.as_deref()).await;
    log_failure(
        state,
        &ctx.request_id,
        "anthropic",
        decision,
        body,
        duration_ms,
        &error,
    )
    .await;
    error
}

/// 直连单发结果 → 统一收尾形状：错误携带初始决策，供 [`finish_dispatch`]
/// 单点失败收尾引用（直连分支 decision 全程零克隆，历史语义；Err 装箱
/// 避免 result_large_err 的栈搬运）。
fn dispatch_direct<T>(
    result: Result<T, GateError>,
    decision: RoutingDecision,
) -> Result<(T, RoutingDecision), Box<(GateError, RoutingDecision)>> {
    match result {
        Ok(v) => Ok((v, decision)),
        Err(e) => Err(Box::new((e, decision))),
    }
}

/// 派发结果收尾单点 —— Ok 原样透传；Err 走
/// [`on_dispatch_failure`]（亲和解除 + 失败落库）后透传错误。
async fn finish_dispatch<T>(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    body: &Value,
    outcome: Result<(T, RoutingDecision), Box<(GateError, RoutingDecision)>>,
) -> Result<(T, RoutingDecision), FluxgateError> {
    match outcome {
        Ok(pair) => Ok(pair),
        Err(boxed) => {
            let (e, decision) = *boxed;
            Err(on_dispatch_failure(state, ctx, body, &decision, FluxgateError::from(e)).await)
        }
    }
}

/// 非流式转换路径派发 — resilience 开启时恒走换路 fallback 循环，
/// 关闭时单发直连；失败收尾经 on_dispatch_failure 单点执行。
/// 返回（统一响应，最终决策，观测头表）
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn dispatch_unified_request(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    request: crate::protocol::UnifiedRequest,
) -> Result<
    (
        crate::protocol::UnifiedResponse,
        RoutingDecision,
        axum::http::HeaderMap,
    ),
    FluxgateError,
> {
    #[cfg(feature = "resilience")]
    {
        let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
            state.config.read().resilience.expose_fallback_headers,
        );
        return match crate::api::execute_request_with_fallback(
            state,
            model,
            decision.clone(),
            request,
            ctx.call_options.clone(),
            &mut fallback_trail,
        )
        .await
        {
            Ok((response, decision)) => {
                let obs = crate::api::fallback_observation_headers(&decision, &fallback_trail);
                Ok((response, decision, obs))
            }
            Err(e) => {
                Err(on_dispatch_failure(state, ctx, body, &decision, FluxgateError::from(e)).await)
            }
        };
    }
    #[cfg(not(feature = "resilience"))]
    match state
        .vendor_manager
        .execute_request_with_options(&decision.vendor.id, request, ctx.call_options.clone())
        .await
    {
        Ok(r) => Ok((r, decision, axum::http::HeaderMap::new())),
        Err(e) => {
            Err(on_dispatch_failure(state, ctx, body, &decision, FluxgateError::from(e)).await)
        }
    }
}

/// 派发产物：最终成功路由 + 观测头（换路循环在闭包内完成，链已知）。
/// 实现 [`FinalRoute`] 供 get_or 闭包流以最终 vendor/model 组装回写键
/// （评审修复 major#2）。
struct FinalDispatch {
    decision: RoutingDecision,
    obs_headers: axum::http::HeaderMap,
}

impl FinalRoute for FinalDispatch {
    fn vendor_id(&self) -> &str {
        &self.decision.vendor.id
    }
    fn model_id(&self) -> &str {
        &self.decision.model.model_id
    }
}

/// 缓存参与 → 响应带 `X-Cache: MISS` 头（HIT 已提前返回）；不参与则裸
/// JSON 头。
/// O4：语义命中变体——X-Cache: SEMANTIC-HIT（区别于精确 HIT/MISS）
fn json_response_with_cache_flag_semantic(
    response: Value,
    participated: bool,
    semantic: bool,
) -> axum::response::Response {
    if participated && semantic {
        (
            [
                ("Content-Type", "application/json"),
                ("X-Cache", "SEMANTIC-HIT"),
            ],
            Json(response),
        )
            .into_response()
    } else if participated {
        (
            [("Content-Type", "application/json"), ("X-Cache", "MISS")],
            Json(response),
        )
            .into_response()
    } else {
        ([("Content-Type", "application/json")], Json(response)).into_response()
    }
}

/// IR chunk 流 → Anthropic 出站 SSE 事件流：AnthropicStreamEncoder 补齐
/// 生命周期事件并加 `data:` 前缀；错误发 SSE error 事件后提前终止
fn anthropic_sse_stream(
    chunk_stream: impl futures::Stream<Item = Result<crate::protocol::StreamChunk, GateError>>
        + Send
        + 'static,
) -> axum::body::Body {
    let sse_stream = async_stream::stream! {
        let mut encoder = crate::protocol::AnthropicStreamEncoder::new();
        let mut chunk_stream = Box::pin(chunk_stream);
        let mut errored = false;
        while let Some(item) = futures::StreamExt::next(&mut chunk_stream).await {
            match item {
                Ok(chunk) => {
                    for event in encoder.encode_chunk(chunk) {
                        yield Ok::<bytes::Bytes, std::io::Error>(
                            crate::api::sse::encode_sse_event(&event, None).into(),
                        );
                    }
                }
                Err(e) => {
                    errored = true;
                    yield Ok(crate::api::sse::stream_error_frame(&e).into());
                    break;
                }
            }
        }
        if !errored {
            for event in encoder.finish() {
                yield Ok(crate::api::sse::encode_sse_event(&event, None).into());
            }
        }
    };
    axum::body::Body::from_stream(sse_stream)
}

/// 流式派发成功日志（metrics 已在 manager 层记账，handler 不
/// 重复计；日志按派发成功记 200，响应体为流占位标记）。
async fn log_stream_dispatch(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    decision: &RoutingDecision,
    body: &Value,
) {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    log_request(
        state,
        &ctx.request_id,
        "anthropic",
        decision,
        body,
        Some(&json!({"stream": true})),
        duration_ms,
        200,
        None,
    )
    .await;
}

/// 非流式派发成功日志（200；token 从响应 usage 取，语义见
/// [`log_request`]）。
async fn log_success_dispatch(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    decision: &RoutingDecision,
    body: &Value,
    response: &Value,
) {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    log_request(
        state,
        &ctx.request_id,
        "anthropic",
        decision,
        body,
        Some(response),
        duration_ms,
        200,
        None,
    )
    .await;
    // 分账落库（hash 透传 + cache 两类 + 四档单价）。
    crate::api::record_keyed_usage(
        state,
        &decision.vendor.id,
        &decision.vendor.type_,
        &decision.model.model_id,
        &ctx.api_key_hash,
        response,
    )
    .await;
}

/// 异步日志 push：try_send 背压绝不阻塞响应路径；token 从响应 usage 取，
/// status_code 从实际结果取，大 payload（>64KB）截断；logger 未接线时跳过
#[allow(clippy::too_many_arguments)]
async fn log_request(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    response: Option<&Value>,
    duration_ms: i64,
    status_code: i32,
    error_message: Option<String>,
) {
    // LinUCB 奖励回传 — 与落库记账同点（本函数收敛成功/失败
    // 两路，`response: None` = 失败）。门控（接线 + selection 热读）在
    // ApiState 内，默认 weighted 档零开销、无状态更新。
    // 评审修复 major：臂键与选择侧同源归一（anthropic 无 hints 路由，
    // 即 route_inner 的 abstract_model.to_lowercase()/auto→__global__）。
    state.record_linucb_reward(
        Some(
            crate::router::bandit::linucb_arm_key(
                request.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            )
            .as_str(),
        ),
        &decision.vendor.id,
        request,
        response.is_some(),
    );

    let messages = crate::api::truncate_payload(
        request
            .get("messages")
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_default(),
    );
    let response_str = response
        .and_then(|r| serde_json::to_string(r).ok())
        .map(crate::api::truncate_payload);

    // tokens 从响应 usage 取——**双格式**。本端点的
    // 响应既可能是 Anthropic 格式（透传路径的原始上游 JSON；转换路径经
    // anthropic_converter::response_to_external 产出 {"input_tokens",
    // "output_tokens"}，见 anthropic_converter.rs）。
    // 两套键名都做提取，避免 token 字段漏记。
    // prompt 缺失回退 input_tokens、completion 缺失回退 output_tokens；
    // Anthropic 无 total_tokens 键，缺省时以 prompt+completion 合成。
    let usage = response.and_then(|r| r.get("usage"));
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("input_tokens")))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let completion_tokens = usage
        .and_then(|u| {
            u.get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
        })
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .or_else(|| match (prompt_tokens, completion_tokens) {
            (Some(p), Some(c)) => Some(p + c),
            _ => None,
        });

    // usage 进预聚合器已迁移至 分账路径（`log_success_dispatch` 经
    // `record_keyed_usage` 记账：hash 透传 + cache 两类 + 四档单价）。
    // 此处保留 usage 提取（RequestLog 落库仍需 prompt/completion/total）。

    let log = RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/messages".to_string(),
        input_format: input_format.to_string(),
        abstract_model: request
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        messages,
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(status_code),
        response: response_str,
        error_message,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        duration_ms: Some(duration_ms),
        cost_usd: None,
    };

    // logger 未接线 = 降级为不落日志（不回退同步直写）
    if let Some(logger) = state.logger.as_ref() {
        if let Err(e) = logger.log(log).await {
            tracing::warn!(
                "{}",
                t_args("api-log-push-rejected", &[("error", e.to_string())])
            );
        }
    }
}

/// 失败路径落库 — 在 `?` 早退前补记（status_code/error_message
/// 真实值），消除"失败请求不留痕"的盲区。
#[allow(clippy::too_many_arguments)]
async fn log_failure(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    duration_ms: i64,
    error: &FluxgateError,
) {
    log_request(
        state,
        request_id,
        input_format,
        decision,
        request,
        None,
        duration_ms,
        crate::api::status_code_for_error(error),
        Some(error.to_string()),
    )
    .await;
}
