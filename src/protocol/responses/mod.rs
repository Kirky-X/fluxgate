// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAI Responses API 协议层：wire 类型、请求/响应转换与语义化流式事件。
//!
//! 与 chat 转换器（`OpenAIConverter`/`AnthropicConverter`）的分工：
//! - 出站（chat IR → Responses 上游）实现 [`ProtocolConverter`]；
//! - 入站（Responses 客户端 → IR）使用本模块的富类型与
//!   [`convert_in::to_unified_request`]，避免把 item 语义硬塞进 chat IR。

pub mod convert_in;
pub mod convert_out;
pub mod stream;
pub mod types;

pub use convert_in::{parse_request, to_unified_request, ParsedResponsesRequest};
pub use convert_out::{now_secs, queued_response, to_responses_response};
pub use stream::{parse_stream_frame, ResponsesStreamEncoder};
pub use types::{
    IncompleteDetails, InputFunctionCall, InputFunctionCallOutput, InputItemReference,
    InputMessage, InputTokensDetails, OutputContent, OutputItem, OutputTokensDetails,
    ParsedInputItem, ReasoningConfig, ResponseError, ResponsesRequest, ResponsesResponse,
    ResponsesUsage, StreamEvent, TextConfig, MAX_RESPONSES_INPUT_ITEMS,
    MAX_RESPONSES_METADATA_ENTRIES, MAX_RESPONSES_METADATA_KEY_CHARS,
    MAX_RESPONSES_METADATA_VALUE_CHARS, MAX_RESPONSES_OUTPUT_TOKENS,
};

use crate::error::GateError;
use crate::protocol::unified::{
    FinishReason, Message, MessageContent, Role, TokenUsage, Tool, ToolChoice, UnifiedRequest,
    UnifiedResponse,
};
use crate::protocol::{Protocol, ProtocolConverter};
use serde_json::{json, Map, Value};

/// chat IR ↔ Responses 转换器（出站方向为主）。
pub struct ResponsesConverter;

impl ResponsesConverter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ResponsesConverter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProtocolConverter for ResponsesConverter {
    fn input_protocol(&self) -> Protocol {
        Protocol::Responses
    }

    fn output_protocol(&self) -> Protocol {
        Protocol::Responses
    }

    /// 入站 Responses 请求 → IR（与 [`convert_in::to_unified_request`] 同源，
    /// 仅消费当前回合 items）。
    fn to_unified(&self, body: Value) -> Result<UnifiedRequest, GateError> {
        let parsed = parse_request(&body)?;
        let items = parsed.current_items.clone();
        to_unified_request(&parsed, &items)
    }

    /// IR → Responses 请求体（chat 客户端命中 `api_style="responses"` 厂商）。
    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, GateError> {
        let mut instructions = request.system_prompt.clone().filter(|s| !s.is_empty());
        let mut input: Vec<Value> = Vec::new();
        for message in &request.messages {
            match (&message.role, &message.content) {
                (Role::System, content) => {
                    let text = message_text(content);
                    if !text.is_empty() {
                        instructions = Some(match instructions {
                            Some(existing) if existing != text => format!("{existing}\n{text}"),
                            Some(existing) => existing,
                            None => text,
                        });
                    }
                }
                (Role::Tool, MessageContent::ToolResult(result)) => input.push(json!({
                    "type": "function_call_output",
                    "call_id": result.tool_call_id,
                    "output": result.content,
                })),
                (Role::Tool, other) => input.push(json!({
                    "type": "function_call_output",
                    "call_id": "",
                    "output": message_text(other),
                })),
                (Role::Assistant, MessageContent::ToolCalls(calls)) => {
                    for call in calls {
                        input.push(function_call_item(
                            &call.id,
                            &call.function.name,
                            &call.function.arguments,
                        ));
                    }
                }
                (Role::Assistant, MessageContent::TextWithToolCalls { text, tool_calls }) => {
                    if !text.is_empty() {
                        input.push(json!({"role": "assistant", "content": text}));
                    }
                    for call in tool_calls {
                        input.push(function_call_item(
                            &call.id,
                            &call.function.name,
                            &call.function.arguments,
                        ));
                    }
                }
                (role, content) => {
                    let text = message_text(content);
                    let role = match role {
                        Role::Assistant => "assistant",
                        _ => "user",
                    };
                    input.push(json!({"role": role, "content": text}));
                }
            }
        }

        let mut body = Map::new();
        body.insert("model".into(), json!(request.model));
        body.insert("input".into(), Value::Array(input));
        // 网关持有状态：上游不存也不引用 previous_response_id
        body.insert("store".into(), json!(false));
        if let Some(instructions) = instructions {
            body.insert("instructions".into(), json!(instructions));
        }
        if let Some(max) = request.max_tokens {
            body.insert("max_output_tokens".into(), json!(max));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".into(), json!(top_p));
        }
        if request.stream {
            body.insert("stream".into(), json!(true));
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(tool_to_responses).collect();
            body.insert("tools".into(), Value::Array(tools));
        }
        if let Some(tool_choice) = request.tool_choice.as_ref() {
            body.insert("tool_choice".into(), tool_choice_to_responses(tool_choice));
        }
        if let Some(parallel) = request.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(parallel));
        }
        if let Some(format) = request.response_format.as_ref() {
            body.insert(
                "text".into(),
                json!({"format": response_format_to_text(format)}),
            );
        }
        if let Some(effort) = request.effort {
            body.insert("reasoning".into(), json!({"effort": effort.to_wire()}));
        }
        if let Some(user) = request.user {
            body.insert("user".into(), json!(user));
        }
        if let Some(metadata) = request.vendor_extensions.get("metadata") {
            body.insert("metadata".into(), metadata.clone());
        }
        Ok(Value::Object(body))
    }

    /// IR → Responses 响应对象（id 由调用方在入站 handler 生成；此实现
    /// 供直接调用方使用 `resp_<uuid>`）。
    fn response_to_external(&self, response: UnifiedResponse) -> Result<Value, GateError> {
        let request = ResponsesRequest {
            model: response.model.clone(),
            ..Default::default()
        };
        let id = format!("resp_{}", uuid::Uuid::new_v4());
        let built = to_responses_response(&id, &response, &request, true);
        Ok(serde_json::to_value(built).unwrap_or(Value::Null))
    }

    /// Responses 响应 → IR（出站 chat 客户端消费）。
    fn parse_response(&self, response: Value) -> Result<UnifiedResponse, GateError> {
        let parsed: ResponsesResponse =
            serde_json::from_value(response).map_err(|error| GateError::Protocol {
                source: format!("responses response: {error}"),
                inner: None,
            })?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for item in &parsed.output {
            match item {
                OutputItem::Message { content, .. } => {
                    for part in content {
                        match part {
                            OutputContent::OutputText { text: part, .. } => text.push_str(part),
                        }
                    }
                }
                OutputItem::FunctionCall {
                    id,
                    call_id,
                    name,
                    arguments,
                    ..
                } => tool_calls.push(crate::protocol::unified::ToolCall {
                    id: if call_id.is_empty() {
                        id.clone()
                    } else {
                        call_id.clone()
                    },
                    call_type: "function".to_string(),
                    function: crate::protocol::unified::ToolCallFunction {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                }),
                OutputItem::Reasoning { .. } => {}
            }
        }

        let has_tools = !tool_calls.is_empty();
        let content = match (text.is_empty(), has_tools) {
            (false, false) => MessageContent::Text(text),
            (true, true) => MessageContent::ToolCalls(tool_calls),
            (false, true) => MessageContent::TextWithToolCalls { text, tool_calls },
            (true, false) => MessageContent::Text(String::new()),
        };
        let finish = match parsed.status.as_str() {
            "incomplete" => match parsed
                .incomplete_details
                .as_ref()
                .map(|details| details.reason.as_str())
            {
                Some("max_output_tokens") => FinishReason::Length,
                Some("content_filter") => FinishReason::ContentFilter,
                _ => FinishReason::Stop,
            },
            _ if has_tools => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        };

        Ok(UnifiedResponse {
            id: parsed.id.clone(),
            object: "chat.completion".to_string(),
            created: parsed.created_at,
            model: parsed.model.clone(),
            choices: vec![crate::protocol::unified::Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content,
                }),
                finish_reason: Some(finish.to_wire().to_string()),
                finish: Some(finish),
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: parsed.usage.input_tokens,
                completion_tokens: parsed.usage.output_tokens,
                total_tokens: parsed.usage.total_tokens,
                cache_read_tokens: parsed
                    .usage
                    .input_tokens_details
                    .as_ref()
                    .map(|details| details.cached_tokens as u64)
                    .unwrap_or(0),
                cache_creation_tokens: 0,
                cost_usd: None,
                cost_cny: None,
            },
        })
    }

    /// 单 chunk → 单事件（入站 handler 用专用编码器，此实现仅供 trait 完整性）。
    fn stream_chunk_to_external(
        &self,
        chunk: crate::protocol::StreamChunk,
    ) -> Result<Value, GateError> {
        Ok(match chunk {
            crate::protocol::StreamChunk::Content { delta, .. } => json!({
                "type": "response.output_text.delta",
                "delta": message_text(&delta),
            }),
            crate::protocol::StreamChunk::ToolCall { id, arguments, .. } => json!({
                "type": "response.function_call_arguments.delta",
                "item_id": id,
                "delta": arguments.unwrap_or_default(),
            }),
            crate::protocol::StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => json!({
                "type": "response.completed",
                "response": {
                    "usage": {
                        "input_tokens": prompt_tokens,
                        "output_tokens": completion_tokens,
                        "total_tokens": total_tokens,
                    }
                }
            }),
            crate::protocol::StreamChunk::Heartbeat => json!({"type": "response.in_progress"}),
        })
    }

    fn parse_stream_frame(&self, data: &str) -> Option<crate::protocol::StreamChunk> {
        parse_stream_frame(data)
    }
}

fn function_call_item(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

fn message_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::MultiModal(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                crate::protocol::unified::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        MessageContent::ToolResult(result) => result.content.clone(),
        MessageContent::ToolCalls(_) => String::new(),
        MessageContent::TextWithToolCalls { text, .. } => text.clone(),
    }
}

fn tool_to_responses(tool: &Tool) -> Value {
    let mut value = json!({
        "type": "function",
        "name": tool.function.name,
        "description": tool.function.description,
    });
    if tool.function.parameters.is_object() {
        value["parameters"] = tool.function.parameters.clone();
    }
    if let Some(strict) = tool.function.strict {
        value["strict"] = json!(strict);
    }
    value
}

fn tool_choice_to_responses(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Strategy(strategy) => json!(strategy),
        ToolChoice::Specific { function, .. } => json!({
            "type": "function",
            "name": function.name,
        }),
    }
}

/// OpenAI chat `response_format` → Responses `text.format`。
fn response_format_to_text(format: &Value) -> Value {
    if format.get("type").and_then(Value::as_str) == Some("json_schema") {
        let schema = format.get("json_schema").cloned().unwrap_or_default();
        let mut text = json!({"type": "json_schema"});
        if let Some(name) = schema.get("name") {
            text["name"] = name.clone();
        }
        if let Some(inner) = schema.get("schema") {
            text["schema"] = inner.clone();
        }
        if let Some(strict) = schema.get("strict") {
            text["strict"] = strict.clone();
        }
        return text;
    }
    format.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::unified::{
        EffortLevel, ToolCall, ToolCallFunction, ToolFunction, ToolResult,
    };

    fn request() -> UnifiedRequest {
        UnifiedRequest {
            model: "gpt-4.1".into(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: MessageContent::Text("be brief".into()),
                },
                Message {
                    role: Role::User,
                    content: MessageContent::Text("weather?".into()),
                },
                Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCalls(vec![ToolCall {
                        id: "call_1".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: "get_weather".into(),
                            arguments: "{\"city\":\"bj\"}".into(),
                        },
                    }]),
                },
                Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult(ToolResult {
                        tool_call_id: "call_1".into(),
                        function_name: "get_weather".into(),
                        content: "sunny".into(),
                        is_error: false,
                    }),
                },
            ],
            max_tokens: Some(32),
            tools: vec![Tool {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "get_weather".into(),
                    description: "d".into(),
                    parameters: json!({"type": "object"}),
                    strict: Some(true),
                },
            }],
            response_format: Some(json!({
                "type": "json_schema",
                "json_schema": {"name": "answer", "schema": {"type": "object"}, "strict": true}
            })),
            parallel_tool_calls: Some(false),
            effort: Some(EffortLevel::High),
            ..Default::default()
        }
    }

    #[test]
    fn chat_request_maps_to_responses_wire() {
        let converter = ResponsesConverter::new();
        let body = converter.request_to_external(request()).expect("出站转换");
        assert_eq!(body["model"], "gpt-4.1");
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false, "网关持有状态");
        assert_eq!(body["max_output_tokens"], 32);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["strict"], true);
        assert!(
            body["tools"][0].get("function").is_none(),
            "Responses 为扁平工具"
        );
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["name"], "answer");
        let input = body["input"].as_array().expect("input 数组");
        assert!(input
            .iter()
            .any(|item| item["type"] == "function_call" && item["call_id"] == "call_1"));
        assert!(input
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "call_1"
                && item["output"] == "sunny"));
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn responses_response_parses_to_ir() {
        let converter = ResponsesConverter::new();
        let response = json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 10,
            "status": "completed",
            "model": "gpt-4.1",
            "output": [
                {"type": "message", "id": "msg_1", "status": "completed", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hi ", "annotations": []},
                             {"type": "output_text", "text": "there", "annotations": []}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_7", "name": "f",
                 "arguments": "{}", "status": "completed"}
            ],
            "usage": {"input_tokens": 4, "output_tokens": 6, "total_tokens": 10,
                      "input_tokens_details": {"cached_tokens": 2}}
        });
        let unified = converter.parse_response(response).expect("响应解析");
        assert_eq!(unified.id, "resp_1");
        assert_eq!(unified.usage.prompt_tokens, 4);
        assert_eq!(unified.usage.cache_read_tokens, 2);
        let choice = &unified.choices[0];
        assert_eq!(choice.finish, Some(FinishReason::ToolCalls));
        match &choice.message.as_ref().unwrap().content {
            MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "hi there");
                assert_eq!(tool_calls[0].id, "call_7");
            }
            other => panic!("期望文本+工具: {other:?}"),
        }
    }

    #[test]
    fn missing_output_is_protocol_error() {
        let converter = ResponsesConverter::new();
        let error = converter
            .parse_response(json!({
                "id": "resp_bad",
                "object": "response",
                "created_at": 1,
                "status": "completed",
                "model": "m"
            }))
            .expect_err("缺 output 必须拒绝");
        assert!(
            matches!(error, GateError::Protocol { .. }),
            "必须映射 502 protocol_error: {error:?}"
        );
    }

    #[test]
    fn incomplete_response_maps_to_length() {
        let converter = ResponsesConverter::new();
        let unified = converter
            .parse_response(json!({
                "id": "resp_2",
                "object": "response",
                "created_at": 1,
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "model": "m",
                "output": [],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }))
            .expect("响应解析");
        assert_eq!(
            unified.choices[0].finish,
            Some(FinishReason::Length),
            "incomplete → Length"
        );
    }
}
