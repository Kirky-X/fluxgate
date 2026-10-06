// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::protocol::embedding_request_to_openai;
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
/// 落库路径共用（收敛原 8 段重复收尾的长参数列表）。
struct ChatDispatchCtx {
    request_id: String,
    start_time: std::time::Instant,
    session_key: Option<String>,
    api_key_hash: String,
    // recId=15（评审修复 major）：奖励臂键（None = X-Vendor 直连跳过奖励）
    linucb_reward_key: Option<String>,
}

#[axum::debug_handler]
pub async fn chat_completions(
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

    // 提取路由提示与会话亲和键（recId=11：X-Session-Id 优先，messages
    // 前缀哈希兜底）
    let hints = RoutingHints::from_headers(&headers);
    let session_key = hints.effective_session_key(&body);
    // recId=10(2)：亲和键租户维度（与响应缓存 cache_api_key_hash 同源，
    // 从已验证 key 派生；affinity_tenant_scoped 关闭时不参与键构造）
    let api_key_hash = crate::api::response_cache::cache_api_key_hash(&headers);

    // 路由决策（支持 X-Vendor 和 X-Model 请求头；无 hints 时走亲和路由）
    let (decision, linucb_reward_key) = resolve_route(
        &state,
        model,
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
    };

    // 检查是否可以使用零拷贝透传（入站格式 OpenAI == 出站格式 OpenAI）。
    // 透传路径不做工具 schema 上限校验（字节保真是透传的设计承诺，
    // converter 入站校验不适用），兜底 = axum 体限 + auth + 限流。
    let use_passthrough = decision.vendor.type_ == "openai";

    if use_passthrough {
        if stream {
            return chat_passthrough_stream(&state, &ctx, model, &body, decision).await;
        }
        return chat_passthrough_json(&state, &ctx, &headers, model, &body, decision).await;
    }

    // 转换路径：OpenAI → Unified → 目标厂商
    let unified_request = state.openai_converter.to_unified(body.clone())?;

    if stream {
        return chat_convert_stream(&state, &ctx, model, &body, decision, unified_request).await;
    }
    chat_convert_json(
        &state,
        &ctx,
        &headers,
        model,
        &body,
        decision,
        unified_request,
    )
    .await
}

/// 透传流式路径（recId=2 二期/3 阶段一/11/10(3)）：派发 → 首绑定亲和 →
/// 分帧转发 → 流式日志 → SSE 响应。
async fn chat_passthrough_stream(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, NexusFlowError> {
    // recId=2（二期）：cover_all_paths 开启时「首字节前换路」——
    // Err（均发生在响应头前，manager check_stream_status 早退）排除
    // 重派；Ok（流已建立）即交出 body，流中错误仍以 SSE error 事件
    // 终止不再换路。关闭时单发直连，行为与历史逐位一致。
    let (stream_response, decision) =
        dispatch_with_fallback(state, ctx, model, body, decision, true, |d| {
            let vendor_manager = state.vendor_manager.clone();
            let body = body.clone();
            async move {
                vendor_manager
                    .execute_passthrough_stream_request(&d.vendor.id, body)
                    .await
            }
        })
        .await?;

    // recId=11：派发成功即回写亲和绑定（流后续错误由 metrics 记账）
    // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
    crate::api::bind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // recId=3 阶段一：透传流过分帧器逐帧原样转发（帧边界与上游一致），
    // 替换旧的整块 bytes 直转 —— 后者无法检测上游中流 error 事件，
    // 且错误中断会伪装成正常流结束
    let body_stream =
        axum::body::Body::from_stream(crate::api::sse::reframe_passthrough_stream(stream_response));

    log_stream_dispatch(state, ctx, &decision, body).await;

    Ok((STREAM_HEADERS, body_stream).into_response())
}

/// 透传非流式路径（recId=2 一期/12/11/10(3)）：缓存探测 → 派发 → 迁移
/// 重绑定 → Miss 回写 → 成功日志 → X-Cache 响应。
async fn chat_passthrough_json(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
) -> Result<axum::response::Response, NexusFlowError> {
    // recId=12：非流式透传的精确响应缓存（命中直接返回 HIT）
    let cache_outcome = cache_probe(state, headers, &decision, body).await;
    if let Some(hit) = cache_hit_response(state, ctx, &decision, body, &cache_outcome).await {
        return Ok(hit);
    }

    // 普通请求 - 零拷贝透传
    // recId=2（一期）：cover_all_paths 开启时包换路 fallback 循环
    //（resilience feature）——主 vendor 耗尽/429/5xx 时排除后重路由，
    // actual_vendor 记最终成功者；关闭时单发直连，行为与历史逐位一致
    let (response, decision) =
        dispatch_with_fallback(state, ctx, model, body, decision, true, |d| {
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
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    persist_cache_miss(state, &cache_outcome, &decision, &response).await;

    log_success_dispatch(state, ctx, &decision, body, &response).await;

    // recId=12：缓存参与（Miss 回写完成）→ 响应带 X-Cache: MISS
    Ok(json_response_with_cache_flag(response, &cache_outcome))
}

/// 转换流式路径（recId=2 二期/3 阶段二/14②/11/10(3)）：派发 → 首绑定
/// 亲和 → IR→OpenAI 出站事件流 → 流式日志 → SSE 响应。
async fn chat_convert_stream(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, NexusFlowError> {
    // recId=3 阶段二：manager 返回分帧 + 反向解析后的 IR chunk 流
    // recId=2（二期）：cover_all_paths 开启时「首字节前换路」（转换路径
    // UnifiedRequest 任意 vendor type 均合法，不按协议过滤）；关闭时
    // 单发直连，行为与历史逐位一致
    let (chunk_stream, decision) =
        dispatch_with_fallback(state, ctx, model, body, decision, false, |d| {
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
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;

    // IR → OpenAI 出站事件（存量 stream_chunk_to_external）。
    // recId=14②：错误发带语义的 SSE error 事件后提前终止，不再补
    // [DONE] —— 客户端与重试逻辑可分辨成败（旧实现错误后仍以 [DONE]
    // 收尾伪装正常结束）
    let converter = state.openai_converter.clone();
    let body_stream = openai_sse_stream(converter, chunk_stream);

    log_stream_dispatch(state, ctx, &decision, body).await;

    Ok((STREAM_HEADERS, body_stream).into_response())
}

/// 转换非流式路径（recId=7⑤/12/11/10(3)）：缓存探测 → 派发（resilience
/// 恒走换路循环）→ 统一响应回转 → 迁移重绑定 → Miss 回写 → 成功日志 →
/// X-Cache 响应。
async fn chat_convert_json(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    headers: &HeaderMap,
    model: &str,
    body: &Value,
    decision: RoutingDecision,
    unified_request: crate::protocol::UnifiedRequest,
) -> Result<axum::response::Response, NexusFlowError> {
    // 普通请求 - 通过 UnifiedRequest
    // recId=12：非流式转换路径的精确响应缓存（命中直接返回 HIT）
    let cache_outcome = cache_probe(state, headers, &decision, body).await;
    if let Some(hit) = cache_hit_response(state, ctx, &decision, body, &cache_outcome).await {
        return Ok(hit);
    }

    // recId=7⑤：单发调用包换路 fallback 循环（resilience feature）——
    // 主 vendor 耗尽/429/5xx 时排除后重路由，actual_vendor 记最终成功者
    let (unified_response, decision) =
        dispatch_unified_request(state, ctx, model, body, decision, unified_request).await?;

    // 将统一响应转换回 OpenAI 格式
    let response = state
        .openai_converter
        .response_to_external(unified_response)?;

    // recId=11：执行成功回写亲和绑定（resilience 换路成功时 decision 已是
    // 最终成功者——失败迁移绑定语义）；recId=12：缓存 Miss 回写
    // recId=10(3)：迁移重绑定无条件 set（不得收敛到已失败者）
    crate::api::rebind_session_affinity(
        state,
        &ctx.api_key_hash,
        ctx.session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    persist_cache_miss(state, &cache_outcome, &decision, &response).await;

    log_success_dispatch(state, ctx, &decision, body, &response).await;

    // recId=12：缓存参与 → 响应带 X-Cache 头（此处必为 MISS，HIT 已提前返回）
    Ok(json_response_with_cache_flag(response, &cache_outcome))
}

/// recId=15/17/19：路由解析 — X-Vendor/X-Model hints 直连 vs 亲和 weighted
/// 路由二选一；LinUCB 特征仅 selection="linucb"（热读）且 bandit 接线时
/// 计算并参与选择，默认 weighted 档 None、零开销、行为与历史逐位一致；
/// x-deadline-ms 头消费与否由 [routing].lps_deadline_gate 热读门控
///（默认关闭不消费）。
///
/// 返回 (路由决策, LinUCB 奖励臂键)。臂键与选择侧同源归一（评审修复 major）：
/// - X-Vendor 直连（try_vendor_route 单候选）臂未被咨询 → None 跳过奖励
///   （「未选择不更新」，避免伪样本）；
/// - X-Model → 该模型的归一键（route_with_hints 以其路由）；
/// - 其余（含 auto → "__global__"）→ body.model 归一键。
async fn resolve_route(
    state: &Arc<ApiState>,
    model: &str,
    headers: &HeaderMap,
    hints: RoutingHints,
    session_key: Option<&str>,
    api_key_hash: &str,
    body: &Value,
) -> Result<(RoutingDecision, Option<String>), NexusFlowError> {
    // recId=19(a)：X-Weight 请求级权重随 hints 传入——是否生效由
    // [routing].allow_request_weight 门控（Router 侧热读判定，关闭时忽略）
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
    // recId=17：x-deadline-ms 头（可选，非法值忽略）
    let lps_deadline_ms = headers
        .get("x-deadline-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
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
            )
            .await?
    };
    Ok((decision, linucb_reward_key))
}

/// recId=9④ + recId=11：派发失败收尾单点 — 原实现 8 处（4 路径 ×
/// resilience 开关）重复的「亲和解除 + 失败落库」收敛于此；换路耗尽与
/// 单发失败同语义（recId=11：最终失败解除亲和绑定，避免旧键指向已失败
/// vendor）。错误经调用方包装后透传。
async fn on_dispatch_failure(
    state: &Arc<ApiState>,
    ctx: &ChatDispatchCtx,
    body: &Value,
    decision: &RoutingDecision,
    error: NexusFlowError,
) -> NexusFlowError {
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
    ctx: &ChatDispatchCtx,
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
    ctx: &ChatDispatchCtx,
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
    ctx: &ChatDispatchCtx,
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

/// recId=3 阶段二 / recId=14②：IR chunk 流 → OpenAI 出站 SSE 事件流。
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
        let mut errored = false;
        while let Some(item) = futures::StreamExt::next(&mut chunk_stream).await {
            match item {
                Ok(chunk) => match converter.stream_chunk_to_external(chunk) {
                    Ok(event) => {
                        yield Ok::<bytes::Bytes, std::io::Error>(
                            crate::api::sse::encode_sse_event(&event.to_string(), None)
                                .into(),
                        );
                    }
                    Err(e) => {
                        errored = true;
                        yield Ok(crate::api::sse::encode_sse_error(&e.to_string()).into());
                        break;
                    }
                },
                Err(e) => {
                    errored = true;
                    yield Ok(crate::api::sse::encode_sse_error(&e.to_string()).into());
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

/// 流式派发成功日志（recId=4⑤：metrics 已在 manager 层记账，handler 不
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
}

/// recId=9③④⑥：异步日志 push — 构造 RequestLog 后 `logger.log()` push
/// 即返回（try_send 背压，绝不阻塞响应路径）；token 从**响应 usage** 取
/// （修复旧实现填请求 max_tokens 的语义错误）；status_code 从实际结果取；
/// 大 payload（>64KB）截断。
///
/// `response: None` = 失败路径（无响应体），status_code/error_message 由
/// 调用方传真实值（见 [`crate::api::status_code_for_error`]）。
/// logger 未接线（None）时跳过落库 —— 明确的降级语义，不做同步直写
///（那会重新把 DB 延迟拉回响应关键路径）。
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
    // recId=15（评审修复 major）：奖励臂键（None = 直连路径跳过奖励）
    linucb_arm_key: Option<&str>,
) {
    // recId=15：LinUCB 奖励回传 — 与落库记账同点（本函数收敛成功/失败
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

    // recId=9④：prompt/completion/total tokens 从响应 usage 取
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

    // recId=9②：logger 未接线 = 降级为不落日志（不回退同步直写）
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

/// recId=9④：失败路径落库 — 在 `?` 早退前补记（status_code/error_message
/// 真实值），消除"失败请求不留痕"的盲区。
#[allow(clippy::too_many_arguments)] // 落库上下文穿线（log_chat_request 同款先例）
async fn log_chat_failure(
    state: &Arc<ApiState>,
    request_id: &str,
    input_format: &str,
    decision: &RoutingDecision,
    request: &Value,
    duration_ms: i64,
    error: &NexusFlowError,
    // recId=15（评审修复 major）：奖励臂键（None = 直连路径跳过奖励）
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

/// recId=15：embeddings 回归供应商治理 — 删除每请求 `reqwest::Client::new()`
/// 与硬编码 api.openai.com 兜底，改走 VendorManager 治理路径
/// `execute_passthrough_request_at(vendor, body, "embeddings")`：共享连接池、
/// RPS/并发限流、advanced limiter、token 记账、metrics.record_request 全部
/// 生效；上游非 2xx → VendorUnavailable（error.rs 固定映射 503，与 recId=14
/// 错误传播同口径）。日志同 chat 走异步管道（recId=9③）。
#[axum::debug_handler]
pub async fn embeddings(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, NexusFlowError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    // 路由决策 - embedding 使用特殊路由
    let model = body.get("model").and_then(|v| v.as_str()).ok_or_else(|| {
        NexusFlowError::from(GateError::Validation {
            field: "model".into(),
        })
    })?;

    let decision = match state.router.route(model).await {
        Ok(d) => d,
        Err(e) => return Err(e.into()),
    };

    // 只支持 OpenAI vendor
    if decision.vendor.type_ != "openai" {
        return Err(NexusFlowError::from(GateError::Protocol {
            source: t("api-embeddings-openai-only"),
            inner: None,
        }));
    }

    // 转换为 OpenAI 格式
    let openai_body = embedding_request_to_openai(body.clone())?;

    // 治理路径执行（recId=15①）
    let response_json = match state
        .vendor_manager
        .execute_passthrough_request_at(&decision.vendor.id, openai_body, "embeddings")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let e = NexusFlowError::from(e);
            // 失败落库（recId=9④）：status_code/error_message 真实值
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
            )
            .await;
            return Err(e);
        }
    };

    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    let input_str = body
        .get("input")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();
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
    )
    .await;

    Ok(Json(response_json))
}

/// recId=15①/9③：embeddings 的异步日志 push（同 log_chat_request 的背压
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

    // recId=9⑤：embeddings usage 同样进预聚合器
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
