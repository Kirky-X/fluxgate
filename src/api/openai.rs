// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::protocol::embedding_request_to_openai;
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

/// 单请求横切参数束 — 路由解析后一次构造，供派发失败收尾、缓存早退与落库路径共用。
struct ChatDispatchCtx {
    request_id: String,
    start_time: std::time::Instant,
    session_key: Option<String>,
    api_key_hash: String,
    // 奖励臂键（None = X-Vendor 直连跳过奖励）
    linucb_reward_key: Option<String>,
    // 难度档位标签（None = 定档未发生；仅 [routing.difficulty] 命中 groups 时有值）
    difficulty_tier: Option<&'static str>,
    // N2 词法压缩是否发生（X-Compressed 响应头来源）
    lexcompressed: bool,
    // N1 batch 标记（X-Batch-Routed 响应头来源）
    batch: bool,
}

#[axum::debug_handler]
pub async fn chat_completions(
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

    // N2 词法压缩（[lexcompress] + X-Compress: 1）：改写 body 必须先于
    // 特征/缓存键/前缀哈希，保证下游口径一致
    let lexcompressed =
        crate::middleware::lexcompress::apply_to_body(&state.config, &headers, &mut body);

    // 提取路由提示与会话亲和键（X-Session-Id 优先，messages 前缀哈希兜底）
    let hints = RoutingHints::from_headers(&headers);
    let request_batch = hints.batch;
    let session_key = hints.effective_session_key(&body);
    // 亲和键租户维度（与响应缓存同源，从已验证 key 派生；affinity_tenant_scoped 关闭时不参与键构造）
    let api_key_hash = crate::utils::tenant::api_key_digest(&headers);

    // 难度定档（[routing.difficulty]）：显式 hints 直连不接定档（语义同
    // 亲和）。None 接线 = 历史行为
    let (difficulty_tier, routed_model) = crate::router::difficulty::resolve_tier(
        state.difficulty.as_deref(),
        &model,
        &body,
        hints.has_hints(),
    )
    .await;

    // 路由决策（支持 X-Vendor 和 X-Model 请求头；无 hints 时走亲和路由）
    let (decision, linucb_reward_key) = resolve_route(
        &state,
        &routed_model,
        &headers,
        hints,
        session_key.as_deref(),
        &api_key_hash,
        &body,
    )
    .await?;

    let ctx = ChatDispatchCtx {
        request_id,
        start_time,
        session_key,
        api_key_hash,
        linucb_reward_key,
        difficulty_tier,
        lexcompressed,
        batch: request_batch && decision.vendor.supports_batch,
    };

    // 零拷贝透传（入站格式 OpenAI == 出站格式 OpenAI）；透传路径不做工具
    // schema 上限校验（字节保真是设计承诺），兜底 = axum 体限 + auth + 限流。
    let use_passthrough = decision.vendor.type_ == "openai";

    if use_passthrough {
        if stream {
            return chat_passthrough_stream(&state, &ctx, &model, &body, decision).await;
        }
        return chat_passthrough_json(&state, &ctx, &headers, &model, &body, decision).await;
    }

    // 转换路径：OpenAI → Unified → 目标厂商
    let unified_request = state.openai_converter.to_unified(body.clone())?;

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

/// 透传流式路径：派发 → 首绑定亲和 → 分帧转发 → 流式日志 → SSE 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_passthrough_stream(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, FluxgateError> {
    // cover_all_paths 开启时「首字节前换路」——Err（均发生在响应头前，
    // manager check_stream_status 早退）排除重派；Ok（流已建立）即交出 body，
    // 流中错误仍以 SSE error 事件终止不再换路。关闭时单发直连。
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
                async move {
                    vendor_manager
                        .execute_passthrough_stream_request(&d.vendor.id, body)
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
                .execute_passthrough_stream_request(&decision.vendor.id, body.clone())
                .await,
            decision,
        )
    };
    #[cfg(not(feature = "resilience"))]
    let outcome = dispatch_direct(
        state
            .vendor_manager
            .execute_passthrough_stream_request(&decision.vendor.id, body.clone())
            .await,
        decision,
    );
    let (stream_response, decision) = finish_dispatch(state, ctx, body, outcome).await?;

    // 派发成功即回写亲和绑定（流后续错误由 metrics 记账）；首绑定收敛
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // 受配额约束的 key 才挂帧观测并在正常 EOF 回补，其余形态零包装
    //（透传字节逐字节不变）；三级早退判定在 quota 模块单点。
    let body_stream = crate::middleware::quota::passthrough_stream_body(
        &state.config,
        state.quota.as_ref(),
        &ctx.api_key_hash,
        body,
        &decision.vendor.type_,
        stream_response,
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

/// 透传非流式路径：get_or 闭包流（派发 + 换路 + 重绑定 + 最终键回写）→
/// HIT 早退（0 结算）→ 成功日志 → X-Cache 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_passthrough_json(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, FluxgateError> {
    // leader 闭包 = 上游派发：resilience 换路循环 + finish_dispatch 失败
    // 收尾 + 亲和重绑定 + 观测头。闭包成功返回的 FinalDispatch 即
    // **最终成功者**（回写键/日志/结算以其为准，初始候选仅入去重键）。
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
                // 尝试链经 fallback_trail 出站（开关关闭时为空表，零行为变化）。
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
                                async move {
                                    vendor_manager
                                        .execute_passthrough_request(&d.vendor.id, body)
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
                                .execute_passthrough_request(
                                    &initial_decision.vendor.id,
                                    body.clone(),
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
                        .execute_passthrough_request(&initial_decision.vendor.id, body.clone())
                        .await
                        .map(|v| (v, axum::http::HeaderMap::new())),
                    initial_decision,
                );
                let ((response, obs_headers), decision) =
                    finish_dispatch(&state, ctx, &body, outcome).await?;
                // 执行成功回写亲和绑定（换路成功时 decision 已是最终成功者）
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
    // 入口预扣——否则缓存重复命中会持续虚耗配额；成功日志用初始路由决策。
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
        .unwrap_or_else(|| panic!("{}", t("api-final-route-missing")));
    log_success_dispatch(state, ctx, &final_route.decision, body, &flow.value).await;

    // 非流式响应后回补（有 usage 才结算；无 usage 保持预扣）；cache 包含
    // 语义与分账侧同一判定。
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

    Ok(crate::api::with_fallback_headers(
        json_response_with_cache_flag_semantic(flow.value, flow.participated, flow.semantic),
        &crate::api::extend_obs_with_flags(
            crate::api::extend_obs_with_tier(final_route.obs_headers.clone(), ctx.difficulty_tier),
            ctx.batch,
            ctx.lexcompressed,
        ),
    ))
}

/// 转换流式路径：派发 → 首绑定亲和 → IR→OpenAI 出站事件流 → 流式日志 → SSE 响应。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn chat_convert_stream(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, FluxgateError> {
    // manager 返回分帧 + 反向解析后的 IR chunk 流；cover_all_paths 开启时
    // 「首字节前换路」（转换路径 UnifiedRequest 任意 vendor type 均合法，
    // 不按协议过滤）；关闭时单发直连。尝试链经 fallback_trail 出站
    //（开关关闭时为空表，零行为变化）。
    #[cfg(feature = "resilience")]
    let mut fallback_trail = crate::vendor::resilience::AttemptTrail::new(
        state.config.read().resilience.expose_fallback_headers,
    );
    #[cfg(feature = "resilience")]
    let outcome = if state.config.read().resilience.cover_all_paths {
        // decision.clone() 进循环：失败收尾仍引用初始决策；换路循环逐 attempt 克隆请求体
        crate::api::execute_dispatch_with_fallback(
            state,
            model,
            decision.clone(),
            false,
            |d| {
                let vendor_manager = state.vendor_manager.clone();
                let unified_request = unified_request.clone();
                async move {
                    vendor_manager
                        .execute_stream_request(&d.vendor.id, unified_request)
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
                .execute_stream_request(&decision.vendor.id, unified_request)
                .await,
            decision,
        )
    };
    #[cfg(not(feature = "resilience"))]
    let outcome = dispatch_direct(
        state
            .vendor_manager
            .execute_stream_request(&decision.vendor.id, unified_request)
            .await,
        decision,
    );
    let (chunk_stream, decision) = finish_dispatch(state, ctx, body, outcome).await?;

    // 转换流结束回补（捕获末个 Usage chunk 含 cache 两档；无 Usage 帧保持
    // 预扣）。三级早退（未接线/关闭/无限额租户）返回 None → 零包装直通。
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

    // 派发成功即回写亲和绑定；首绑定收敛（tenant_scoped 开启时 bind_if_absent）
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // IR → OpenAI 出站事件；错误发带语义的 SSE error 事件后提前终止，不再补
    // [DONE] —— 客户端与重试逻辑可分辨成败
    let converter = state.openai_converter.clone();
    let body_stream = openai_sse_stream(converter, chunk_stream);

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
    ctx: &ChatDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, FluxgateError> {
    // leader 闭包 = 派发 + 回转：resilience 恒走换路 fallback 循环
    // （Throttled 原因追踪 + Retry-After 提示语义）+ 失败收尾（dispatch_
    // unified_request 内单点）+ 亲和重绑定；最终成功者随闭包返回。
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
                // 将统一响应转换回 OpenAI 格式
                let response = state
                    .openai_converter
                    .response_to_external(unified_response)?;
                // 执行成功回写亲和绑定（换路成功时 decision 已是最终成功者）
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
        .unwrap_or_else(|| panic!("{}", t("api-final-route-missing")));
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

/// 路由解析 — X-Vendor/X-Model hints 直连 vs 亲和 weighted 路由二选一；
/// LinUCB 特征仅 linucb 档且接线时计算；x-deadline-ms 头由 lps_deadline_gate 门控。
/// 返回 (路由决策, LinUCB 奖励臂键)：X-Vendor 直连 → None（未选择不更新）；
/// X-Model → 该模型归一项；其余 → body.model 归一项
async fn resolve_route(
    state: &Arc<ApiState>,
    model: &str,
    headers: &HeaderMap,
    hints: RoutingHints,
    session_key: Option<&str>,
    api_key_hash: &str,
    body: &Value,
) -> Result<(RoutingDecision, Option<String>), FluxgateError> {
    // X-Weight 请求级权重随 hints 传入——是否生效由 [routing].allow_request_weight
    // 门控（Router 侧热读判定，关闭时忽略）
    let request_weight = hints.preferred_weight;
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
    let linucb_features = linucb_x.as_ref().map(|a| a.as_slice());
    let linucb_reward_key = if hints.preferred_vendor.is_some() {
        None
    } else {
        Some(crate::router::bandit::linucb_arm_key(
            hints.preferred_model.as_deref().unwrap_or(model),
        ))
    };
    // x-deadline-ms 头（可选，非法值忽略）
    let lps_deadline_ms = headers
        .get("x-deadline-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
    let ctx = crate::api::route_ctx(state, body, hints.batch);
    let decision = if hints.has_hints() {
        state
            .router
            .route_with_hints(
                model,
                hints.preferred_vendor.as_deref(),
                hints.preferred_model.as_deref(),
                request_weight,
                Some(api_key_hash),
                linucb_features,
                lps_deadline_ms,
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
                request_weight,
                linucb_features,
                lps_deadline_ms,
                ctx,
            )
            .await?
    };
    Ok((decision, linucb_reward_key))
}

/// 派发失败收尾单点 —「亲和解除 + 失败落库」收敛于此；换路耗尽与单发
/// 失败同语义（最终失败解除亲和绑定，避免旧键指向已失败 vendor）。
async fn on_dispatch_failure(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    body: &Value,
    decision: &RoutingDecision,
    error: FluxgateError,
) -> FluxgateError {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    crate::api::clear_session_affinity(state, &ctx.api_key_hash, ctx.session_key.as_deref()).await;
    log_chat_failure(
        state,
        &ctx.request_id,
        "openai",
        decision,
        body,
        duration_ms,
        &error,
        ctx.linucb_reward_key.as_deref(),
    )
    .await;
    error
}

/// 直连单发结果 → 统一收尾形状：错误携带初始决策，供 [`finish_dispatch`]
/// 单点失败收尾引用；Err 装箱避免 result_large_err 的栈搬运。
fn dispatch_direct<T>(
    result: Result<T, GateError>,
    decision: RoutingDecision,
) -> Result<(T, RoutingDecision), Box<(GateError, RoutingDecision)>> {
    match result {
        Ok(v) => Ok((v, decision)),
        Err(e) => Err(Box::new((e, decision))),
    }
}

/// 派发结果收尾单点 —— Ok 原样透传；Err 走 [`on_dispatch_failure`]（亲和解除 + 失败落库）后透传错误。
async fn finish_dispatch<T>(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
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

/// 非流式转换路径派发 — resilience 开启时恒走换路 fallback 循环，关闭时
/// 单发直连；失败收尾经 on_dispatch_failure 单点执行。
/// 返回三元组（统一响应，最终决策，观测头表——开关关闭/特性关闭时为空表）。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn dispatch_unified_request(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
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
        .execute_request(&decision.vendor.id, request)
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

/// 阶段二 / IR chunk 流 → OpenAI 出站 SSE 事件流。
/// 错误发带语义的 SSE error 事件后提前终止，不再补 [DONE] —— 客户端与
/// 重试逻辑可分辨成败（旧实现错误后仍以 [DONE] 收尾伪装正常结束）。
fn openai_sse_stream(
    converter: Arc<dyn crate::protocol::ProtocolConverter>,
    chunk_stream: impl futures::Stream<Item = Result<crate::protocol::StreamChunk, GateError>>
        + Send
        + 'static,
) -> axum::body::Body {
    let sse_stream = async_stream::stream! {
        let mut chunk_stream = Box::pin(chunk_stream);
        // 流级一致性归一化（首块 role + chunk 间 id/created 一致）。
        let mut encoder = crate::protocol::OpenAIStreamEncoder::new();
        let mut errored = false;
        while let Some(item) = futures::StreamExt::next(&mut chunk_stream).await {
            match item {
                Ok(chunk) => match converter.stream_chunk_to_external(chunk) {
                    Ok(event) => {
                        let event = encoder.normalize(event);
                        yield Ok::<bytes::Bytes, std::io::Error>(
                            crate::api::sse::encode_sse_event(&event.to_string(), None)
                                .into(),
                        );
                    }
                    Err(e) => {
                        errored = true;
                        yield Ok(crate::api::sse::stream_error_frame(&e).into());
                        break;
                    }
                },
                Err(e) => {
                    errored = true;
                    yield Ok(crate::api::sse::stream_error_frame(&e).into());
                    break;
                }
            }
        }
        if !errored {
            yield Ok(crate::api::sse::encode_sse_done().into());
        }
    };
    axum::body::Body::from_stream(sse_stream)
}

/// 流式派发成功日志（metrics 已在 manager 层记账，handler 不
/// 重复计；日志按派发成功记 200，响应体为流占位标记）。
async fn log_stream_dispatch(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    decision: &RoutingDecision,
    body: &Value,
) {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    log_chat_request(
        state,
        &ctx.request_id,
        "openai",
        decision,
        body,
        Some(&json!({"stream": true})),
        duration_ms,
        200,
        None,
        ctx.linucb_reward_key.as_deref(),
    )
    .await;
}

/// 非流式派发成功日志（200；token 从响应 usage 取，语义见
/// [`log_chat_request`]）。
async fn log_success_dispatch(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    decision: &RoutingDecision,
    body: &Value,
    response: &Value,
) {
    let duration_ms = ctx.start_time.elapsed().as_millis() as i64;
    log_chat_request(
        state,
        &ctx.request_id,
        "openai",
        decision,
        body,
        Some(response),
        duration_ms,
        200,
        None,
        ctx.linucb_reward_key.as_deref(),
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
/// status_code 从实际结果取，大 payload（>64KB）截断。
/// `response: None` = 失败路径，status_code/error_message 由调用方传真实值；
/// logger 未接线时跳过落库（不做同步直写）
#[allow(clippy::too_many_arguments)]
async fn log_chat_request(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    response: Option<&Value>,
    duration_ms: i64,
    status_code: i32,
    error_message: Option<String>,
    // 奖励臂键（None = 直连路径跳过奖励）
    linucb_arm_key: Option<&str>,
) {
    // LinUCB 奖励回传 — 与落库记账同点（本函数收敛成功/失败
    // 两路，`response: None` = 失败）。臂键由 handler 按路由分支归一
    // （评审修复 major，见 resolve_route 的 linucb_reward_key）。
    state.record_linucb_reward(
        linucb_arm_key,
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

    // prompt/completion/total tokens 从响应 usage 取
    //（旧实现 prompt_tokens 填的是请求 max_tokens —— 语义错误）
    let usage = response.and_then(|r| r.get("usage"));
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    // usage 进预聚合器已迁移至 分账路径（`log_success_dispatch` 经
    // `record_keyed_usage` 记账：hash 透传 + cache 两类 + 四档单价）。
    // 此处保留 usage 提取（RequestLog 落库仍需 prompt/completion/total）。

    let log = RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/chat/completions".to_string(),
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
        // push 失败仅可能为 logger 已停机 — 记 warn，不影响响应
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
#[allow(clippy::too_many_arguments)] // 落库上下文穿线
async fn log_chat_failure(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    duration_ms: i64,
    error: &FluxgateError,
    // 奖励臂键（None = 直连路径跳过奖励）
    linucb_arm_key: Option<&str>,
) {
    log_chat_request(
        state,
        request_id,
        input_format,
        decision,
        request,
        None,
        duration_ms,
        crate::api::status_code_for_error(error),
        Some(error.to_string()),
        linucb_arm_key,
    )
    .await;
}

/// embeddings 走 VendorManager 治理路径（共享连接池、限流、token 记账、
/// metrics 全部生效）；上游非 2xx → VendorUnavailable；日志同 chat 走异步管道
#[axum::debug_handler]
pub async fn embeddings(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, FluxgateError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();
    // 请求方 key 摘要透传分账（与响应缓存同源）。
    let api_key_hash = crate::utils::tenant::api_key_digest(&headers);

    // 路由决策 - embedding 使用特殊路由
    let model = body.get("model").and_then(|v| v.as_str()).ok_or_else(|| {
        FluxgateError::from(GateError::Validation {
            field: "model".into(),
        })
    })?;

    let decision = match state.router.route_embedding(model).await {
        Ok(d) => d,
        Err(e) => return Err(e.into()),
    };

    // 只支持 OpenAI vendor
    if decision.vendor.type_ != "openai" {
        return Err(FluxgateError::from(GateError::Protocol {
            source: t("api-embeddings-openai-only"),
            inner: None,
        }));
    }

    // 转换为 OpenAI 格式
    let openai_body = embedding_request_to_openai(body.clone())?;

    // 治理路径执行
    let response_json = match state
        .vendor_manager
        .execute_passthrough_request_at(&decision.vendor.id, openai_body, "embeddings")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let e = FluxgateError::from(e);
            // 失败落库：status_code/error_message 真实值
            let duration_ms = start_time.elapsed().as_millis() as i64;
            let input_str = body
                .get("input")
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .unwrap_or_default();
            let decision_ref = &decision;
            let error_str = e.to_string();
            push_embeddings_log(
                &state,
                &request_id,
                model,
                decision_ref,
                &input_str,
                None,
                duration_ms,
                crate::api::status_code_for_error(&e),
                Some(error_str),
                &api_key_hash,
            )
            .await;
            return Err(e);
        }
    };

    // 记录日志
    let input_str = body
        .get("input")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();

    // 维度守门：无 dimensions 参数的请求参与记录/比对（首见放行；维度漂移 →
    // 502 且向量不落给客户端，错误体含 vendor/model/新旧维度值）。带
    // dimensions 参数的请求由上游按参数裁剪维度，不写记录也不比对。
    // batch 响应逐元素校验（同批各元素与同一基准比对）；base64 编码形态
    // （embedding 为字符串）不解码守门，debug 日志显性化跳过
    if body.get("dimensions").map(|v| v.is_null()).unwrap_or(true) {
        if let Some(pin) = state.router.embedding_pin() {
            let data = response_json.get("data").and_then(|data| data.as_array());
            if data.is_some_and(|data| {
                data.iter()
                    .any(|item| item.get("embedding").is_some_and(|e| e.is_string()))
            }) {
                tracing::debug!(
                    vendor = %decision.vendor.id,
                    model = %decision.model.model_id,
                    "embedding dimension guard skipped: base64-encoded response"
                );
            } else if let Some(data) = data {
                let mut mismatch = None;
                for item in data {
                    let Some(dims) = item
                        .get("embedding")
                        .and_then(|embedding| embedding.as_array())
                        .map(|embedding| embedding.len() as u32)
                    else {
                        continue;
                    };
                    if let Err((old, now)) = pin.record_and_check(
                        &decision.vendor.id,
                        &decision.model.model_id,
                        "-",
                        dims,
                    ) {
                        mismatch = Some((old, now));
                        break;
                    }
                }
                if let Some((old, now)) = mismatch {
                    state
                        .vendor_manager
                        .metrics()
                        .record_embedding_dimension_mismatch();
                    let error = FluxgateError::from(GateError::Protocol {
                        source: t_args(
                            "api-embeddings-dimension-mismatch",
                            &[
                                ("vendor", decision.vendor.id.clone()),
                                ("model", decision.model.model_id.clone()),
                                ("old", old.to_string()),
                                ("new", now.to_string()),
                                // pin 键是 abstract（路由归一后），重置指引必须
                                // 指向同源键；model 仅用于定位漂移厂商条目
                                ("abstract", crate::router::bandit::linucb_arm_key(model)),
                            ],
                        ),
                        inner: None,
                    });
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    let error_message = error.to_string();
                    push_embeddings_log(
                        &state,
                        &request_id,
                        model,
                        &decision,
                        &input_str,
                        None,
                        duration_ms,
                        502,
                        Some(error_message),
                        &api_key_hash,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
    }

    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    push_embeddings_log(
        &state,
        &request_id,
        model,
        &decision,
        &input_str,
        Some(&response_json),
        duration_ms,
        200,
        None,
        &api_key_hash,
    )
    .await;

    Ok(Json(response_json))
}

/// embeddings 的异步日志 push（同 log_chat_request 的背压
/// 语义；usage 从响应取，token 语义一致）。
#[allow(clippy::too_many_arguments)]
async fn push_embeddings_log(
    state: &Arc<ApiState>,
    request_id: &str,
    model: &str,
    decision: &RoutingDecision,
    input_str: &str,
    response: Option<&Value>,
    duration_ms: i64,
    status_code: i32,
    error_message: Option<String>,
    api_key_hash: &str,
) {
    let usage = response.and_then(|r| r.get("usage"));
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    // embeddings usage 进预聚合器已迁移至 分账路径（本函数经
    // `record_keyed_usage` 记账：hash 由调用方透传 + 四档单价）。
    // 此处保留 usage 提取（RequestLog 落库仍需 prompt/completion/total）。
    if let Some(response) = response {
        crate::api::record_keyed_usage(
            state,
            &decision.vendor.id,
            &decision.vendor.type_,
            &decision.model.model_id,
            api_key_hash,
            response,
        )
        .await;
    }

    let log = RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/embeddings".to_string(),
        input_format: "openai".to_string(),
        abstract_model: Some(model.to_string()),
        messages: crate::api::truncate_payload(input_str.to_string()),
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(status_code),
        response: response
            .and_then(|r| serde_json::to_string(r).ok())
            .map(crate::api::truncate_payload),
        error_message,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        duration_ms: Some(duration_ms),
        cost_usd: None,
    };

    if let Some(logger) = state.logger.as_ref() {
        if let Err(e) = logger.log(log).await {
            tracing::warn!(
                "{}",
                t_args("api-log-push-rejected", &[("error", e.to_string())])
            );
        }
    }
}
