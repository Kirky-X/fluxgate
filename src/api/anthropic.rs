// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::GateError;
use crate::i18n::t_args;
use crate::{error::NexusFlowError, router::RoutingDecision, ApiState};
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::api::response_cache::CacheOutcome;

/// 流式响应公共头（透传与转换两条流式路径共用）。
const STREAM_HEADERS: [(&str, &str); 3] = [
    ("Content-Type", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
];

/// 单请求横切参数束 — 路由解析后一次构造，供派发失败收尾、缓存早退与
/// 落库路径共用（收敛原 8 段重复收尾的长参数列表）。LinUCB 奖励臂键在
/// [`log_request`] 内按 body.model 归一（anthropic 无 hints 路由语义），
/// 不随 ctx 穿线。
struct AnthropicDispatchCtx {
    request_id: String,
    start_time: std::time::Instant,
    session_key: Option<String>,
    api_key_hash: String,
}

#[axum::debug_handler]
pub async fn messages(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, NexusFlowError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body.get("model").and_then(|v| v.as_str()).ok_or_else(|| {
        NexusFlowError::from(GateError::Validation {
            field: "model".into(),
        })
    })?;

    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // recId=11：会话亲和键（X-Session-Id 优先，messages 前缀哈希兜底）。
    // Anthropic 入站保持既有无 hints 路由语义，仅接入会话亲和；
    // recId=19(a)：X-Weight 请求级权重一并传入（[routing].
    // allow_request_weight 门控在 Router 侧热读判定，关闭时忽略）。
    let hints = RoutingHints::from_headers(&headers);
    let session_key = hints.effective_session_key(&body);
    // recId=10(2)：亲和键租户维度（与响应缓存 cache_api_key_hash 同源）
    let api_key_hash = crate::api::response_cache::cache_api_key_hash(&headers);

    // 路由决策
    let decision = resolve_route(
        &state,
        model,
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
    };

    // 检查是否可以使用零拷贝透传（入站格式 Anthropic == 出站格式 Anthropic）。
    // 透传路径不做工具 schema 上限校验（字节保真是透传的设计承诺），
    // 兜底 = axum 体限 + auth + 限流（与 openai.rs 同口径）。
    let use_passthrough = decision.vendor.type_ == "anthropic";

    if use_passthrough {
        // 零拷贝路径：直接透传请求体
        if stream {
            // recId=2（二期）：cover_all_paths 开启时「首字节前换路」——
            // Err（均发生在响应头前，manager check_stream_status 早退）排除
            // 重派；Ok（流已建立）即交出 body，流中错误仍以 SSE error 事件
            // 终止不再换路。关闭时单发直连，行为与历史逐位一致。
            let (stream_response, decision) =
                dispatch_with_fallback(&state, &ctx, model, &body, decision, true, |d| {
                    let vendor_manager = state.vendor_manager.clone();
                    let body = body.clone();
                    async move {
                        vendor_manager
                            .execute_passthrough_stream_request(&d.vendor.id, body)
                            .await
                    }
                })
                .await?;

            // recId=11：派发成功即回写亲和绑定
            // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
            crate::api::bind_session_affinity(
                &state,
                &ctx.api_key_hash,
                ctx.session_key.as_deref(),
                &decision.vendor.id,
            )
            .await;

            // recId=3 阶段一：透传流过分帧器逐帧原样转发 —— 此前原始字节
            // 直转（连重帧都没有），上游网络中断会伪装成正常流结束
            let body_stream = axum::body::Body::from_stream(
                crate::api::sse::reframe_passthrough_stream(stream_response),
            );

            log_stream_dispatch(&state, &ctx, &decision, &body).await;

            return Ok((STREAM_HEADERS, body_stream).into_response());
        }

        // recId=12：非流式透传的精确响应缓存（命中直接返回 HIT）
        let cache_outcome = cache_probe(&state, &headers, &decision, &body).await;
        if let Some(hit) = cache_hit_response(&state, &ctx, &decision, &body, &cache_outcome).await
        {
            return Ok(hit);
        }

        // 普通请求 - 零拷贝透传
        // recId=2（一期）：cover_all_paths 开启时包换路 fallback 循环
        //（resilience feature）——主 vendor 耗尽/429/5xx 时排除后重路由，
        // actual_vendor 记最终成功者；关闭时单发直连，行为与历史逐位一致
        let (response, decision) =
            dispatch_with_fallback(&state, &ctx, model, &body, decision, true, |d| {
                let vendor_manager = state.vendor_manager.clone();
                let body = body.clone();
                async move {
                    vendor_manager
                        .execute_passthrough_request(&d.vendor.id, body)
                        .await
                }
            })
            .await?;

        // recId=11：执行成功回写亲和绑定（换路成功时 decision 已是最终
        // 成功者——失败迁移绑定语义）；recId=12：未命中则回写响应缓存
        // recId=10(3)：迁移重绑定无条件 set（不得收敛到已失败者）
        crate::api::rebind_session_affinity(
            &state,
            &ctx.api_key_hash,
            ctx.session_key.as_deref(),
            &decision.vendor.id,
        )
        .await;
        persist_cache_miss(&state, &cache_outcome, &decision, &response).await;

        log_success_dispatch(&state, &ctx, &decision, &body, &response).await;

        // recId=12：缓存参与（Miss 回写完成）→ 响应带 X-Cache: MISS
        return Ok(json_response_with_cache_flag(response, &cache_outcome));
    }

    // 转换路径：Anthropic → Unified → 目标厂商
    let unified_request = state.anthropic_converter.to_unified(body.clone())?;

    if stream {
        // recId=3 阶段二：manager 返回分帧 + 反向解析后的 IR chunk 流
        // recId=2（二期）：cover_all_paths 开启时「首字节前换路」（转换路径
        // UnifiedRequest 任意 vendor type 均合法，不按协议过滤）；关闭时
        // 单发直连，行为与历史逐位一致
        let (chunk_stream, decision) =
            dispatch_with_fallback(&state, &ctx, model, &body, decision, false, |d| {
                let vendor_manager = state.vendor_manager.clone();
                let unified_request = unified_request.clone();
                async move {
                    vendor_manager
                        .execute_stream_request(&d.vendor.id, unified_request)
                        .await
                }
            })
            .await?;

        // recId=11：派发成功即回写亲和绑定
        // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
        crate::api::bind_session_affinity(
            &state,
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
        // recId=14②：错误发带语义的 SSE error 事件后提前终止（不补
        // message_stop，客户端可分辨成败）
        let body_stream = anthropic_sse_stream(chunk_stream);

        log_stream_dispatch(&state, &ctx, &decision, &body).await;

        return Ok((STREAM_HEADERS, body_stream).into_response());
    }

    // 通过 VendorManager 执行请求
    // recId=12：非流式转换路径的精确响应缓存（命中直接返回 HIT）
    let cache_outcome = cache_probe(&state, &headers, &decision, &body).await;
    if let Some(hit) = cache_hit_response(&state, &ctx, &decision, &body, &cache_outcome).await {
        return Ok(hit);
    }

    // recId=7⑤：单发调用包换路 fallback 循环（resilience feature）——
    // 主 vendor 耗尽/429/5xx 时排除后重路由，actual_vendor 记最终成功者
    let (unified_response, decision) =
        dispatch_unified_request(&state, &ctx, model, &body, decision, unified_request).await?;

    // 将统一响应转换回 Anthropic 格式
    let response = state
        .anthropic_converter
        .response_to_external(unified_response)?;

    // recId=11：执行成功回写亲和绑定（resilience 换路成功时 decision 已是
    // 最终成功者——失败迁移绑定语义）；recId=12：缓存 Miss 回写
    // recId=10(3)：迁移重绑定无条件 set（不得收敛到已失败者）
    crate::api::rebind_session_affinity(
        &state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    persist_cache_miss(&state, &cache_outcome, &decision, &response).await;

    log_success_dispatch(&state, &ctx, &decision, &body, &response).await;

    // recId=12：缓存参与 → 响应带 X-Cache 头（此处必为 MISS，HIT 已提前返回）
    Ok(json_response_with_cache_flag(response, &cache_outcome))
}

/// recId=15/17/19：路由解析 — Anthropic 入站保持既有无 hints 直连路由
/// 语义（不接 route_with_hints，仅接入会话亲和与请求级权重）；LinUCB
/// 请求特征仅 selection="linucb"（热读）且 bandit 接线时计算，默认
/// weighted 档 None、零开销；x-deadline-ms 头消费与否由
/// [routing].lps_deadline_gate 热读门控（默认关闭不消费）。
async fn resolve_route(
    state: &Arc<ApiState>,
    model: &str,
    headers: &HeaderMap,
    hints: RoutingHints,
    session_key: Option<&str>,
    api_key_hash: &str,
    body: &Value,
) -> Result<RoutingDecision, NexusFlowError> {
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
    // recId=17：x-deadline-ms 头（语义同 openai chat；门控在 Router 侧）
    let lps_deadline_ms = headers
        .get("x-deadline-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
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
        )
        .await?;
    Ok(decision)
}

/// recId=9④ + recId=11：派发失败收尾单点 — 原实现 8 处（4 路径 ×
/// resilience 开关）重复的「亲和解除 + 失败落库」收敛于此；换路耗尽与
/// 单发失败同语义（recId=11：最终失败解除亲和绑定，避免旧键指向已失败
/// vendor）。错误经调用方包装后透传。
async fn on_dispatch_failure(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    body: &Value,
    decision: &RoutingDecision,
    error: NexusFlowError,
) -> NexusFlowError {
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

/// cover_all_paths 三态派发（recId=2 一期/二期）：resilience 开启且开关
/// 热读生效时走换路 fallback 循环（`decision.clone()` 供失败落库引用初始
/// 决策），否则单发直连——`direct` 闭包即直连调用本体（与 fallback 循环
/// 的派发闭包同构），行为两分支均与历史逐位一致。失败收尾经
/// [`on_dispatch_failure`] 单点执行。
///
/// 泛型约定同 [`crate::api::execute_dispatch_with_fallback`]：闭包持有
/// decision 所有权并返回不借用环境的 future（owned captures）。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn dispatch_with_fallback<T, F, Fut>(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    same_protocol_only: bool,
    direct: F,
) -> Result<(T, RoutingDecision), NexusFlowError>
where
    F: Fn(RoutingDecision) -> Fut,
    Fut: std::future::Future<Output = Result<T, GateError>>,
{
    #[cfg(feature = "resilience")]
    if state.config.read().resilience.cover_all_paths {
        return match crate::api::execute_dispatch_with_fallback(
            state,
            model,
            decision.clone(),
            same_protocol_only,
            direct,
        )
        .await
        {
            Ok(r) => Ok(r),
            Err(e) => {
                Err(on_dispatch_failure(state, ctx, body, &decision, NexusFlowError::from(e)).await)
            }
        };
    }
    match direct(decision.clone()).await {
        Ok(r) => Ok((r, decision)),
        Err(e) => {
            Err(on_dispatch_failure(state, ctx, body, &decision, NexusFlowError::from(e)).await)
        }
    }
}

/// recId=7⑤：非流式转换路径派发 — resilience 开启时恒走换路 fallback
/// 循环（execute_request_with_fallback：Throttled 原因追踪 + Retry-After
/// 提示语义，与透传/流式的 cover_all_paths 门控路径不同源，不得合并）；
/// 关闭时单发直连。失败收尾经 [`on_dispatch_failure`] 单点执行。
#[cfg_attr(not(feature = "resilience"), allow(unused_variables))]
async fn dispatch_unified_request(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    request: crate::protocol::UnifiedRequest,
) -> Result<(crate::protocol::UnifiedResponse, RoutingDecision), NexusFlowError> {
    #[cfg(feature = "resilience")]
    {
        return match crate::api::execute_request_with_fallback(
            state,
            model,
            decision.clone(),
            request,
        )
        .await
        {
            Ok(r) => Ok(r),
            Err(e) => {
                Err(on_dispatch_failure(state, ctx, body, &decision, NexusFlowError::from(e)).await)
            }
        };
    }
    #[cfg(not(feature = "resilience"))]
    match state
        .vendor_manager
        .execute_request(&decision.vendor.id, request)
        .await
    {
        Ok(r) => Ok((r, decision)),
        Err(e) => {
            Err(on_dispatch_failure(state, ctx, body, &decision, NexusFlowError::from(e)).await)
        }
    }
}

/// recId=12：缓存探测 — [cache] 未接线 → None（不参与）；命中/未命中/
/// 关闭/不可缓存语义由 [`crate::api::response_cache::ResponseCache::check`]
/// 判定。
async fn cache_probe(
    state: &Arc<ApiState>,
    headers: &HeaderMap,
    decision: &RoutingDecision,
    body: &Value,
) -> Option<CacheOutcome> {
    match state.response_cache.as_ref() {
        Some(cache) => Some(
            cache
                .check(headers, &decision.vendor.id, &decision.model.model_id, body)
                .await,
        ),
        None => None,
    }
}

/// recId=12：HIT 早退 — 命中即记 200 日志并返回 `X-Cache: HIT` 响应；
/// 未命中/不参与返回 None（调用方继续派发）。
async fn cache_hit_response(
    state: &Arc<ApiState>,
    ctx: &AnthropicDispatchCtx,
    decision: &RoutingDecision,
    body: &Value,
    cache_outcome: &Option<CacheOutcome>,
) -> Option<axum::response::Response> {
    let cached = match cache_outcome.as_ref()? {
        CacheOutcome::Hit(cached) => cached,
        _ => return None,
    };
    log_success_dispatch(state, ctx, decision, body, cached).await;
    Some(
        (
            [("Content-Type", "application/json"), ("X-Cache", "HIT")],
            Json(cached.clone()),
        )
            .into_response(),
    )
}

/// recId=12：Miss 回写 — 以最终 vendor/model 组装键（cover_all_paths 换路
/// 后已是最终 vendor；非 resilience 即初始且唯一候选），键维度与响应体
/// 一致（评审修复 major#2）。
async fn persist_cache_miss(
    state: &Arc<ApiState>,
    cache_outcome: &Option<CacheOutcome>,
    decision: &RoutingDecision,
    response: &Value,
) {
    if let Some(CacheOutcome::Miss(parts)) = cache_outcome {
        if let Some(cache) = state.response_cache.as_ref() {
            cache
                .store(
                    parts,
                    &decision.vendor.id,
                    &decision.model.model_id,
                    response,
                )
                .await;
        }
    }
}

/// recId=12：缓存参与 → 响应带 `X-Cache: MISS` 头（HIT 已早退，此处必为
/// MISS 或不参与）；不参与则裸 JSON 头。
fn json_response_with_cache_flag(
    response: Value,
    cache_outcome: &Option<CacheOutcome>,
) -> axum::response::Response {
    if cache_outcome
        .as_ref()
        .is_some_and(CacheOutcome::participated)
    {
        (
            [("Content-Type", "application/json"), ("X-Cache", "MISS")],
            Json(response),
        )
            .into_response()
    } else {
        ([("Content-Type", "application/json")], Json(response)).into_response()
    }
}

/// recId=3 阶段二 / recId=14②：IR chunk 流 → Anthropic 出站 SSE 事件流。
/// AnthropicStreamEncoder 状态机补齐 message_start/content_block_start/
/// …_delta/…_stop/message_stop 生命周期，每事件加 `data:` 前缀（评审
/// blocker：裸 JSON 行不构成 SSE 事件）。错误发带语义的 SSE error 事件后
/// 提前终止（不补 message_stop，客户端可分辨成败）。
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
                    yield Ok(crate::api::sse::encode_sse_error(&e.to_string()).into());
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

/// 流式派发成功日志（recId=4⑤：metrics 已在 manager 层记账，handler 不
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
}

/// recId=9③④⑥：异步日志 push — 构造 RequestLog 后 `logger.log()` push
/// 即返回（try_send 背压，绝不阻塞响应路径）；token 从**响应 usage** 取
/// （修复旧实现填请求 max_tokens 的语义错误）；status_code 从实际结果取；
/// 大 payload（>64KB）截断。logger 未接线（None）时跳过落库。
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
    // recId=15：LinUCB 奖励回传 — 与落库记账同点（本函数收敛成功/失败
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

    // recId=9④（评审修正）：tokens 从响应 usage 取——**双格式**。本端点的
    // 响应既可能是 Anthropic 格式（透传路径的原始上游 JSON；转换路径经
    // anthropic_converter::response_to_external 产出 {"input_tokens",
    // "output_tokens"}，见 anthropic_converter.rs:215-218），此前只读
    // OpenAI 键名导致三个 token 字段恒 NULL、预聚合器永不记账。
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

    // recId=9⑤：usage 进预聚合器（记录即返回，flush 由后台 worker 承担）
    if let (Some(pa), Some(p), Some(c), Some(t)) = (
        state.pre_aggregator.as_ref(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
    ) {
        if let Err(e) = pa
            .record_raw(
                &decision.vendor.id,
                &decision.model.model_id,
                p.clamp(0, i32::MAX) as u32,
                c.clamp(0, i32::MAX) as u32,
                t.clamp(0, i32::MAX) as u32,
                0.0,
            )
            .await
        {
            // 记账失败不失败请求（recId=9⑤ 语义保持；错误显性化）
            tracing::warn!(
                "{}",
                t_args("api-usage-record-rejected", &[("error", e.to_string())])
            );
        }
    }

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

    // recId=9②：logger 未接线 = 降级为不落日志（不回退同步直写）
    if let Some(logger) = state.logger.as_ref() {
        if let Err(e) = logger.log(log).await {
            tracing::warn!(
                "{}",
                t_args("api-log-push-rejected", &[("error", e.to_string())])
            );
        }
    }
}

/// recId=9④：失败路径落库 — 在 `?` 早退前补记（status_code/error_message
/// 真实值），消除"失败请求不留痕"的盲区。
#[allow(clippy::too_many_arguments)]
async fn log_failure(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    duration_ms: i64,
    error: &NexusFlowError,
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
