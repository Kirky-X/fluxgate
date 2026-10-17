// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `POST /v1/messages/count_tokens` — Anthropic 兼容 token 计数。
//! 入参 `{model, messages, system?, tools?}`，返回 `{"input_tokens": u64}`；
//! 近似估算（字符数/4 + 结构开销，不引 tokenizer 依赖）。走既有鉴权与
//! 统一错误体系；model 存在性不校验（估计与注册无关）

use crate::error::{FluxgateError, GateError};
use crate::i18n::t;
use crate::utils::estimate_messages_tokens;
use crate::ApiState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn count_tokens(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, FluxgateError> {
    if body.get("model").and_then(Value::as_str).is_none() {
        return Err(FluxgateError::from(GateError::Validation {
            field: t("api-count-tokens-model-invalid"),
        }));
    }
    match body.get("messages").and_then(Value::as_array) {
        None => {
            return Err(FluxgateError::from(GateError::Validation {
                field: t("api-count-tokens-messages-invalid"),
            }));
        }
        Some(messages) if messages.is_empty() => {
            return Err(FluxgateError::from(GateError::Validation {
                field: t("api-count-tokens-messages-empty"),
            }));
        }
        Some(_) => {}
    }

    Ok(Json(json!({
        "input_tokens": estimate_messages_tokens(&body),
    })))
}
