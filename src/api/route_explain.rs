// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `POST /admin/route/explain` — 路由可解释性：入参 `{model, session_id?}`，
//! 响应逐候选 `{vendor, model, decision, reason_code, detail}`。纯只读；
//! reason_code 为稳定机器码（不做 i18n）；未知模型 → 400

use crate::error::{FluxgateError, GateError};
use crate::i18n::{t, t_args};
use crate::ApiState;
use axum::{extract::State, http::HeaderMap, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn route_explain(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, FluxgateError> {
    let model = body.get("model").and_then(Value::as_str).ok_or_else(|| {
        FluxgateError::from(GateError::Validation {
            field: t("api-route-explain-model-invalid"),
        })
    })?;
    let session_id = body.get("session_id").and_then(Value::as_str);
    // 租户维度与路由同源（api_key_hash）：`affinity_tenant_scoped` 开启时
    // 亲和键带它；否则查 legacy 键空间。认证关闭时 digest 归 anonymous，
    // 与 bind/settle 侧的 None 判等由 `affinity_tenant_scoped` 门统一。
    let tenant = state
        .config
        .read()
        .routing
        .affinity_tenant_scoped
        .then(|| crate::utils::tenant::api_key_digest(&headers));
    let tenant = tenant.as_deref();

    let assessments = state.router.explain_route(model, session_id, tenant).await;
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
                // quota views 块（R-budget-002）：平局打破生效者与降档档位，
                // 与生产路由同快照单点（quota_factor_scores）
                item["quota"] = json!({
                    "tie_break": a.quota_tie_break.unwrap_or(false),
                    "degrade_tier": a.quota_degrade_tier,
                });
            }
            item
        })
        .collect();

    Ok(Json(json!({
        "model": model,
        "candidates": candidates,
        // 亲和决策块（R-affinity-004）：解释点只读，不续期不改状态
        "affinity": crate::api::routing_hints::affinity_decision_block(
            &state, session_id, tenant
        )
        .await,
    })))
}
