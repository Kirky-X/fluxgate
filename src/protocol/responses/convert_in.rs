// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Responses 请求解析与 IR 转换（纯函数，无 IO）。
//!
//! 两层入口：
//! - [`parse_request`]：wire 校验 + input item 归一（字符串 → item 数组），
//!   产出当前回合 items、native-only 判定与原始请求；
//! - [`to_unified_request`]：把（已合并历史回放的）items 转成
//!   [`UnifiedRequest`]，供现有路由池消费。
//!
//! 校验口径：必填缺一不可、越界显式拒绝不静默截断；错误一律
//! `GateError::Validation`（400）并点名字段与下标。

use crate::error::GateError;
use crate::protocol::responses::types::{
    InputMessage, ParsedInputItem, ResponsesRequest, StreamEvent, TextConfig,
    MAX_RESPONSES_INPUT_ITEMS, MAX_RESPONSES_METADATA_ENTRIES, MAX_RESPONSES_METADATA_KEY_CHARS,
    MAX_RESPONSES_METADATA_VALUE_CHARS, MAX_RESPONSES_OUTPUT_TOKENS,
};
use crate::protocol::unified::{
    validate_tool_schemas, ContentBlock, EffortLevel, ImageSource, Message, MessageContent, Role,
    SamplingParams, Tool, ToolCall, ToolCallFunction, ToolChoice, ToolChoiceFunction, ToolFunction,
    ToolResult, UnifiedRequest,
};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// 解析后的入站请求（当前回合）。
#[derive(Debug, Clone)]
pub struct ParsedResponsesRequest {
    pub request: ResponsesRequest,
    /// 归一后的当前回合 input items（字符串 input 已包成 message item）。
    /// 直接入库作为 `input_items`，历史回放时原样拼接。
    pub current_items: Vec<Value>,
    /// 只能原生直通的理由（None = 可走 IR 转换路径）。
    pub native_only: Option<String>,
}

impl ParsedResponsesRequest {
    /// 有效 `store`：缺省 true。
    pub fn store(&self) -> bool {
        self.request.store.unwrap_or(true)
    }
}

fn validation(field: impl Into<String>) -> GateError {
    GateError::Validation {
        field: field.into(),
    }
}

/// 解析并校验请求体，归一 input items。
pub fn parse_request(body: &Value) -> Result<ParsedResponsesRequest, GateError> {
    let request: ResponsesRequest = serde_json::from_value(body.clone()).map_err(|error| {
        // 缺 model 是最常见形态：点名 model；其余归为 body
        if error.to_string().contains("model") {
            validation("model")
        } else {
            validation(format!("body: {error}"))
        }
    })?;

    if request.model.trim().is_empty() {
        return Err(validation("model"));
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        if !(1..=MAX_RESPONSES_OUTPUT_TOKENS).contains(&max_output_tokens) {
            return Err(validation(format!(
                "max_output_tokens: {max_output_tokens} out of range 1..={MAX_RESPONSES_OUTPUT_TOKENS}"
            )));
        }
    }
    if let Some(metadata) = request.metadata.as_ref() {
        if metadata.len() > MAX_RESPONSES_METADATA_ENTRIES {
            return Err(validation(format!(
                "metadata: {} entries exceeds limit {MAX_RESPONSES_METADATA_ENTRIES}",
                metadata.len()
            )));
        }
        for (key, value) in metadata {
            if key.chars().count() > MAX_RESPONSES_METADATA_KEY_CHARS {
                return Err(validation(format!(
                    "metadata key {key:?} exceeds {MAX_RESPONSES_METADATA_KEY_CHARS} chars"
                )));
            }
            if value.chars().count() > MAX_RESPONSES_METADATA_VALUE_CHARS {
                return Err(validation(format!(
                    "metadata value for key {key:?} exceeds {MAX_RESPONSES_METADATA_VALUE_CHARS} chars"
                )));
            }
        }
    }
    if request.background && request.stream {
        return Err(validation(
            "stream: background mode does not support streaming",
        ));
    }

    let current_items = normalize_input(&request.input)?;
    if current_items.len() > MAX_RESPONSES_INPUT_ITEMS {
        return Err(validation(format!(
            "input: {} items exceeds limit {MAX_RESPONSES_INPUT_ITEMS}",
            current_items.len()
        )));
    }
    // 逐 item 形状校验（不产出 IR，只保证可解析；历史回放后会再校验一次）
    let _ = parse_items(&current_items)?;

    let native_only = detect_native_only(&request, &current_items);
    Ok(ParsedResponsesRequest {
        request,
        current_items,
        native_only,
    })
}

/// input → item 数组；字符串包成单条 user message。
fn normalize_input(input: &Value) -> Result<Vec<Value>, GateError> {
    match input {
        Value::String(text) => Ok(vec![serde_json::json!({
            "role": "user",
            "content": text,
        })]),
        Value::Array(items) => Ok(items.clone()),
        Value::Null => Err(validation("input")),
        _ => Err(validation("input: must be a string or an array of items")),
    }
}

/// 判定只能原生直通的能力（内置工具 / prompt 模板 / 文件与音频输入）。
fn detect_native_only(request: &ResponsesRequest, items: &[Value]) -> Option<String> {
    if request.prompt.is_some() {
        return Some("prompt".to_string());
    }
    if let Some(tools) = request.tools.as_ref() {
        for tool in tools {
            let Some(kind) = tool.get("type").and_then(Value::as_str) else {
                continue;
            };
            if kind != "function" {
                return Some(format!("tools[type={kind}]"));
            }
        }
    }
    if has_input_file_or_audio(items) {
        return Some("input_file_or_audio".to_string());
    }
    None
}

fn has_input_file_or_audio(items: &[Value]) -> bool {
    fn content_has_file(content: &Value) -> bool {
        match content {
            Value::Array(blocks) => blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("input_file") | Some("input_audio")
                )
            }),
            _ => false,
        }
    }
    items.iter().any(|item| {
        let item_type = item.get("type").and_then(Value::as_str);
        if matches!(item_type, Some("input_file") | Some("input_audio")) {
            return true;
        }
        item.get("content").is_some_and(content_has_file)
    })
}

/// input item 判别（历史回放后与当前回合共用同一套规则）。
pub fn parse_items(items: &[Value]) -> Result<Vec<ParsedInputItem>, GateError> {
    let mut parsed = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item_type = item.get("type").and_then(Value::as_str);
        match item_type {
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation(format!("input[{index}].call_id")))?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation(format!("input[{index}].name")))?;
                let arguments = item
                    .get("arguments")
                    .map(|value| match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                parsed.push(ParsedInputItem::FunctionCall(
                    crate::protocol::responses::types::InputFunctionCall {
                        call_id: call_id.to_string(),
                        name: name.to_string(),
                        arguments,
                        id: item.get("id").and_then(Value::as_str).map(str::to_string),
                    },
                ));
            }
            Some("function_call_output") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation(format!("input[{index}].call_id")))?;
                parsed.push(ParsedInputItem::FunctionCallOutput(
                    crate::protocol::responses::types::InputFunctionCallOutput {
                        call_id: call_id.to_string(),
                        output: item.get("output").cloned().unwrap_or(Value::Null),
                    },
                ));
            }
            Some("reasoning") => parsed.push(ParsedInputItem::Reasoning),
            Some("item_reference") => {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation(format!("input[{index}].id")))?;
                parsed.push(ParsedInputItem::ItemReference(id.to_string()));
            }
            Some("input_file") | Some("input_audio") => {
                return Err(validation(format!(
                    "input[{index}].type: file/audio input requires a native Responses vendor"
                )))
            }
            _ => {
                // message item：显式 type=message，或省略 type 仅带 role
                let role = item.get("role").and_then(Value::as_str).ok_or_else(|| {
                    validation(format!(
                        "input[{index}]: unrecognized item (missing role and known type)"
                    ))
                })?;
                parsed.push(ParsedInputItem::Message(InputMessage {
                    role: role.to_string(),
                    content: item.get("content").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
    Ok(parsed)
}

/// 把（已合并历史回放的）items 转成 `UnifiedRequest`。
///
/// `parsed.current_items` 仅为当前回合（用于存储），`merged_items` 为
/// 历史 + 当前（用于上游请求）。调用方须先确认 `native_only.is_none()`。
pub fn to_unified_request(
    parsed: &ParsedResponsesRequest,
    merged_items: &[Value],
) -> Result<UnifiedRequest, GateError> {
    if let Some(reason) = parsed.native_only.as_deref() {
        return Err(validation(format!(
            "request requires a native Responses vendor ({reason})"
        )));
    }
    if merged_items.len() > MAX_RESPONSES_INPUT_ITEMS {
        return Err(validation(format!(
            "input: {} items exceeds limit {MAX_RESPONSES_INPUT_ITEMS}",
            merged_items.len()
        )));
    }
    let parsed_items = parse_items(merged_items)?;
    let messages = items_to_messages(&parsed_items)?;

    let tools = parse_tools(parsed.request.tools.as_ref())?;
    validate_tool_schemas(&tools)?;
    let tool_choice = parsed.request.tool_choice.as_ref().map(map_tool_choice);
    let effort = parsed
        .request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort.as_deref())
        .and_then(EffortLevel::from_wire);
    let response_format = parsed.request.text.as_ref().and_then(map_text_format);

    let mut vendor_extensions = Map::new();
    if let Some(metadata) = parsed.request.metadata.as_ref() {
        vendor_extensions.insert(
            "metadata".to_string(),
            serde_json::to_value(metadata).unwrap_or(Value::Null),
        );
    }
    for (key, value) in [
        (
            "service_tier",
            parsed
                .request
                .service_tier
                .as_ref()
                .map(|v| Value::String(v.clone())),
        ),
        (
            "safety_identifier",
            parsed
                .request
                .safety_identifier
                .as_ref()
                .map(|v| Value::String(v.clone())),
        ),
        (
            "max_tool_calls",
            parsed
                .request
                .max_tool_calls
                .map(|v| Value::Number(v.into())),
        ),
        (
            "include",
            parsed
                .request
                .include
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok()),
        ),
        (
            "truncation",
            parsed
                .request
                .truncation
                .as_ref()
                .map(|v| Value::String(v.clone())),
        ),
    ] {
        if let Some(value) = value {
            vendor_extensions.insert(key.to_string(), value);
        }
    }

    Ok(UnifiedRequest {
        model: parsed.request.model.clone(),
        messages,
        system_prompt: parsed.request.instructions.clone(),
        temperature: parsed.request.temperature,
        max_tokens: parsed
            .request
            .max_output_tokens
            .map(|v| v.min(u32::MAX as u64) as u32),
        stream: parsed.request.stream,
        stop: Vec::new(),
        top_p: parsed.request.top_p,
        frequency_penalty: None,
        presence_penalty: None,
        logit_bias: HashMap::new(),
        user: parsed.request.user.clone(),
        sampling: Some(SamplingParams::default()),
        thinking_config: None,
        thinking_include_thoughts: None,
        effort,
        turn_index: None,
        tools,
        tool_choice,
        parallel_tool_calls: parsed.request.parallel_tool_calls,
        response_format,
        vendor_extensions: vendor_extensions.into_iter().collect(),
    })
}

/// items → IR 消息：连续 function_call 合并为一条 assistant 消息；
/// function_call_output → tool 消息；reasoning/未知跳过。
fn items_to_messages(items: &[ParsedInputItem]) -> Result<Vec<Message>, GateError> {
    let mut messages = Vec::new();
    let mut pending_calls: Vec<ToolCall> = Vec::new();
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut seen_outputs: HashSet<String> = HashSet::new();

    let flush_calls = |messages: &mut Vec<Message>, pending: &mut Vec<ToolCall>| {
        if !pending.is_empty() {
            messages.push(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(std::mem::take(pending)),
            });
        }
    };

    for item in items {
        match item {
            ParsedInputItem::Message(message) => {
                flush_calls(&mut messages, &mut pending_calls);
                let role = match message.role.as_str() {
                    "assistant" => Role::Assistant,
                    "system" | "developer" => Role::System,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                let content = content_to_ir(&message.content)?;
                messages.push(Message { role, content });
            }
            ParsedInputItem::FunctionCall(call) => {
                seen_calls.insert(call.call_id.clone());
                pending_calls.push(ToolCall {
                    id: call.call_id.clone(),
                    call_type: "function".to_string(),
                    function: ToolCallFunction {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                });
            }
            ParsedInputItem::FunctionCallOutput(output) => {
                flush_calls(&mut messages, &mut pending_calls);
                if !seen_calls.contains(&output.call_id) {
                    return Err(validation(format!(
                        "input: function_call_output call_id {:?} has no preceding function_call",
                        output.call_id
                    )));
                }
                if !seen_outputs.insert(output.call_id.clone()) {
                    return Err(validation(format!(
                        "input: duplicate function_call_output for call_id {:?}",
                        output.call_id
                    )));
                }
                messages.push(Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult(ToolResult {
                        tool_call_id: output.call_id.clone(),
                        function_name: String::new(),
                        content: output_to_text(&output.output),
                        is_error: false,
                    }),
                });
            }
            ParsedInputItem::Reasoning => {
                // chat 上游无 reasoning 明细：IR 路径丢弃（原生直通才保留）
            }
            ParsedInputItem::ItemReference(id) => {
                return Err(validation(format!(
                    "input: item_reference {id:?} must be resolved before conversion"
                )));
            }
            ParsedInputItem::Unknown(_) => {}
        }
    }
    flush_calls(&mut messages, &mut pending_calls);
    Ok(messages)
}

/// content（字符串或 content blocks）→ IR `MessageContent`。
fn content_to_ir(content: &Value) -> Result<MessageContent, GateError> {
    match content {
        Value::String(text) => Ok(MessageContent::Text(text.clone())),
        Value::Array(blocks) => {
            let mut parts: Vec<ContentBlock> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            parts.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("input_image") => {
                        let url = block
                            .get("image_url")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let source = image_source(url);
                        parts.push(ContentBlock::Image {
                            source,
                            detail: block
                                .get("detail")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        });
                    }
                    Some(other) => {
                        return Err(validation(format!(
                            "input content block type {other:?} is not supported on chat vendors"
                        )))
                    }
                    None => {}
                }
            }
            if parts.is_empty() {
                Ok(MessageContent::Text(String::new()))
            } else if parts.len() == 1 && matches!(parts[0], ContentBlock::Text { .. }) {
                match parts.remove(0) {
                    ContentBlock::Text { text } => Ok(MessageContent::Text(text)),
                    _ => unreachable!(),
                }
            } else {
                Ok(MessageContent::MultiModal(parts))
            }
        }
        Value::Null => Ok(MessageContent::Text(String::new())),
        other => Ok(MessageContent::Text(other.to_string())),
    }
}

/// data URL → Base64；其余 → URL。
fn image_source(url: &str) -> ImageSource {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(',') {
            if let Some(media_type) = meta.strip_suffix(";base64") {
                return ImageSource::Base64 {
                    media_type: media_type.to_string(),
                    data: data.to_string(),
                };
            }
        }
    }
    ImageSource::Url {
        url: url.to_string(),
    }
}

/// function_call_output → 文本（字符串原样；content 数组拼接文本块）。
fn output_to_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| block.as_str().map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// flat / nested function tools → IR `Tool`；非 function 已在 native_only 拦截。
fn parse_tools(tools: Option<&Vec<Value>>) -> Result<Vec<Tool>, GateError> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };
    let mut parsed = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(validation(format!(
                "tools[{index}]: non-function tool requires a native Responses vendor"
            )));
        }
        // Responses flat 形态；兼容 chat nested 形态
        let (name, description, parameters, strict) = if let Some(function) = tool.get("function") {
            (
                function.get("name").and_then(Value::as_str),
                function.get("description").and_then(Value::as_str),
                function.get("parameters"),
                function.get("strict").and_then(Value::as_bool),
            )
        } else {
            (
                tool.get("name").and_then(Value::as_str),
                tool.get("description").and_then(Value::as_str),
                tool.get("parameters"),
                tool.get("strict").and_then(Value::as_bool),
            )
        };
        let name = name.ok_or_else(|| validation(format!("tools[{index}].name")))?;
        parsed.push(Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: name.to_string(),
                description: description.unwrap_or("").to_string(),
                parameters: parameters.cloned().unwrap_or(Value::Null),
                strict,
            },
        });
    }
    Ok(parsed)
}

fn map_tool_choice(value: &Value) -> ToolChoice {
    match value {
        Value::String(strategy) => ToolChoice::Strategy(strategy.clone()),
        Value::Object(object) => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| {
                    object
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            ToolChoice::Specific {
                choice_type: object
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("function")
                    .to_string(),
                function: ToolChoiceFunction {
                    name: name.to_string(),
                },
            }
        }
        _ => ToolChoice::Strategy("auto".to_string()),
    }
}

/// Responses `text.format` → OpenAI chat `response_format`（json_schema 需
/// 包一层 `json_schema` 对象；其余原样）。
fn map_text_format(text: &TextConfig) -> Option<Value> {
    let format = text.format.as_ref()?;
    match format.get("type").and_then(Value::as_str) {
        Some("json_schema") => {
            let mut schema = Map::new();
            if let Some(name) = format.get("name") {
                schema.insert("name".to_string(), name.clone());
            }
            if let Some(inner) = format.get("schema") {
                schema.insert("schema".to_string(), inner.clone());
            }
            if let Some(strict) = format.get("strict") {
                schema.insert("strict".to_string(), strict.clone());
            }
            Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": Value::Object(schema),
            }))
        }
        _ => Some(format.clone()),
    }
}

/// 从流式事件中提取文本增量（入站编码器与出站解析共用的便捷函数）。
pub fn event_text_delta(event: &StreamEvent) -> Option<&str> {
    match event.event_type.as_str() {
        "response.output_text.delta" => event.delta.as_deref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(body: Value) -> ParsedResponsesRequest {
        parse_request(&body).expect("请求解析")
    }

    #[test]
    fn string_input_becomes_user_message() {
        let parsed = parse(json!({"model": "m", "input": "hello", "max_output_tokens": 16}));
        assert_eq!(parsed.current_items.len(), 1);
        let request = to_unified_request(&parsed, &parsed.current_items).expect("IR 转换");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, Role::User);
        match &request.messages[0].content {
            MessageContent::Text(text) => assert_eq!(text, "hello"),
            other => panic!("期望文本消息: {other:?}"),
        }
    }

    #[test]
    fn function_call_history_maps_to_tool_calls_and_outputs() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": [
                {"role": "user", "content": "weather?"},
                {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"bj\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "sunny"}
            ]
        }));
        let request = to_unified_request(&parsed, &parsed.current_items).expect("IR 转换");
        assert_eq!(request.messages.len(), 3);
        assert!(matches!(
            &request.messages[1].content,
            MessageContent::ToolCalls(calls) if calls[0].id == "call_1"
        ));
        assert!(matches!(
            &request.messages[2].content,
            MessageContent::ToolResult(result) if result.tool_call_id == "call_1" && result.content == "sunny"
        ));
    }

    #[test]
    fn orphan_function_call_output_is_rejected() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": [
                {"role": "user", "content": "x"},
                {"type": "function_call_output", "call_id": "missing", "output": "y"}
            ]
        }));
        let error =
            to_unified_request(&parsed, &parsed.current_items).expect_err("孤儿输出必须拒绝");
        assert!(error.to_string().contains("missing"), "{error}");
    }

    #[test]
    fn tools_text_format_and_effort_map_to_ir() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": "x",
            "instructions": "be brief",
            "tools": [{
                "type": "function",
                "name": "f",
                "description": "d",
                "parameters": {"type": "object"},
                "strict": true
            }],
            "tool_choice": {"type": "function", "name": "f"},
            "text": {"format": {"type": "json_schema", "name": "answer", "schema": {"type": "object"}, "strict": true}},
            "reasoning": {"effort": "high"},
            "parallel_tool_calls": false
        }));
        let request = to_unified_request(&parsed, &parsed.current_items).expect("IR 转换");
        assert_eq!(request.system_prompt.as_deref(), Some("be brief"));
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].function.strict, Some(true));
        assert!(matches!(
            request.tool_choice,
            Some(ToolChoice::Specific { .. })
        ));
        assert_eq!(request.effort, Some(EffortLevel::High));
        assert_eq!(request.parallel_tool_calls, Some(false));
        let format = request.response_format.expect("response_format");
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["json_schema"]["name"], "answer");
    }

    #[test]
    fn builtin_tools_mark_native_only() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": "x",
            "tools": [{"type": "web_search"}]
        }));
        assert!(parsed
            .native_only
            .as_deref()
            .unwrap()
            .contains("web_search"));
    }

    #[test]
    fn input_file_marks_native_only() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": [{"role": "user", "content": [{"type": "input_file", "file_id": "f_1"}]}]
        }));
        assert_eq!(parsed.native_only.as_deref(), Some("input_file_or_audio"));
    }

    #[test]
    fn limits_and_metadata_are_validated() {
        let missing_max = parse_request(&json!({"model": "m", "input": "x"}));
        assert!(missing_max.is_ok(), "max_output_tokens 可缺省（上游默认）");

        let out_of_range = parse_request(&json!({
            "model": "m",
            "input": "x",
            "max_output_tokens": 0
        }));
        assert!(out_of_range.is_err(), "max_output_tokens=0 必须拒绝");

        let metadata = parse_request(&json!({
            "model": "m",
            "input": "x",
            "max_output_tokens": 16,
            "metadata": (0..17).map(|i| (format!("k{i}"), "v".to_string())).collect::<std::collections::BTreeMap<_,_>>()
        }));
        assert!(metadata.is_err(), "metadata 超过 16 项必须拒绝");

        let background_stream = parse_request(&json!({
            "model": "m",
            "input": "x",
            "max_output_tokens": 16,
            "background": true,
            "stream": true
        }));
        assert!(background_stream.is_err(), "background+stream 必须拒绝");
    }

    #[test]
    fn image_data_url_splits_base64() {
        let parsed = parse(json!({
            "model": "m",
            "max_output_tokens": 16,
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "low"}
            ]}]
        }));
        let request = to_unified_request(&parsed, &parsed.current_items).expect("IR 转换");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert!(matches!(blocks[0], ContentBlock::Text { .. }));
                match &blocks[1] {
                    ContentBlock::Image { source, detail } => {
                        assert!(
                            matches!(source, ImageSource::Base64 { media_type, .. } if media_type == "image/png")
                        );
                        assert_eq!(detail.as_deref(), Some("low"));
                    }
                    other => panic!("期望图片块: {other:?}"),
                }
            }
            other => panic!("期望多模态: {other:?}"),
        }
    }
}
