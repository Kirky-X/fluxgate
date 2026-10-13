// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! `POST /admin/route/difficulty-explain` — 定档解释端点（只读，零计量
//! 副作用）。请求体 `{ model, body }`：model 为请求模型名（groups 判定），
//! body 为完整请求体（特征提取输入）。未接线 / groups 未命中返回 404 语义
//! 的 Validation 错误（与 route/explain 的未知模型同口径）。

use crate::api::state::ApiState;
use crate::error::{GateError, NexusFlowError};
use axum::extract::State;
use axum::Json;
use serde_json::Value;
use std::sync::Arc;

pub async fn route_difficulty_explain(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, NexusFlowError> {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            NexusFlowError::from(GateError::Validation {
                field: crate::i18n::t("api-route-explain-model-invalid"),
            })
        })?;
    let body = payload.get("body").cloned().unwrap_or(Value::Null);

    let difficulty = state.difficulty.as_ref().ok_or_else(|| {
        NexusFlowError::from(GateError::Validation {
            field: crate::i18n::t("api-route-explain-model-unknown").to_string(),
        })
    })?;
    let explanation = difficulty.explain(model, &body).await.ok_or_else(|| {
        NexusFlowError::from(GateError::Validation {
            field: crate::i18n::t("api-route-explain-model-unknown").to_string(),
        })
    })?;
    Ok(Json(explanation))
}
