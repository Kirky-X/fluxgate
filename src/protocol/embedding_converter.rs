// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::GateError;
use serde_json::{json, Value};

/// OpenAI Embedding 请求转换。
///
/// input 语义为严格拒绝、不做静默裁剪：字符串 → 单元素数组；字符串数组 →
/// 原样；缺失 / 空数组 / 含非字符串元素 / 其他类型 → `validation_error`。
/// 静默丢弃元素会让客户端无法按 `data[].index` 对齐返回的嵌入（元素数与
/// 返回条数错位），空 input 发上游只会换来不可诊断的错误。
pub fn embedding_request_to_openai(body: Value) -> Result<Value, GateError> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(GateError::Validation {
            field: "model".into(),
        })?;

    let input_value = body.get("input").ok_or(GateError::Validation {
        field: "input".into(),
    })?;
    let input: Vec<String> = if let Some(text) = input_value.as_str() {
        vec![text.to_string()]
    } else if let Some(items) = input_value.as_array() {
        items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or(GateError::Validation {
                        field: "input".into(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        return Err(GateError::Validation {
            field: "input".into(),
        });
    };
    if input.is_empty() {
        return Err(GateError::Validation {
            field: "input".into(),
        });
    }

    let mut result = json!({
        "model": model,
        "input": input,
    });

    if let Some(encoding_format) = body.get("encoding_format").and_then(|v| v.as_str()) {
        result["encoding_format"] = json!(encoding_format);
    }

    if let Some(dimensions) = body.get("dimensions").and_then(|v| v.as_u64()) {
        result["dimensions"] = json!(dimensions);
    }

    if let Some(user) = body.get("user").and_then(|v| v.as_str()) {
        result["user"] = json!(user);
    }

    Ok(result)
}
