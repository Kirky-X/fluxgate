// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `POST /admin/route/explain` — 路由可解释性：入参 `{model, session_id?}`，
//! 响应逐候选 `{vendor, model, decision, reason_code, detail}`。纯只读；
//! reason_code 为稳定机器码（不做 i18n）；未知模型 → 400

use crate::error::{FluxgateError, GateError};
use crate::i18n::{t, t_args};
use crate::ApiState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn route_explain(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, FluxgateError> {
    let model = body.get("model").and_then(Value::as_str).ok_or_else(|| {
        FluxgateError::from(GateError::Validation {
            field: t("api-route-explain-model-invalid"),
        })
    })?;
    let session_id = body.get("session_id").and_then(Value::as_str);

    let assessments = state.router.explain_route(model, session_id).await;
    if assessments.is_empty() {
        return Err(FluxgateError::from(GateError::Validation {
            field: t_args(
                "api-route-explain-model-unknown",
                &[("model", model.to_string())],
            ),
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
                let factors = a
                    .factors
                    .as_ref()
                    .unwrap_or_else(|| panic!("{}", t("api-explain-no-factors")));
                item["factors"] = json!({
                    "model": factors.model,
                    "queue": factors.queue,
                    "window": factors.window,
                    "latency": factors.latency,
                    "error": factors.error,
                });
                item["weight"] = json!(a
                    .weight
                    .unwrap_or_else(|| panic!("{}", t("api-explain-no-weight"))));
                item["base_weight"] = json!(a
                    .base_weight
                    .unwrap_or_else(|| panic!("{}", t("api-explain-no-base"))));
                item["selection"] = json!(a
                    .selection
                    .unwrap_or_else(|| panic!("{}", t("api-explain-no-selection"))));
            }
            item
        })
        .collect();

    Ok(Json(json!({
        "model": model,
        "candidates": candidates,
    })))
}
