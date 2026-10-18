// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAI Responses API wire 类型（请求 / 响应 / input item / output item /
//! usage / 流式事件）。
//!
//! 设计口径：
//! - 请求侧 `input` 保留原始 `Value`（字符串或数组两种形状），逐 item
//!   解析在 `convert_in`，避免 serde 对「无 type 的 message item」判别失败；
//! - 响应侧 `output` 用 tagged 枚举精确序列化，`extra` 保真槽承载未知键；
//! - 流式事件统一用 [`StreamEvent`] 解析（`type` 必填，其余按需）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 单请求 input items 上限（含回放历史）
pub const MAX_RESPONSES_INPUT_ITEMS: usize = 10_000;
/// `max_output_tokens` 上限
pub const MAX_RESPONSES_OUTPUT_TOKENS: u64 = 1_000_000;
/// `metadata` 条目上限
pub const MAX_RESPONSES_METADATA_ENTRIES: usize = 16;
/// `metadata` 键长度上限（字符）
pub const MAX_RESPONSES_METADATA_KEY_CHARS: usize = 64;
/// `metadata` 值长度上限（字符）
pub const MAX_RESPONSES_METADATA_VALUE_CHARS: usize = 512;

/// `POST /v1/responses` 请求体（`input` 原样保留，解析见 convert_in）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    /// 未识别顶层字段保真槽（原生直通原样转发；IR 路径忽略）
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `text.format`（结构化输出）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
}

/// `reasoning`（努力级别与摘要）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// input message item（`role` + 字符串或 content 数组）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
}

/// input `function_call` item（上一轮 assistant 工具调用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputFunctionCall {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// input `function_call_output` item（工具执行结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputFunctionCallOutput {
    pub call_id: String,
    #[serde(default)]
    pub output: Value,
}

/// input `item_reference` item（引用网关存储中的历史 item）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputItemReference {
    pub id: String,
}

/// 解析后的 input item 判别结果（`convert_in` 产出）。
#[derive(Debug, Clone)]
pub enum ParsedInputItem {
    Message(InputMessage),
    FunctionCall(InputFunctionCall),
    FunctionCallOutput(InputFunctionCallOutput),
    /// reasoning 在 IR 路径无承载（原生直通才保留）
    Reasoning,
    ItemReference(String),
    /// 未知 item：IR 路径忽略并 debug 日志
    Unknown(Value),
}

/// 输出内容块（目前仅 `output_text`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContent {
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<Value>,
    },
}

/// 输出 item。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        status: String,
        role: String,
        content: Vec<OutputContent>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: String,
    },
    /// 原生直通时上游 reasoning item 原样保留（IR 路径不合成）
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<Value>,
    },
}

impl OutputItem {
    /// item 类型标签（流式事件与日志用）。
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Message { .. } => "message",
            Self::FunctionCall { .. } => "function_call",
            Self::Reasoning { .. } => "reasoning",
        }
    }
}

/// Responses `usage`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponsesUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<InputTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
}

/// 错误对象（`response.failed` 与 `error` 事件）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

/// 未完成原因。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteDetails {
    pub reason: String,
}

/// Responses 响应对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: String,
    pub created_at: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<IncompleteDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    pub model: String,
    /// 上游响应必须携带 output 数组（缺失/非数组 → 反序列化失败 → 502）
    pub output: Vec<OutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub store: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(default)]
    pub usage: ResponsesUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// 未识别字段保真槽（原生直通响应）
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// 流式事件（双向共用：入站编码与出站解析）。
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub response: Option<ResponsesResponse>,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub item: Option<Value>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub output_index: Option<u32>,
    #[serde(default)]
    pub content_index: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl StreamEvent {
    /// 从 SSE `data` 负载解析；非法 JSON 或缺少 `type` 返回 None。
    pub fn from_data(data: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(data).ok()?;
        let event_type = value.get("type")?.as_str()?.to_string();
        let mut event: StreamEvent = serde_json::from_value(value).ok()?;
        event.event_type = event_type;
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_parses_string_and_array_input() {
        let string_input: ResponsesRequest = serde_json::from_value(json!({
            "model": "gpt-4.1",
            "input": "hello"
        }))
        .expect("字符串 input");
        assert_eq!(string_input.input, json!("hello"));
        assert_eq!(string_input.store, None);

        let array_input: ResponsesRequest = serde_json::from_value(json!({
            "model": "gpt-4.1",
            "input": [{"role": "user", "content": "hi"}],
            "store": false,
            "metadata": {"trace": "t1"}
        }))
        .expect("数组 input");
        assert!(array_input.input.is_array());
        assert_eq!(array_input.store, Some(false));
        assert_eq!(array_input.metadata.as_ref().unwrap()["trace"], "t1");
    }

    #[test]
    fn request_absorbs_unknown_fields() {
        let request: ResponsesRequest = serde_json::from_value(json!({
            "model": "m",
            "input": "x",
            "future_field": {"a": 1}
        }))
        .expect("未知字段吸收");
        assert_eq!(request.extra["future_field"], json!({"a": 1}));
    }

    #[test]
    fn output_items_serialize_with_type_tags() {
        let items = vec![
            OutputItem::Message {
                id: "msg_1".into(),
                status: "completed".into(),
                role: "assistant".into(),
                content: vec![OutputContent::OutputText {
                    text: "hello".into(),
                    annotations: vec![],
                }],
            },
            OutputItem::FunctionCall {
                id: "fc_1".into(),
                call_id: "call_1".into(),
                name: "f".into(),
                arguments: "{}".into(),
                status: "completed".into(),
            },
            OutputItem::Reasoning {
                id: "rs_1".into(),
                summary: vec![],
            },
        ];
        let value = serde_json::to_value(&items).expect("序列化");
        assert_eq!(value[0]["type"], "message");
        assert_eq!(value[0]["content"][0]["type"], "output_text");
        assert_eq!(value[1]["type"], "function_call");
        assert_eq!(value[2]["type"], "reasoning");
    }

    #[test]
    fn usage_parses_details() {
        let response: ResponsesResponse = serde_json::from_value(json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": "m",
            "output": [],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 3,
                "total_tokens": 13,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens_details": {"reasoning_tokens": 1}
            }
        }))
        .expect("响应解析");
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(
            response
                .usage
                .input_tokens_details
                .as_ref()
                .unwrap()
                .cached_tokens,
            4
        );
        assert_eq!(
            response
                .usage
                .output_tokens_details
                .as_ref()
                .unwrap()
                .reasoning_tokens,
            1
        );
    }

    #[test]
    fn stream_event_requires_type() {
        assert!(StreamEvent::from_data(r#"{"delta":"x"}"#).is_none());
        assert!(StreamEvent::from_data("not json").is_none());
        let event = StreamEvent::from_data(
            r#"{"type":"response.output_text.delta","delta":"hi","output_index":0}"#,
        )
        .expect("事件解析");
        assert_eq!(event.event_type, "response.output_text.delta");
        assert_eq!(event.delta.as_deref(), Some("hi"));
        assert_eq!(event.output_index, Some(0));
    }
}
