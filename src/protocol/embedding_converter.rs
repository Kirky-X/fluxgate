// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::error::GateError;
use serde_json::{json, Value};

/// OpenAI Embedding 请求转换。
///
/// input 语义：字符串 → 单元素数组；字符串数组 / 整数 token 数组 /
/// 嵌套整数 token 数组原样透传（不裁剪）；混合形态、浮点/布尔元素、
/// 空值 → `validation_error`（静默丢弃会让客户端无法按 `data[].index`
/// 对齐返回的嵌入，空 input 发上游只会换来不可诊断的错误）。
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
    let is_token = |value: &Value| value.as_i64().is_some() || value.as_u64().is_some();
    let input: Value = if let Some(text) = input_value.as_str() {
        json!([text])
    } else if let Some(items) = input_value.as_array() {
        if items.is_empty() {
            return Err(GateError::Validation {
                field: "input".into(),
            });
        }
        let all_strings = items.iter().all(Value::is_string);
        let all_tokens = items.iter().all(is_token);
        let all_token_arrays = items.iter().all(|item| {
            item.as_array()
                .is_some_and(|inner| !inner.is_empty() && inner.iter().all(is_token))
        });
        if all_strings || all_tokens || all_token_arrays {
            // 整数 token 数组与嵌套 token 数组按原 shape 透传（元素数 =
            // 返回条数对齐）；字符串数组同样原位保真
            input_value.clone()
        } else {
            return Err(GateError::Validation {
                field: "input".into(),
            });
        }
    } else {
        return Err(GateError::Validation {
            field: "input".into(),
        });
    };

    let mut result = json!({
        "model": model,
        "input": input,
    });

    // encoding_format（float/base64）字符串原样透传（D4；base64 响应由
    // 上游返回字符串，SDK 侧兼容）
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_and_string_array_inputs_keep_shape() {
        let out = embedding_request_to_openai(json!({"model": "m", "input": "hello"}))
            .expect("string input");
        assert_eq!(out["input"], json!(["hello"]));

        let out = embedding_request_to_openai(json!({"model": "m", "input": ["a", "b"]}))
            .expect("string array");
        assert_eq!(out["input"], json!(["a", "b"]));
    }

    #[test]
    fn token_and_nested_token_arrays_pass_through() {
        // 整数 token 数组：元素级保真（不得转字符串）
        let out = embedding_request_to_openai(json!({"model": "m", "input": [11, 22, 33]}))
            .expect("token array");
        assert_eq!(out["input"], json!([11, 22, 33]));
        assert!(out["input"][0].is_u64(), "必须保持整数类型");

        // 嵌套 token 数组：原 shape 透传不裁剪
        let out = embedding_request_to_openai(json!({"model": "m", "input": [[1, 2], [3]]}))
            .expect("nested token array");
        assert_eq!(out["input"], json!([[1, 2], [3]]));

        // 大 token id（u64 边界）不丢
        let out = embedding_request_to_openai(json!({"model": "m", "input": [200000]}))
            .expect("large token id");
        assert_eq!(out["input"], json!([200000]));
    }

    #[test]
    fn encoding_format_base64_passes_through() {
        let out = embedding_request_to_openai(json!({
            "model": "m",
            "input": "hi",
            "encoding_format": "base64",
            "dimensions": 256,
            "user": "u1"
        }))
        .expect("base64 format");
        assert_eq!(out["encoding_format"], "base64");
        assert_eq!(out["dimensions"], 256);
        assert_eq!(out["user"], "u1");
    }

    #[test]
    fn invalid_inputs_are_rejected_not_trimmed() {
        for input in [
            json!([]),
            json!(["a", 1]),
            json!([1.5]),
            json!([[1], "a"]),
            json!([[]]),
            json!([true]),
            json!(42),
            json!(null),
        ] {
            let result = embedding_request_to_openai(json!({"model": "m", "input": input}));
            match result {
                Err(GateError::Validation { field }) => assert_eq!(field, "input"),
                other => panic!("非法 input 必须拒绝: {other:?}"),
            }
        }
        // 缺失 input / 缺失 model
        assert!(embedding_request_to_openai(json!({"model": "m"})).is_err());
        assert!(embedding_request_to_openai(json!({"input": "hi"})).is_err());
    }
}
