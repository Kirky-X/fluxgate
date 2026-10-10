// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! `POST /admin/route/explain` — 路由可解释性（WP5）。
//!
//! 入参 `{model, session_id?}`，响应逐候选
//! `{vendor, model, decision, reason_code, detail}`（入选者另附 5 因子
//! 权重分解与 selection 档位）。纯只读：不发上游请求，不改亲和/熔断/
//! 队列任何状态。鉴权与既有 admin 端点一致（认证中间件；未鉴权 401）。
//! `reason_code` 为稳定机器码（不做 i18n）；未知模型 → 400
//! `validation_error`（复用既有 i18n 键）。

use crate::error::{GateError, NexusFlowError};
use crate::ApiState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn route_explain(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, NexusFlowError> {
    let model = body.get("model").and_then(Value::as_str).ok_or_else(|| {
        NexusFlowError::from(GateError::Validation {
            field: "model (missing or not a string)".into(),
        })
    })?;
    let session_id = body.get("session_id").and_then(Value::as_str);

    let assessments = state.router.explain_route(model, session_id).await;
    if assessments.is_empty() {
        return Err(NexusFlowError::from(GateError::Validation {
            field: format!("model '{model}' (unknown model)"),
        }));
    }

    let candidates: Vec<Value> = assessments
        .iter()
        .map(|a| {
            let mut item = json!({
                "vendor": a.vendor_id,
                "model": a.model_id,
                "decision": if a.eligible { "eligible" } else { "excluded" },
                "reason_code": a.reason_code,
                "detail": a.detail,
            });
            if a.eligible {
                let factors = a.factors.as_ref().expect("入选者必有分解");
                item["factors"] = json!({
                    "model": factors.model,
                    "queue": factors.queue,
                    "window": factors.window,
                    "latency": factors.latency,
                    "error": factors.error,
                });
                item["weight"] = json!(a.weight.expect("入选者必有权重"));
                item["base_weight"] = json!(a.base_weight.expect("入选者必有合成基准"));
                item["selection"] = json!(a.selection.expect("入选者必有档位"));
            }
            item
        })
        .collect();

    Ok(Json(json!({
        "model": model,
        "candidates": candidates,
    })))
}
