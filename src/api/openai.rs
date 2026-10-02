// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::api::RoutingHints;
use crate::database::RequestLog;
use crate::i18n::{t, t_args};
use crate::protocol::embedding_request_to_openai;
use crate::{error::NexusFlowError, router::RoutingDecision, ApiState};
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn chat_completions(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, NexusFlowError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(NexusFlowError::Validation {
            field: "model".into(),
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

    // 路由决策（支持 X-Vendor 和 X-Model 请求头；无 hints 时走亲和路由）。
    // recId=19(a)：X-Weight 请求级权重随 hints 传入——是否生效由
    // [routing].allow_request_weight 门控（Router 侧热读判定，关闭时忽略）
    let request_weight = hints.preferred_weight;
    // recId=15：LinUCB 请求特征 — 仅 selection="linucb"（热读）且接线时
    // 计算并参与选择；默认 weighted 档 None、零开销、行为与历史逐位一致。
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
    let linucb_features = linucb_x.as_ref().map(|a| a.as_slice());
    // recId=15（评审修复 major）：奖励臂键与选择侧同源归一——
    // - X-Vendor 直连（try_vendor_route 单候选）臂未被咨询 → None 跳过
    //   奖励（「未选择不更新」，避免伪样本）；
    // - X-Model → 该模型的归一键（route_with_hints 以其路由）；
    // - 其余（含 auto → "__global__"）→ body.model 归一键。
    let linucb_reward_key = if hints.preferred_vendor.is_some() {
        None
    } else {
        Some(crate::router::bandit::linucb_arm_key(
            hints.preferred_model.as_deref().unwrap_or(model),
        ))
    };
    // recId=17：x-deadline-ms 头（可选，非法值忽略）——消费与否由
    // [routing].lps_deadline_gate 热读门控（默认关闭不消费）
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
                Some(api_key_hash.as_str()),
                linucb_features,
                lps_deadline_ms,
            )
            .await?
    } else {
        state
            .router
            .route_weighted(
                model,
                session_key.as_deref(),
                Some(api_key_hash.as_str()),
                &[],
                request_weight,
                linucb_features,
                lps_deadline_ms,
            )
            .await?
    };

    // 检查是否可以使用零拷贝透传（入站格式 OpenAI == 出站格式 OpenAI）
    let use_passthrough = decision.vendor.type_ == "openai";

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
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        // recId=11：最终失败解除亲和绑定
                        crate::api::clear_session_affinity(
                            &state,
                            &api_key_hash,
                            session_key.as_deref(),
                        )
                        .await;
                        log_chat_failure(
                            &state,
                            &request_id,
                            "openai",
                            &decision,
                            &body,
                            duration_ms,
                            &e,
                            linucb_reward_key.as_deref(),
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
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        // recId=11：最终失败解除亲和绑定
                        crate::api::clear_session_affinity(
                            &state,
                            &api_key_hash,
                            session_key.as_deref(),
                        )
                        .await;
                        log_chat_failure(
                            &state,
                            &request_id,
                            "openai",
                            &decision,
                            &body,
                            duration_ms,
                            &e,
                            linucb_reward_key.as_deref(),
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
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_chat_failure(
                        &state,
                        &request_id,
                        "openai",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                        linucb_reward_key.as_deref(),
                    )
                    .await;
                    return Err(e);
                }
            };

            // recId=11：派发成功即回写亲和绑定（流后续错误由 metrics 记账）
            // recId=10(3)：首绑定收敛（tenant_scoped 开启时 bind_if_absent）
            crate::api::bind_session_affinity(
                &state,
                &api_key_hash,
                session_key.as_deref(),
                &decision.vendor.id,
            )
            .await;

            // recId=3 阶段一：透传流过分帧器逐帧原样转发（帧边界与上游一致），
            // 替换旧的整块 bytes 直转 —— 后者无法检测上游中流 error 事件，
            // 且错误中断会伪装成正常流结束
            let body_stream = axum::body::Body::from_stream(
                crate::api::sse::reframe_passthrough_stream(stream_response),
            );

            let duration_ms = start_time.elapsed().as_millis() as i64;
            // recId=4⑤：流式「派发处计」— metrics 已在 manager 层记账，
            // handler 不重复计；日志按派发成功记 200
            log_chat_request(
                &state,
                &request_id,
                "openai",
                &decision,
                &body,
                Some(&json!({"stream": true})),
                duration_ms,
                200,
                None,
                linucb_reward_key.as_deref(),
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
            log_chat_request(
                &state,
                &request_id,
                "openai",
                &decision,
                &body,
                Some(cached),
                duration_ms,
                200,
                None,
                linucb_reward_key.as_deref(),
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
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_chat_failure(
                        &state,
                        &request_id,
                        "openai",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                        linucb_reward_key.as_deref(),
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
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_chat_failure(
                        &state,
                        &request_id,
                        "openai",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                        linucb_reward_key.as_deref(),
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
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：最终失败解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref())
                    .await;
                log_chat_failure(
                    &state,
                    &request_id,
                    "openai",
                    &decision,
                    &body,
                    duration_ms,
                    &e,
                    linucb_reward_key.as_deref(),
                )
                .await;
                return Err(e);
            }
        };

        // recId=11：执行成功回写亲和绑定（换路成功时 decision 已是最终
        // 成功者——失败迁移绑定语义）；recId=12：未命中则回写响应缓存
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
        log_chat_request(
            &state,
            &request_id,
            "openai",
            &decision,
            &body,
            Some(&response),
            duration_ms,
            200,
            None,
            linucb_reward_key.as_deref(),
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

    // 转换路径：OpenAI → Unified → 目标厂商
    let unified_request = state.openai_converter.to_unified(body.clone())?;

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
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_chat_failure(
                        &state,
                        &request_id,
                        "openai",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                        linucb_reward_key.as_deref(),
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
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    // recId=11：最终失败解除亲和绑定
                    crate::api::clear_session_affinity(
                        &state,
                        &api_key_hash,
                        session_key.as_deref(),
                    )
                    .await;
                    log_chat_failure(
                        &state,
                        &request_id,
                        "openai",
                        &decision,
                        &body,
                        duration_ms,
                        &e,
                        linucb_reward_key.as_deref(),
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
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：最终失败解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref())
                    .await;
                log_chat_failure(
                    &state,
                    &request_id,
                    "openai",
                    &decision,
                    &body,
                    duration_ms,
                    &e,
                    linucb_reward_key.as_deref(),
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

        // IR → OpenAI 出站事件（存量 stream_chunk_to_external）。
        // recId=14②：错误发带语义的 SSE error 事件后提前终止，不再补
        // [DONE] —— 客户端与重试逻辑可分辨成败（旧实现错误后仍以 [DONE]
        // 收尾伪装正常结束）
        let converter = state.openai_converter.clone();
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

        let body_stream = axum::body::Body::from_stream(sse_stream);

        // 记录流式请求日志 (简化) — recId=4⑤：metrics 在 manager 层记账，
        // handler 不重复计；日志按派发成功记 200
        let duration_ms = start_time.elapsed().as_millis() as i64;
        log_chat_request(
            &state,
            &request_id,
            "openai",
            &decision,
            &body,
            Some(&json!({"stream": true})),
            duration_ms,
            200,
            None,
            linucb_reward_key.as_deref(),
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

    // 普通请求 - 通过 UnifiedRequest
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
        log_chat_request(
            &state,
            &request_id,
            "openai",
            &decision,
            &body,
            Some(cached),
            duration_ms,
            200,
            None,
            linucb_reward_key.as_deref(),
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
                let duration_ms = start_time.elapsed().as_millis() as i64;
                // recId=11：换路 fallback 耗尽（最终失败）解除亲和绑定
                crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref()).await;
                log_chat_failure(
                    &state,
                    &request_id,
                    "openai",
                    &decision,
                    &body,
                    duration_ms,
                    &e,
                linucb_reward_key.as_deref(),
            )
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
            let duration_ms = start_time.elapsed().as_millis() as i64;
            // recId=11：最终失败解除亲和绑定
            crate::api::clear_session_affinity(&state, &api_key_hash, session_key.as_deref()).await;
            log_chat_failure(
                &state,
                &request_id,
                "openai",
                &decision,
                &body,
                duration_ms,
                &e,
                linucb_reward_key.as_deref(),
            )
            .await;
            return Err(e);
        }
    };

    // 将统一响应转换回 OpenAI 格式
    let response = state
        .openai_converter
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
    log_chat_request(
        &state,
        &request_id,
        "openai",
        &decision,
        &body,
        Some(&response),
        duration_ms,
        200,
        None,
        linucb_reward_key.as_deref(),
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
    // （评审修复 major，见 chat_completions 的 linucb_reward_key）。
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
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(NexusFlowError::Validation {
            field: "model".into(),
        })?;

    let decision = match state.router.route(model).await {
        Ok(d) => d,
        Err(e) => return Err(e),
    };

    // 只支持 OpenAI vendor
    if decision.vendor.type_ != "openai" {
        return Err(NexusFlowError::Protocol {
            source: t("api-embeddings-openai-only"),
            inner: None,
        });
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
        if let Err(e) = logger.log(log) {
            tracing::warn!(
                "{}",
                t_args("api-log-push-rejected", &[("error", e.to_string())])
            );
        }
    }
}
