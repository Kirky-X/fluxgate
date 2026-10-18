// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAI JSON Schema → Gemini `responseSchema` / function `parameters` 清洗。
//!
//! Gemini Schema 是 OpenAPI 子集：不支持 `$ref`/`$defs`/`additionalProperties`
//! 等键。本模块做**有界内联**（深度上限 32、环引用显式报错），并删除上游
//! 必拒的约束键；无法等价表达的布尔 schema 退化为 `{"type":"object"}`。

use crate::error::GateError;
use crate::i18n::t_args;
use serde_json::{json, Map, Value};

/// 引用内联深度上限（防病态 schema 递归展开）。
const MAX_REF_DEPTH: usize = 32;

/// 清洗入口：任意 JSON Schema → Gemini 可接受的 Schema 对象。
pub fn sanitize_schema(schema: &Value) -> Result<Value, GateError> {
    let mut stack = Vec::new();
    sanitize_node(schema, schema, &mut stack, 0)
}

/// `root` 用于解析 `$ref`；`node` 为当前节点。
fn sanitize_node(
    root: &Value,
    node: &Value,
    stack: &mut Vec<String>,
    depth: usize,
) -> Result<Value, GateError> {
    if depth > MAX_REF_DEPTH {
        return Err(schema_error("api-gemini-schema-ref-depth", "$ref"));
    }
    match node {
        // 布尔 schema（true=任意 / false=拒绝）无 Gemini 等价表达：
        // 统一退化为 object，避免把布尔值塞进 responseSchema 触发 400。
        Value::Bool(_) => Ok(json!({"type": "object"})),
        Value::Object(obj) => {
            if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
                return resolve_ref(root, reference, stack, depth);
            }
            let mut out = Map::new();
            for (key, value) in obj {
                match key.as_str() {
                    // 上游必拒或已由内联处理的键
                    "$schema"
                    | "$id"
                    | "$defs"
                    | "definitions"
                    | "additionalProperties"
                    | "strict"
                    | "$comment" => continue,
                    "const" => {
                        out.insert("enum".into(), json!([value]));
                    }
                    "type" => {
                        if let Some(types) = value.as_array() {
                            let nullable = types.iter().any(|t| t.as_str() == Some("null"));
                            if let Some(first) = types.iter().find(|t| t.as_str() != Some("null")) {
                                out.insert("type".into(), first.clone());
                            }
                            if nullable {
                                out.insert("nullable".into(), json!(true));
                            }
                        } else {
                            out.insert("type".into(), value.clone());
                        }
                    }
                    "properties" => {
                        let mut properties = Map::new();
                        if let Some(entries) = value.as_object() {
                            for (name, child) in entries {
                                properties.insert(
                                    name.clone(),
                                    sanitize_node(root, child, stack, depth + 1)?,
                                );
                            }
                        }
                        out.insert("properties".into(), Value::Object(properties));
                    }
                    "items" => {
                        out.insert(
                            "items".into(),
                            sanitize_node(root, value, stack, depth + 1)?,
                        );
                    }
                    "anyOf" | "allOf" | "oneOf" => {
                        let mut branches = Vec::new();
                        if let Some(entries) = value.as_array() {
                            for child in entries {
                                branches.push(sanitize_node(root, child, stack, depth + 1)?);
                            }
                        }
                        out.insert(key.clone(), Value::Array(branches));
                    }
                    _ => {
                        out.insert(key.clone(), value.clone());
                    }
                }
            }
            Ok(Value::Object(out))
        }
        // 非对象/布尔 schema 无合法形态：按 object 兜底（调用方仍可被上游拒绝）
        _ => Ok(json!({"type": "object"})),
    }
}

/// 解析本地引用（仅 `#/$defs/<name>` 与 `#/definitions/<name>`）。
fn resolve_ref(
    root: &Value,
    reference: &str,
    stack: &mut Vec<String>,
    depth: usize,
) -> Result<Value, GateError> {
    let name = reference
        .strip_prefix("#/$defs/")
        .or_else(|| reference.strip_prefix("#/definitions/"))
        .ok_or_else(|| schema_error("api-gemini-schema-ref-unsupported", reference))?;
    // JSON Pointer 转义（`~1` → `/`，`~0` → `~`）
    let name = name.replace("~1", "/").replace("~0", "~");
    if stack.iter().any(|seen| seen == &name) {
        return Err(schema_error("api-gemini-schema-ref-cycle", reference));
    }
    let definition = root
        .get("$defs")
        .or_else(|| root.get("definitions"))
        .and_then(|defs| defs.get(&name))
        .ok_or_else(|| schema_error("api-gemini-schema-ref-missing", reference))?;
    stack.push(name);
    let result = sanitize_node(root, definition, stack, depth + 1);
    stack.pop();
    result
}

fn schema_error(key: &str, detail: &str) -> GateError {
    GateError::Validation {
        field: t_args(key, &[("detail", detail.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn removes_openai_only_keys_and_keeps_supported() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "title": "Out",
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["name"]
        });
        let out = sanitize_schema(&schema).unwrap();
        assert!(out.get("$schema").is_none());
        assert!(out.get("additionalProperties").is_none());
        assert_eq!(out["title"], "Out");
        assert_eq!(out["properties"]["name"]["minLength"], 1);
        assert_eq!(out["properties"]["tags"]["items"]["type"], "string");
        assert_eq!(out["required"], json!(["name"]));
    }

    #[test]
    fn inlines_local_refs() {
        let schema = json!({
            "$defs": {
                "Item": {"type": "object", "properties": {"id": {"type": "integer"}}}
            },
            "type": "object",
            "properties": {"item": {"$ref": "#/$defs/Item"}}
        });
        let out = sanitize_schema(&schema).unwrap();
        assert_eq!(out["properties"]["item"]["type"], "object");
        assert_eq!(
            out["properties"]["item"]["properties"]["id"]["type"],
            "integer"
        );
        assert!(out.get("$defs").is_none());
    }

    #[test]
    fn cyclic_ref_is_rejected() {
        let schema = json!({
            "$defs": {
                "A": {"type": "object", "properties": {"b": {"$ref": "#/$defs/B"}}},
                "B": {"type": "object", "properties": {"a": {"$ref": "#/$defs/A"}}}
            },
            "$ref": "#/$defs/A"
        });
        let error = sanitize_schema(&schema).unwrap_err();
        assert!(matches!(error, GateError::Validation { .. }));
    }

    #[test]
    fn unsupported_ref_and_missing_ref_are_rejected() {
        let external = json!({"$ref": "https://example.com/schema.json"});
        assert!(sanitize_schema(&external).is_err());
        let missing = json!({"$ref": "#/$defs/nope"});
        assert!(sanitize_schema(&missing).is_err());
    }

    #[test]
    fn nullable_type_array_and_const_are_normalized() {
        let schema = json!({
            "type": "object",
            "properties": {
                "x": {"type": ["string", "null"]},
                "kind": {"const": "fixed"}
            }
        });
        let out = sanitize_schema(&schema).unwrap();
        assert_eq!(out["properties"]["x"]["type"], "string");
        assert_eq!(out["properties"]["x"]["nullable"], true);
        assert_eq!(out["properties"]["kind"]["enum"], json!(["fixed"]));
    }

    #[test]
    fn boolean_schema_degrades_to_object() {
        assert_eq!(
            sanitize_schema(&json!(true)).unwrap(),
            json!({"type": "object"})
        );
        assert_eq!(
            sanitize_schema(&json!({"type": "object", "properties": {"x": false}})).unwrap()
                ["properties"]["x"],
            json!({"type": "object"})
        );
    }
}
