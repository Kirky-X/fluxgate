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

#[axum::debug_handler]
pub async fn messages(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, NexusFlowError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(NexusFlowError::from(GateError::Validation {
            field: "model".into(),
        }))?;

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
    // recId=15：LinUCB 请求特征 — 语义同 openai chat（默认 weighted 档
    // None、零开销）。
    let linucb_x = if state.bandit.is_some()
        && state
            .config
            .read()
            .routing
            .selection
            .trim()
            .eq_ignore_ascii_case("linucb")
    {
        Some(crate::router::linucb_features_from_body(&body))
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
            session_key.as_deref(),
            Some(api_key_hash.as_str()),
            &[],
            hints.preferred_weight,
            linucb_x.as_ref().map(|a| a.as_slice()),
            lps_deadline_ms,
        )
        .await?;

    // 检查是否可以使用零拷贝透传（入站格式 Anthropic == 出站格式 Anthropic）
    let use_passthrough = decision.vendor.type_ == "anthropic";

    if use_passthrough {
        // 零拷贝路径：直接透传请求体
        if stream {
            // recId=9④：失败在 ? 早退前补记（下同，四处执行点同款）
            // recId=2（二期）：cover_all_paths 开启时「首字节前换路」——
            // Err（均发生在响应头前，manager check_stream_status 早退）排除
            // 重派；Ok（流已建立）即交出 body，流中错误仍以 SSE error 事件
            // 终止不再换路。关闭时单发直连，行为与历史逐位一致。
            #[cfg(feature = "resilience")]
            let (stream_response, decision) = if state.config.read().resilience.cover_all_paths {
                // decision clone 传参：Err 分支的失败落库需引用初始路由决策
                match crate::api::execute_dispatch_with_fallback(
                    &state,
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
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        let e = NexusFlowError::from(e);
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        // recId=11：最终失败解除亲和绑定
                        crate::api::clear_session_affinity(
                            &state,
                            &api_key_hash,
                            session_key.as_deref(),
                        )
                        .await;
                        log_failure(
                            &state,
                            &request_id,
                            "anthropic",
                            &decision,
                            &body,
                            duration_ms,
                            &e,
                        )
                        .await;
                        return Err(e);
                    }
                }
            } else {
                match state
                    .vendor_manager
                    .execute_passthrough_stream_request(&decision.vendor.id, body.clone())
                    .await
                {
                    Ok(r) => (r, decision),
                    Err(e) => {
                        let e = NexusFlowError::from(e);
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        // recId=11：最终失败解除亲和绑定
                        crate::api::clear_session_affinity(
                            &state,
                            &api_key_hash,
                            session_key.as_deref(),
                        )
                        .await;
                        log_failure(
                            &state,
                            &request_id,
                            "anthropic",
                            &decision,
                            &body,
                            duration_ms,
                            &e,
                        )
                        .await;
                        return Err(e);
                    }
                }
            };
            #[cfg(not(feature = "resilience"))]
            let stream_response = match state
                .vendor_manager
                .execute_passthrough_stream_request(&decision.vendor.id, body.clone())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let e = NexusFlowError::from(e);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_failure(
                        &state,
                        &request_id,
                        "anthropic",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                    )
                    .await;
                    return Err(e);
                }
            };

            // recId=11：派发成功即回写亲和绑定
            // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
            crate::api::bind_session_affinity(
                &state,
                &api_key_hash,
                session_key.as_deref(),
                &decision.vendor.id,
            )
            .await;

            // recId=3 阶段一：透传流过分帧器逐帧原样转发 —— 此前原始字节
            // 直转（连重帧都没有），上游网络中断会伪装成正常流结束
            let body_stream = axum::body::Body::from_stream(
                crate::api::sse::reframe_passthrough_stream(stream_response),
            );

            let duration_ms = start_time.elapsed().as_millis() as i64;
            // recId=4⑤：流式「派发处计」— metrics 已在 manager 层记账，
            // handler 不重复计；日志按派发成功记 200
            log_request(
                &state,
                &request_id,
                "anthropic",
                &decision,
                &body,
                Some(&json!({"stream": true})),
                duration_ms,
                200,
                None,
            )
            .await;

            return Ok((
                [
                    ("Content-Type", "text/event-stream"),
                    ("Cache-Control", "no-cache"),
                    ("Connection", "keep-alive"),
                ],
                body_stream,
            )
                .into_response());
        }

        // recId=12：非流式透传的精确响应缓存（命中直接返回 HIT）
        let cache_outcome = match state.response_cache.as_ref() {
            Some(cache) => Some(
                cache
                    .check(
                        &headers,
                        &decision.vendor.id,
                        &decision.model.model_id,
                        &body,
                    )
                    .await,
            ),
            None => None,
        };
        if let Some(crate::api::response_cache::CacheOutcome::Hit(cached)) = &cache_outcome {
            let duration_ms = start_time.elapsed().as_millis() as i64;
            log_request(
                &state,
                &request_id,
                "anthropic",
                &decision,
                &body,
                Some(cached),
                duration_ms,
                200,
                None,
            )
            .await;
            return Ok((
                [("Content-Type", "application/json"), ("X-Cache", "HIT")],
                Json(cached.clone()),
            )
                .into_response());
        }

        // 普通请求 - 零拷贝透传
        // recId=2（一期）：cover_all_paths 开启时包换路 fallback 循环
        //（resilience feature）——主 vendor 耗尽/429/5xx 时排除后重路由，
        // actual_vendor 记最终成功者；关闭时单发直连，行为与历史逐位一致
        #[cfg(feature = "resilience")]
        let (response, decision) = if state.config.read().resilience.cover_all_paths {
            // decision clone 传参：Err 分支的失败落库（recId=9④）需引用初始路由决策
            match crate::api::execute_dispatch_with_fallback(
                &state,
                model,
                decision.clone(),
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
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    let e = NexusFlowError::from(e);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_failure(
                        &state,
                        &request_id,
                        "anthropic",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                    )
                    .await;
                    return Err(e);
                }
            }
        } else {
            match state
                .vendor_manager
                .execute_passthrough_request(&decision.vendor.id, body.clone())
                .await
            {
                Ok(r) => (r, decision),
                Err(e) => {
                    let e = NexusFlowError::from(e);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_failure(
                        &state,
                        &request_id,
                        "anthropic",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                    )
                    .await;
                    return Err(e);
                }
            }
        };
        #[cfg(not(feature = "resilience"))]
        let response = match state
            .vendor_manager
            .execute_passthrough_request(&decision.vendor.id, body.clone())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let e = NexusFlowError::from(e);
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：最终失败解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref())
                    .await;
                log_failure(
                    &state,
                    &request_id,
                    "anthropic",
                    &decision,
                    &body,
                    duration_ms,
                    &e,
                )
                .await;
                return Err(e);
            }
        };

        // recId=11：执行成功回写亲和绑定（换路成功时 decision 已是最终
        // 成功者——失败迁移绑定语义）；recId=12：缓存 Miss 回写
        // recId=10(3)：迁移重绑定无条件 set（不得收敛到已失败者）
        crate::api::rebind_session_affinity(
            &state,
            &api_key_hash,
            session_key.as_deref(),
            &decision.vendor.id,
        )
        .await;
        if let Some(crate::api::response_cache::CacheOutcome::Miss(parts)) = &cache_outcome {
            if let Some(cache) = state.response_cache.as_ref() {
                // decision 此处即本路径最终成功者（cover_all_paths 换路后
                // 已是最终 vendor；关闭时初始且唯一候选）——键维度与响应体
                // 一致（评审修复 major#2）
                cache
                    .store(
                        parts,
                        &decision.vendor.id,
                        &decision.model.model_id,
                        &response,
                    )
                    .await;
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_request(
            &state,
            &request_id,
            "anthropic",
            &decision,
            &body,
            Some(&response),
            duration_ms,
            200,
            None,
        )
        .await;

        // recId=12：缓存参与（Miss 回写完成）→ 响应带 X-Cache: MISS
        return if cache_outcome
            .as_ref()
            .is_some_and(crate::api::response_cache::CacheOutcome::participated)
        {
            Ok((
                [("Content-Type", "application/json"), ("X-Cache", "MISS")],
                Json(response),
            )
                .into_response())
        } else {
            Ok(([("Content-Type", "application/json")], Json(response)).into_response())
        };
    }

    // 转换路径：Anthropic → Unified → 目标厂商
    let unified_request = state.anthropic_converter.to_unified(body.clone())?;

    if stream {
        // recId=3 阶段二：manager 返回分帧 + 反向解析后的 IR chunk 流
        // recId=2（二期）：cover_all_paths 开启时「首字节前换路」（转换路径
        // UnifiedRequest 任意 vendor type 均合法，不按协议过滤）；关闭时
        // 单发直连，行为与历史逐位一致
        #[cfg(feature = "resilience")]
        let (chunk_stream, decision) = if state.config.read().resilience.cover_all_paths {
            // decision clone 传参：Err 分支的失败落库需引用初始路由决策
            match crate::api::execute_dispatch_with_fallback(
                &state,
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
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    let e = NexusFlowError::from(e);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_failure(
                        &state,
                        &request_id,
                        "anthropic",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                    )
                    .await;
                    return Err(e);
                }
            }
        } else {
            match state
                .vendor_manager
                .execute_stream_request(&decision.vendor.id, unified_request)
                .await
            {
                Ok(s) => (s, decision),
                Err(e) => {
                    let e = NexusFlowError::from(e);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_failure(
                        &state,
                        &request_id,
                        "anthropic",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                    )
                    .await;
                    return Err(e);
                }
            }
        };
        #[cfg(not(feature = "resilience"))]
        let chunk_stream = match state
            .vendor_manager
            .execute_stream_request(&decision.vendor.id, unified_request)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let e = NexusFlowError::from(e);
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：最终失败解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref())
                    .await;
                log_failure(
                    &state,
                    &request_id,
                    "anthropic",
                    &decision,
                    &body,
                    duration_ms,
                    &e,
                )
                .await;
                return Err(e);
            }
        };

        // recId=11：派发成功即回写亲和绑定
        // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
        crate::api::bind_session_affinity(
            &state,
            &api_key_hash,
            session_key.as_deref(),
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
        let mut encoder = crate::protocol::AnthropicStreamEncoder::new();
        let sse_stream = async_stream::stream! {
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

        let body_stream = axum::body::Body::from_stream(sse_stream);

        // 记录流式请求日志 — recId=4⑤：metrics 在 manager 层记账，handler
        // 不重复计；日志按派发成功记 200
        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_request(
            &state,
            &request_id,
            "anthropic",
            &decision,
            &body,
            Some(&json!({"stream": true})),
            duration_ms,
            200,
            None,
        )
        .await;

        return Ok((
            [
                ("Content-Type", "text/event-stream"),
                ("Cache-Control", "no-cache"),
                ("Connection", "keep-alive"),
            ],
            body_stream,
        )
            .into_response());
    }

    // 通过 VendorManager 执行请求
    // recId=12：非流式转换路径的精确响应缓存（命中直接返回 HIT）
    let cache_outcome = match state.response_cache.as_ref() {
        Some(cache) => Some(
            cache
                .check(
                    &headers,
                    &decision.vendor.id,
                    &decision.model.model_id,
                    &body,
                )
                .await,
        ),
        None => None,
    };
    if let Some(crate::api::response_cache::CacheOutcome::Hit(cached)) = &cache_outcome {
        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_request(
            &state,
            &request_id,
            "anthropic",
            &decision,
            &body,
            Some(cached),
            duration_ms,
            200,
            None,
        )
        .await;
        return Ok((
            [("Content-Type", "application/json"), ("X-Cache", "HIT")],
            Json(cached.clone()),
        )
            .into_response());
    }

    // recId=7⑤：单发调用包换路 fallback 循环（resilience feature）——
    // 主 vendor 耗尽/429/5xx 时排除后重路由，actual_vendor 记最终成功者
    #[cfg(feature = "resilience")]
    let (unified_response, decision) =
        // decision clone 传参：Err 分支的失败落库（recId=9④）需引用初始路由决策
        match crate::api::execute_request_with_fallback(&state, model, decision.clone(), unified_request)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let e = NexusFlowError::from(e);
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：换路 fallback 耗尽（最终失败）解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref()).await;
                log_failure(&state, &request_id, "anthropic", &decision, &body, duration_ms, &e)
                    .await;
                return Err(e);
            }
        };
    #[cfg(not(feature = "resilience"))]
    let unified_response = match state
        .vendor_manager
        .execute_request(&decision.vendor.id, unified_request)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let e = NexusFlowError::from(e);
            let duration_ms = start_time.elapsed().as_millis() as i64;
            // recId=11：最终失败解除亲和绑定
            crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref()).await;
            log_failure(
                &state,
                &request_id,
                "anthropic",
                &decision,
                &body,
                duration_ms,
                &e,
            )
            .await;
            return Err(e);
        }
    };

    // 将统一响应转换回 Anthropic 格式
    let response = state
        .anthropic_converter
        .response_to_external(unified_response)?;

    // recId=11：执行成功回写亲和绑定（resilience 换路成功时 decision 已是
    // 最终成功者——失败迁移绑定语义）；recId=12：缓存 Miss 回写
    // recId=10(3)：迁移重绑定无条件 set（不得收敛到已失败者）
    crate::api::rebind_session_affinity(
        &state,
        &api_key_hash,
        session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    if let Some(crate::api::response_cache::CacheOutcome::Miss(parts)) = &cache_outcome {
        if let Some(cache) = state.response_cache.as_ref() {
            // recId=11 注释处 decision 已是 resilience 换路后的最终成功者
            //（非 resilience 编译分支即初始且唯一候选）——以最终 vendor/model
            // 组装键，键维度与响应体一致（评审修复 major#2）
            cache
                .store(
                    parts,
                    &decision.vendor.id,
                    &decision.model.model_id,
                    &response,
                )
                .await;
        }
    }

    // 记录日志
    let duration_ms = start_time.elapsed().as_millis() as i64;
    log_request(
        &state,
        &request_id,
        "anthropic",
        &decision,
        &body,
        Some(&response),
        duration_ms,
        200,
        None,
    )
    .await;

    // recId=12：缓存参与 → 响应带 X-Cache 头（此处必为 MISS，HIT 已提前返回）
    if cache_outcome
        .as_ref()
        .is_some_and(crate::api::response_cache::CacheOutcome::participated)
    {
        Ok((
            [("Content-Type", "application/json"), ("X-Cache", "MISS")],
            Json(response),
        )
            .into_response())
    } else {
        Ok(([("Content-Type", "application/json")], Json(response)).into_response())
    }
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
        pa.record_raw(
            &decision.vendor.id,
            &decision.model.model_id,
            p.clamp(0, i32::MAX) as u32,
            c.clamp(0, i32::MAX) as u32,
            t.clamp(0, i32::MAX) as u32,
            0.0,
        )
        .await;
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
        if let Err(e) = logger.log(log) {
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
