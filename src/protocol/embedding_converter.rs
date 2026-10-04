// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// recId=15③：原 `use super::*`（供已删除的 embedding_response_from_openai
// 引用 IR 类型）随死代码一并移除
use crate::error::GateError;
use serde_json::{json, Value};

/// OpenAI Embedding 请求转换
pub fn embedding_request_to_openai(body: Value) -> Result<Value, GateError> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(GateError::Validation {
            field: "model".into(),
        })?;

    let input = body
        .get("input")
        .map(|v| {
            if v.is_string() {
                vec![v.as_str().unwrap_or("").to_string()]
            } else if v.is_array() {
                v.as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();

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

// recId=15③：原 embedding_response_from_openai 死代码已删除 —
// 全库零生产调用（grep 仅 mod.rs re-export 与自身测试），且 embeddings
// 端点走治理透传路径后不再有统一化转换需求。UnifiedEmbeddingResponse 等
// 类型保留于 unified.rs（协议 IR 完整性）。
