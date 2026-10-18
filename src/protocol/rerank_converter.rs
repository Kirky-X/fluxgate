// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Rerank 请求校验与 Cohere 形状转换（纯函数，无 IO）。
//! 校验口径：必填缺一不可、越界显式拒绝不静默截断——documents 条数、
//! 单条长度、query 长度、top_n 越界都在入口拦下，避免上游以不可诊断的
//! 400 返回或静默截断导致客户端 index 对不齐。

use crate::error::GateError;
use serde_json::{json, Value};

/// 单请求 documents 条数上限
pub const MAX_RERANK_DOCUMENTS: usize = 1000;
/// 单条文档字符数上限
pub const MAX_RERANK_DOC_CHARS: usize = 8192;
/// query 字符数上限
pub const MAX_RERANK_QUERY_CHARS: usize = 4096;

/// 校验后的重排请求（documents 统一归一为文本）
#[derive(Debug, Clone, PartialEq)]
pub struct RerankRequest {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: usize,
    pub return_documents: bool,
    pub instruction: Option<String>,
}

fn validation(field: &str) -> GateError {
    GateError::Validation {
        field: field.to_string(),
    }
}

/// 解析并校验入站请求体（Cohere 兼容形状）。
pub fn parse_rerank_request(body: &Value) -> Result<RerankRequest, GateError> {
    if body.get("stream").is_some() {
        return Err(validation("stream (not supported)"));
    }

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| validation("model"))?;

    let query = body
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| validation("query"))?;
    if query.chars().count() > MAX_RERANK_QUERY_CHARS {
        return Err(validation("query (max 4096 chars)"));
    }

    let docs = body
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| validation("documents"))?;
    if docs.is_empty() {
        return Err(validation("documents"));
    }
    if docs.len() > MAX_RERANK_DOCUMENTS {
        return Err(validation("documents (max 1000 items)"));
    }

    let mut documents = Vec::with_capacity(docs.len());
    for (index, doc) in docs.iter().enumerate() {
        let text = match doc {
            Value::String(text) => text.as_str(),
            Value::Object(map) => map
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| validation(&format!("documents[{index}]")))?,
            _ => return Err(validation(&format!("documents[{index}]"))),
        };
        if text.chars().count() > MAX_RERANK_DOC_CHARS {
            return Err(validation(&format!("documents[{index}] (max 8192 chars)")));
        }
        documents.push(text.to_string());
    }

    let top_n = match body.get("top_n") {
        None => documents.len(),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| validation("top_n"))?,
    };
    if top_n == 0 || top_n > documents.len() {
        return Err(validation("top_n"));
    }

    let return_documents = match body.get("return_documents") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| validation("return_documents"))?,
    };

    let instruction = match body.get("instruction") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(validation("instruction")),
    };

    Ok(RerankRequest {
        model: model.to_string(),
        query: query.to_string(),
        documents,
        top_n,
        return_documents,
        instruction,
    })
}

/// 上游结果条目（document 仅在 return_documents=true 时有值）
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResultItem {
    pub index: usize,
    pub relevance_score: f64,
    pub document: Option<String>,
}

fn protocol_error(source: &str) -> GateError {
    GateError::Protocol {
        source: source.to_string(),
        inner: None,
    }
}

/// 构造上游 Cohere 形状请求；`upstream_model` 为条目 model_id（上游真实名）。
pub fn to_upstream_request(request: &RerankRequest, upstream_model: &str) -> Value {
    let mut body = json!({
        "model": upstream_model,
        "query": request.query,
        "documents": request.documents,
        "top_n": request.top_n,
        "return_documents": request.return_documents,
    });
    if let Some(instruction) = &request.instruction {
        body["instruction"] = Value::String(instruction.clone());
    }
    body
}

/// 解析上游响应：index 越界/重复、分数非有限、缺 results 一律 502
///（防止客户端按 index 取文档时数组越界，或 NaN 进入排序）。
pub fn parse_rerank_response(
    body: &Value,
    request: &RerankRequest,
) -> Result<Vec<RerankResultItem>, GateError> {
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_error("rerank upstream response missing results"))?;

    let mut seen = std::collections::HashSet::with_capacity(results.len());
    let mut items = Vec::with_capacity(results.len());
    for item in results {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| protocol_error("rerank upstream result index"))?;
        if index >= request.documents.len() || !seen.insert(index) {
            return Err(protocol_error(
                "rerank upstream result index out of range or duplicated",
            ));
        }
        let relevance_score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .filter(|score| score.is_finite())
            .ok_or_else(|| protocol_error("rerank upstream relevance_score"))?;
        let document = item.get("document").map(|doc| match doc {
            Value::String(text) => text.clone(),
            Value::Object(map) => map
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        });
        items.push(RerankResultItem {
            index,
            relevance_score,
            document,
        });
    }
    Ok(items)
}

/// 分数归一化（配置层已白名单）：`sigmoid` 映射到 (0,1)；`minmax` 批内
/// 缩放（max==min 全 0.5）；其他值（含 `none`）恒等。只改分数，不改
/// `index`/`document`/排序。
pub fn normalize_rerank_scores(items: &mut [RerankResultItem], mode: &str) {
    match mode.trim().to_ascii_lowercase().as_str() {
        "sigmoid" => {
            for item in items.iter_mut() {
                item.relevance_score = 1.0 / (1.0 + (-item.relevance_score).exp());
            }
        }
        "minmax" => {
            let Some(min) = items.iter().map(|i| i.relevance_score).reduce(f64::min) else {
                return;
            };
            let max = items
                .iter()
                .map(|i| i.relevance_score)
                .fold(f64::NEG_INFINITY, f64::max);
            let range = max - min;
            for item in items.iter_mut() {
                item.relevance_score = if range > f64::EPSILON {
                    (item.relevance_score - min) / range
                } else {
                    0.5
                };
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Value {
        json!({
            "model": "rerank-v3.5",
            "query": "什么是重排",
            "documents": ["doc-a", {"text": "doc-b"}],
        })
    }

    #[test]
    fn minimal_request_defaults_top_n_and_return_documents() {
        let request = parse_rerank_request(&minimal()).expect("合法最小请求");
        assert_eq!(request.model, "rerank-v3.5");
        assert_eq!(request.documents, vec!["doc-a", "doc-b"]);
        assert_eq!(request.top_n, 2, "top_n 缺省 = documents.len()");
        assert!(!request.return_documents, "return_documents 缺省 false");
        assert_eq!(request.instruction, None);
    }

    #[test]
    fn explicit_fields_are_carried_through() {
        let body = json!({
            "model": " m1 ",
            "query": "q",
            "documents": ["a"],
            "top_n": 1,
            "return_documents": true,
            "instruction": "rank by relevance",
        });
        let request = parse_rerank_request(&body).expect("显式字段");
        assert_eq!(request.model, "m1", "model 两端空白归一");
        assert_eq!(request.top_n, 1);
        assert!(request.return_documents);
        assert_eq!(request.instruction.as_deref(), Some("rank by relevance"));
    }

    #[test]
    fn required_fields_missing_rejected() {
        for (body, field) in [
            (json!({"query": "q", "documents": ["a"]}), "model"),
            (json!({"model": "m", "documents": ["a"]}), "query"),
            (json!({"model": "m", "query": "q"}), "documents"),
            (
                json!({"model": "   ", "query": "q", "documents": ["a"]}),
                "model",
            ),
            (
                json!({"model": "m", "query": "q", "documents": []}),
                "documents",
            ),
        ] {
            let err = parse_rerank_request(&body).expect_err("必填缺失必须拒绝");
            match err {
                GateError::Validation { field: actual } => {
                    assert_eq!(actual, field, "错误字段名必须精确");
                }
                other => panic!("expected Validation, got {other:?}"),
            }
        }
    }

    #[test]
    fn document_shapes_other_than_text_rejected() {
        for body in [
            json!({"model": "m", "query": "q", "documents": [1]}),
            json!({"model": "m", "query": "q", "documents": [{"body": "x"}]}),
            json!({"model": "m", "query": "q", "documents": [{"text": 1}]}),
        ] {
            let err = parse_rerank_request(&body).expect_err("非文本元素必须拒绝");
            assert!(
                matches!(err, GateError::Validation { ref field } if field.starts_with("documents[0]")),
                "错误必须点名越界元素: {err:?}"
            );
        }
    }

    #[test]
    fn documents_over_cap_rejected() {
        let docs: Vec<String> = (0..=MAX_RERANK_DOCUMENTS)
            .map(|i| format!("d{i}"))
            .collect();
        let body = json!({"model": "m", "query": "q", "documents": docs});
        let err = parse_rerank_request(&body).expect_err("超 1000 条必须拒绝");
        assert!(
            matches!(err, GateError::Validation { ref field } if field.contains("max 1000")),
            "错误必须携带上限: {err:?}"
        );
    }

    #[test]
    fn oversized_document_and_query_rejected() {
        let long_doc = "x".repeat(MAX_RERANK_DOC_CHARS + 1);
        let err = parse_rerank_request(&json!({
            "model": "m", "query": "q", "documents": [long_doc],
        }))
        .expect_err("单条超 8192 必须拒绝");
        assert!(
            matches!(err, GateError::Validation { ref field } if field.contains("8192")),
            "错误必须携带上限: {err:?}"
        );

        let long_query = "q".repeat(MAX_RERANK_QUERY_CHARS + 1);
        let err = parse_rerank_request(&json!({
            "model": "m", "query": long_query, "documents": ["a"],
        }))
        .expect_err("query 超 4096 必须拒绝");
        assert!(
            matches!(err, GateError::Validation { ref field } if field.contains("4096")),
            "错误必须携带上限: {err:?}"
        );
    }

    #[test]
    fn top_n_out_of_range_or_wrong_type_rejected() {
        for top_n in [json!(0), json!(3), json!(-1), json!("2"), json!(1.5)] {
            let body = json!({
                "model": "m", "query": "q", "documents": ["a", "b"], "top_n": top_n,
            });
            let err = parse_rerank_request(&body).expect_err("top_n 越界必须拒绝");
            assert!(
                matches!(err, GateError::Validation { ref field } if field == "top_n"),
                "错误必须点名 top_n: {err:?}"
            );
        }
    }

    #[test]
    fn stream_and_wrong_optional_types_rejected() {
        let stream_err = parse_rerank_request(&json!({
            "model": "m", "query": "q", "documents": ["a"], "stream": true,
        }))
        .expect_err("stream 必须拒绝");
        assert!(
            matches!(stream_err, GateError::Validation { ref field } if field.contains("stream")),
            "错误必须点名 stream: {stream_err:?}"
        );

        let bool_err = parse_rerank_request(&json!({
            "model": "m", "query": "q", "documents": ["a"], "return_documents": "yes",
        }))
        .expect_err("return_documents 非 bool 必须拒绝");
        assert!(
            matches!(bool_err, GateError::Validation { ref field } if field == "return_documents"),
            "错误必须点名 return_documents: {bool_err:?}"
        );

        let instruction_err = parse_rerank_request(&json!({
            "model": "m", "query": "q", "documents": ["a"], "instruction": 42,
        }))
        .expect_err("instruction 非字符串必须拒绝");
        assert!(
            matches!(instruction_err, GateError::Validation { ref field } if field == "instruction"),
            "错误必须点名 instruction: {instruction_err:?}"
        );
    }

    #[test]
    fn upstream_request_uses_real_model_and_defaults() {
        let request = parse_rerank_request(&minimal()).expect("合法请求");
        let upstream = to_upstream_request(&request, "rerank-v3.5-real");
        assert_eq!(upstream["model"], "rerank-v3.5-real");
        assert_eq!(upstream["query"], "什么是重排");
        assert_eq!(upstream["documents"], json!(["doc-a", "doc-b"]));
        assert_eq!(upstream["top_n"], 2);
        assert_eq!(upstream["return_documents"], false);
        assert!(
            upstream.get("instruction").is_none(),
            "未声明 instruction 不得注入"
        );
    }

    #[test]
    fn upstream_request_carries_instruction() {
        let body = json!({
            "model": "m", "query": "q", "documents": ["a"],
            "instruction": "prefer exact matches",
        });
        let request = parse_rerank_request(&body).expect("合法请求");
        let upstream = to_upstream_request(&request, "m-real");
        assert_eq!(upstream["instruction"], "prefer exact matches");
    }

    #[test]
    fn upstream_response_maps_results_in_order() {
        let body = json!({
            "model": "m", "query": "q", "documents": ["a", "b"],
            "return_documents": true,
        });
        let request = parse_rerank_request(&body).expect("合法请求");
        let response = json!({
            "results": [
                {"index": 1, "relevance_score": 0.9, "document": {"text": "b"}},
                {"index": 0, "relevance_score": 0.1, "document": {"text": "a"}},
            ]
        });
        let items = parse_rerank_response(&response, &request).expect("合法响应");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].index, 1);
        assert!((items[0].relevance_score - 0.9).abs() < f64::EPSILON);
        assert_eq!(items[0].document.as_deref(), Some("b"));
        assert_eq!(items[1].index, 0);
    }

    #[test]
    fn upstream_response_requires_results_array() {
        let request = parse_rerank_request(&minimal()).expect("合法请求");
        let err =
            parse_rerank_response(&json!({"meta": {}}), &request).expect_err("缺 results 必须拒绝");
        assert_eq!(err.status_code(), 502);
    }

    #[test]
    fn upstream_response_out_of_range_or_malformed_rejected() {
        let request = parse_rerank_request(&minimal()).expect("合法请求");
        for response in [
            json!({"results": [{"index": 2, "relevance_score": 0.5}]}),
            json!({"results": [
                {"index": 0, "relevance_score": 0.5},
                {"index": 0, "relevance_score": 0.4},
            ]}),
            json!({"results": [{"index": 0, "relevance_score": "high"}]}),
            json!({"results": [{"relevance_score": 0.5}]}),
        ] {
            let err = parse_rerank_response(&response, &request).expect_err("非法上游响应必须拒绝");
            assert_eq!(err.status_code(), 502, "上游形状错误映射 502");
        }
    }

    fn items_with(scores: &[f64]) -> Vec<RerankResultItem> {
        scores
            .iter()
            .enumerate()
            .map(|(index, score)| RerankResultItem {
                index,
                relevance_score: *score,
                document: None,
            })
            .collect()
    }

    #[test]
    fn sigmoid_normalization_is_monotonic_and_bounded() {
        let mut items = items_with(&[-2.0, 0.0, 3.0]);
        normalize_rerank_scores(&mut items, "sigmoid");
        assert!((items[0].relevance_score - 0.119_202_92).abs() < 1e-6);
        assert!((items[1].relevance_score - 0.5).abs() < 1e-9);
        assert!((items[2].relevance_score - 0.952_574_13).abs() < 1e-6);
        assert!(
            items
                .windows(2)
                .all(|w| w[0].relevance_score < w[1].relevance_score),
            "单调性保持"
        );
        assert!(items.iter().all(|i| i.relevance_score.is_finite()));
    }

    #[test]
    fn minmax_normalization_scales_batch_and_keeps_order() {
        let mut items = items_with(&[0.2, 0.9, 0.5]);
        normalize_rerank_scores(&mut items, "minmax");
        assert!(items[0].relevance_score.abs() < 1e-9, "最小值→0");
        assert!((items[1].relevance_score - 1.0).abs() < 1e-9, "最大值→1");
        assert!((items[2].relevance_score - 0.428_571_428).abs() < 1e-6);
        assert_eq!(items[0].index, 0, "index 不变");

        let mut equal = items_with(&[0.7, 0.7]);
        normalize_rerank_scores(&mut equal, "minmax");
        assert!(
            equal.iter().all(|i| (i.relevance_score - 0.5).abs() < 1e-9),
            "等值批全 0.5"
        );

        let mut empty: Vec<RerankResultItem> = Vec::new();
        normalize_rerank_scores(&mut empty, "minmax");
        let mut single = items_with(&[0.3]);
        normalize_rerank_scores(&mut single, "minmax");
        assert!((single[0].relevance_score - 0.5).abs() < 1e-9);

        let mut none = items_with(&[0.2, 0.9]);
        normalize_rerank_scores(&mut none, "none");
        assert_eq!(none[0].relevance_score, 0.2, "none 恒等");
        assert_eq!(none[1].relevance_score, 0.9);
    }
}
