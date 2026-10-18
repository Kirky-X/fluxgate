// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! IR ↔ Gemini：出站 `GenerateContentRequest` 构造、上游响应反解与入站响应编码。

use super::schema::sanitize_schema;
use super::types::{normalize_model, parse_response, Candidate, GEMINI_KNOWN_KEYS};
use crate::error::GateError;
use crate::i18n::t_args;
use crate::protocol::unified::{
    merge_vendor_extensions, Choice, ContentBlock, EffortLevel, FinishReason, ImageSource, Message,
    MessageContent, Role, ThinkingConfig, ToolChoice, UnifiedRequest, UnifiedResponse,
};
use serde_json::{json, Map, Value};

/// IR → Gemini 请求体（model 在 URL，不在 body；原生直通路径不走本函数）。
pub fn request_to_gemini(request: &UnifiedRequest) -> Result<Value, GateError> {
    let system = system_text(request);
    let tool_names = tool_call_names(&request.messages);

    let mut contents = Vec::new();
    let mut index = 0;
    while index < request.messages.len() {
        let message = &request.messages[index];
        if message.role == Role::System {
            index += 1;
            continue;
        }
        if message.role == Role::Tool {
            // 连续 tool 结果合并为一条 user content 的多个 functionResponse
            let mut parts = Vec::new();
            while index < request.messages.len() && request.messages[index].role == Role::Tool {
                if let MessageContent::ToolResult(result) = &request.messages[index].content {
                    let name = if !result.function_name.trim().is_empty() {
                        result.function_name.clone()
                    } else {
                        tool_names
                            .get(&result.tool_call_id)
                            .cloned()
                            .ok_or_else(|| {
                                validation("api-gemini-function-response-name-missing")
                            })?
                    };
                    parts.push(json!({
                        "functionResponse": {
                            "name": name,
                            "response": function_response_payload(result),
                        }
                    }));
                }
                index += 1;
            }
            if !parts.is_empty() {
                contents.push(json!({"role": "user", "parts": parts}));
            }
            continue;
        }

        let role = if message.role == Role::Assistant {
            "model"
        } else {
            "user"
        };
        let parts = message_parts(&message.content);
        if !parts.is_empty() {
            contents.push(json!({"role": role, "parts": parts}));
        }
        index += 1;
    }

    if contents.is_empty() {
        return Err(validation("api-gemini-contents-invalid"));
    }

    let mut body = json!({"contents": contents});
    if let Some(system) = system {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    if !request.tools.is_empty() {
        let mut declarations = Vec::new();
        for tool in &request.tools {
            let mut declaration = json!({"name": tool.function.name});
            if !tool.function.description.is_empty() {
                declaration["description"] = json!(tool.function.description);
            }
            if !tool.function.parameters.is_null() {
                declaration["parameters"] = sanitize_schema(&tool.function.parameters)?;
            }
            declarations.push(declaration);
        }
        body["tools"] = json!([{"functionDeclarations": declarations}]);
        if let Some(choice) = tool_choice_to_gemini(&request.tool_choice) {
            body["toolConfig"] = choice;
        }
    }

    let generation = generation_config(request)?;
    if !generation.is_empty() {
        body["generationConfig"] = Value::Object(generation);
    }

    merge_vendor_extensions(&mut body, &request.vendor_extensions, GEMINI_KNOWN_KEYS);
    Ok(body)
}

/// Gemini 上游响应 → IR。
pub fn parse_gemini_response(
    value: &Value,
    fallback_model: &str,
) -> Result<UnifiedResponse, GateError> {
    let parsed = parse_response(value).ok_or_else(|| GateError::Protocol {
        source: t_args("api-gemini-response-invalid", &[]),
        inner: None,
    })?;

    let (message, finish_raw) = match parsed.candidates.first() {
        Some(candidate) => (
            candidate_message(candidate)?,
            candidate.finish_reason.clone(),
        ),
        None => {
            if parsed.prompt_feedback.is_none() {
                return Err(GateError::Protocol {
                    source: t_args("api-gemini-response-invalid", &[]),
                    inner: None,
                });
            }
            // 阻断提示：下游协议统一看到 content_filter + 空消息
            (
                Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(String::new()),
                },
                Some("SAFETY".to_string()),
            )
        }
    };

    let (finish, finish_reason) = map_finish(finish_raw.as_deref());
    let usage = parsed.usage_metadata.unwrap_or_default();
    let completion = usage
        .candidates_token_count
        .saturating_add(usage.thoughts_token_count);
    let total = if usage.total_token_count > 0 {
        usage.total_token_count
    } else {
        usage.prompt_token_count.saturating_add(completion)
    };
    Ok(UnifiedResponse {
        id: parsed
            .response_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4())),
        object: "chat.completion".into(),
        created: chrono::Utc::now().timestamp() as u64,
        model: parsed
            .model_version
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| normalize_model(fallback_model).to_string()),
        choices: vec![Choice {
            index: 0,
            message: Some(message),
            finish_reason,
            finish,
            logprobs: None,
        }],
        usage: crate::protocol::TokenUsage {
            prompt_tokens: clamp_u32(usage.prompt_token_count),
            completion_tokens: clamp_u32(completion),
            total_tokens: clamp_u32(total),
            cache_read_tokens: usage.cached_content_token_count,
            cache_creation_tokens: 0,
            cost_usd: None,
            cost_cny: None,
        },
    })
}

/// IR → Gemini 响应（入站编码）。
pub fn response_to_gemini(response: &UnifiedResponse) -> Value {
    let choice = response.choices.first();
    let (parts, finish) = match choice.and_then(|c| c.message.as_ref()) {
        Some(message) => (
            message_parts(&message.content),
            choice.and_then(|c| {
                c.finish
                    .or_else(|| c.finish_reason.as_deref().and_then(FinishReason::from_wire))
            }),
        ),
        None => (Vec::new(), None),
    };
    let mut usage = json!({
        "promptTokenCount": response.usage.prompt_tokens,
        "candidatesTokenCount": response.usage.completion_tokens,
        "totalTokenCount": response.usage.total_tokens,
    });
    if response.usage.cache_read_tokens > 0 {
        usage["cachedContentTokenCount"] = json!(response.usage.cache_read_tokens);
    }
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": finish_to_gemini(finish),
            "index": 0,
        }],
        "usageMetadata": usage,
        "modelVersion": response.model,
        "responseId": response.id,
    })
}

fn system_text(request: &UnifiedRequest) -> Option<String> {
    let mut parts: Vec<String> = request
        .messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| text_of(&message.content))
        .collect();
    if let Some(prompt) = request.system_prompt.as_ref().filter(|s| !s.is_empty()) {
        parts.retain(|text| text != prompt);
        parts.insert(0, prompt.clone());
    }
    let joined = parts.join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

fn text_of(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(text) => Some(text.clone()),
        MessageContent::MultiModal(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn tool_call_names(messages: &[Message]) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    for message in messages {
        if let MessageContent::ToolCalls(calls)
        | MessageContent::TextWithToolCalls {
            tool_calls: calls, ..
        } = &message.content
        {
            for call in calls {
                names.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }
    names
}

fn message_parts(content: &MessageContent) -> Vec<Value> {
    match content {
        MessageContent::Text(text) => vec![json!({"text": text})],
        MessageContent::MultiModal(blocks) => blocks.iter().map(block_to_part).collect(),
        MessageContent::ToolCalls(calls) => calls.iter().map(call_to_part).collect(),
        MessageContent::TextWithToolCalls { text, tool_calls } => {
            let mut parts = vec![json!({"text": text})];
            parts.extend(tool_calls.iter().map(call_to_part));
            parts
        }
        MessageContent::ToolResult(result) => vec![json!({
            "functionResponse": {
                "name": result.function_name,
                "response": function_response_payload(result),
            }
        })],
    }
}

fn block_to_part(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"text": text}),
        ContentBlock::Image { source, .. } => match source {
            ImageSource::Base64 { media_type, data } => json!({
                "inlineData": {"mimeType": media_type, "data": data}
            }),
            ImageSource::Url { url } => {
                let mut file = json!({"fileUri": url});
                if let Some(mime) = mime_from_uri(url) {
                    file["mimeType"] = json!(mime);
                }
                json!({"fileData": file})
            }
        },
        // 其它块类型无 Gemini 对应形状：降级为文本占位防丢
        other => json!({"text": format!("{other:?}")}),
    }
}

/// 从 URI 推断常见图片/文档 MIME（Gemini `fileData.mimeType` 可选；
/// 无法推断时省略，由上游按扩展名处理）。
fn mime_from_uri(uri: &str) -> Option<&'static str> {
    let path = uri.split('?').next().unwrap_or(uri).to_ascii_lowercase();
    if path.ends_with(".png") {
        Some("image/png")
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if path.ends_with(".webp") {
        Some("image/webp")
    } else if path.ends_with(".gif") {
        Some("image/gif")
    } else if path.ends_with(".pdf") {
        Some("application/pdf")
    } else {
        None
    }
}

fn call_to_part(call: &crate::protocol::ToolCall) -> Value {
    let args = serde_json::from_str::<Value>(&call.function.arguments)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"raw": call.function.arguments}));
    json!({"functionCall": {"name": call.function.name, "args": args}})
}

fn function_response_payload(result: &crate::protocol::ToolResult) -> Value {
    let parsed = serde_json::from_str::<Value>(&result.content).ok();
    let payload = match parsed {
        Some(value) if value.is_object() => value,
        _ => json!({"result": result.content}),
    };
    if result.is_error {
        json!({"error": payload})
    } else {
        payload
    }
}

fn tool_choice_to_gemini(choice: &Option<ToolChoice>) -> Option<Value> {
    let choice = choice.as_ref()?;
    let function_call = match choice {
        ToolChoice::Strategy(strategy) => match strategy.as_str() {
            "auto" => json!({"mode": "AUTO"}),
            "none" => json!({"mode": "NONE"}),
            "required" => json!({"mode": "ANY"}),
            _ => return None,
        },
        ToolChoice::Specific { function, .. } => json!({
            "mode": "ANY",
            "allowedFunctionNames": [function.name],
        }),
    };
    Some(json!({"functionCallingConfig": function_call}))
}

fn generation_config(request: &UnifiedRequest) -> Result<Map<String, Value>, GateError> {
    let mut out = Map::new();
    let top_p = request
        .top_p
        .or_else(|| request.sampling.as_ref().and_then(|s| s.top_p));
    let top_k = request.sampling.as_ref().and_then(|s| s.top_k);
    let frequency = request
        .frequency_penalty
        .or_else(|| request.sampling.as_ref().and_then(|s| s.frequency_penalty));
    let presence = request
        .presence_penalty
        .or_else(|| request.sampling.as_ref().and_then(|s| s.presence_penalty));

    if let Some(value) = request.temperature {
        out.insert("temperature".into(), json!(value));
    }
    if let Some(value) = top_p {
        out.insert("topP".into(), json!(value));
    }
    if let Some(value) = top_k {
        out.insert("topK".into(), json!(value));
    }
    if let Some(value) = request.max_tokens {
        out.insert("maxOutputTokens".into(), json!(value));
    }
    if !request.stop.is_empty() {
        out.insert("stopSequences".into(), json!(request.stop));
    }
    if let Some(value) = frequency {
        out.insert("frequencyPenalty".into(), json!(value));
    }
    if let Some(value) = presence {
        out.insert("presencePenalty".into(), json!(value));
    }

    if let Some(format) = &request.response_format {
        match format.get("type").and_then(Value::as_str) {
            Some("json_schema") => {
                out.insert("responseMimeType".into(), json!("application/json"));
                if let Some(schema) = format
                    .get("json_schema")
                    .and_then(|s| s.get("schema"))
                    .or_else(|| format.get("schema"))
                {
                    out.insert("responseSchema".into(), sanitize_schema(schema)?);
                }
            }
            Some("json_object") => {
                out.insert("responseMimeType".into(), json!("application/json"));
            }
            _ => {}
        }
    }

    let thinking = thinking_config(request);
    if let Some(thinking) = thinking {
        out.insert("thinkingConfig".into(), thinking);
    }
    Ok(out)
}

fn thinking_config(request: &UnifiedRequest) -> Option<Value> {
    let mut thinking = Map::new();
    match request.thinking_config {
        Some(ThinkingConfig::Disabled) => {
            thinking.insert("thinkingBudget".into(), json!(0));
        }
        Some(ThinkingConfig::Adaptive) => {
            thinking.insert("thinkingBudget".into(), json!(-1));
        }
        Some(ThinkingConfig::Enabled { budget_tokens }) => {
            thinking.insert(
                "thinkingBudget".into(),
                json!(budget_tokens.min(i32::MAX as u64)),
            );
        }
        None => {
            if let Some(effort) = request.effort {
                let budget = match effort {
                    EffortLevel::Low => 1024,
                    EffortLevel::Medium => 8192,
                    EffortLevel::High => 24576,
                };
                thinking.insert("thinkingBudget".into(), json!(budget));
            }
        }
    }
    if let Some(include) = request.thinking_include_thoughts {
        thinking.insert("includeThoughts".into(), json!(include));
    }
    (!thinking.is_empty()).then_some(Value::Object(thinking))
}

fn candidate_message(candidate: &Candidate) -> Result<Message, GateError> {
    let Some(content) = candidate.content.as_ref() else {
        return Ok(Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
        });
    };
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut calls = Vec::new();
    for part in &content.parts {
        if let Some(chunk) = part.text.as_deref() {
            if !part.thought {
                text.push_str(chunk);
            }
        } else if let Some(blob) = part.inline_data.as_ref() {
            blocks.push(ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: blob.mime_type.clone(),
                    data: blob.data.clone(),
                },
                detail: None,
            });
        } else if let Some(file) = part.file_data.as_ref() {
            blocks.push(ContentBlock::Image {
                source: ImageSource::Url {
                    url: file.file_uri.clone(),
                },
                detail: None,
            });
        } else if let Some(call) = part.function_call.as_ref() {
            calls.push(crate::protocol::ToolCall {
                id: format!("call_{}", uuid::Uuid::new_v4()),
                call_type: "function".into(),
                function: crate::protocol::ToolCallFunction {
                    name: call.name.clone(),
                    arguments: call
                        .args
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".into()),
                },
            });
        }
    }
    Ok(Message {
        role: Role::Assistant,
        content: compose_content(text, blocks, calls),
    })
}

fn compose_content(
    text: String,
    blocks: Vec<ContentBlock>,
    calls: Vec<crate::protocol::ToolCall>,
) -> MessageContent {
    let has_text = !text.is_empty();
    let has_blocks = !blocks.is_empty();
    let has_calls = !calls.is_empty();
    match (has_text, has_blocks, has_calls) {
        (false, false, false) => MessageContent::Text(String::new()),
        (true, false, false) => MessageContent::Text(text),
        (false, true, false) => MessageContent::MultiModal(blocks),
        (true, true, false) => {
            let mut all = vec![ContentBlock::Text { text }];
            all.extend(blocks);
            MessageContent::MultiModal(all)
        }
        (_, _, true) => MessageContent::TextWithToolCalls {
            text,
            tool_calls: calls,
        },
    }
}

/// Gemini finishReason → IR 双轨（已知词写两槽，未知词只写原始槽）。
fn map_finish(raw: Option<&str>) -> (Option<FinishReason>, Option<String>) {
    let Some(raw) = raw else {
        return (None, None);
    };
    let (finish, wire) = match raw {
        "STOP" => (Some(FinishReason::Stop), "stop"),
        "MAX_TOKENS" => (Some(FinishReason::Length), "length"),
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "LANGUAGE" => {
            (Some(FinishReason::ContentFilter), "content_filter")
        }
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => (None, "tool_calls"),
        other => (None, other),
    };
    (finish, Some(wire.to_string()))
}

fn finish_to_gemini(finish: Option<FinishReason>) -> &'static str {
    match finish {
        Some(FinishReason::Stop) | Some(FinishReason::ToolCalls) | None => "STOP",
        Some(FinishReason::Length) => "MAX_TOKENS",
        Some(FinishReason::ContentFilter) => "SAFETY",
    }
}

fn clamp_u32(value: u64) -> u32 {
    value.min(u32::MAX as u64) as u32
}

fn validation(key: &str) -> GateError {
    GateError::Validation {
        field: t_args(key, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ToolCall, ToolCallFunction, ToolFunction, ToolResult};
    use serde_json::json;

    fn request_with(messages: Vec<Message>) -> UnifiedRequest {
        UnifiedRequest {
            model: "gemini-2.5-pro".into(),
            messages,
            ..Default::default()
        }
    }

    #[test]
    fn encodes_system_text_and_multimodal() {
        let mut request = request_with(vec![
            Message {
                role: Role::System,
                content: MessageContent::Text("sys".into()),
            },
            Message {
                role: Role::User,
                content: MessageContent::MultiModal(vec![
                    ContentBlock::Text {
                        text: "look".into(),
                    },
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: "image/png".into(),
                            data: "AAA".into(),
                        },
                        detail: None,
                    },
                ]),
            },
        ]);
        request.system_prompt = Some("sys".into());
        let body = request_to_gemini(&request).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "look");
        assert_eq!(
            body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
            "image/png"
        );
    }

    #[test]
    fn url_image_infers_mime_and_keeps_file_uri() {
        let request = request_with(vec![Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "gs://bucket/photo.PNG?x=1".into(),
                },
                detail: None,
            }]),
        }]);
        let body = request_to_gemini(&request).unwrap();
        let file = &body["contents"][0]["parts"][0]["fileData"];
        assert_eq!(file["fileUri"], "gs://bucket/photo.PNG?x=1");
        assert_eq!(file["mimeType"], "image/png");

        let unknown = request_with(vec![Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "gs://bucket/blob".into(),
                },
                detail: None,
            }]),
        }]);
        let body = request_to_gemini(&unknown).unwrap();
        assert!(body["contents"][0]["parts"][0]["fileData"]
            .get("mimeType")
            .is_none());
    }

    #[test]
    fn encodes_tools_and_tool_choice() {
        let mut request = request_with(vec![Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }]);
        request.tools = vec![crate::protocol::Tool {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "f".into(),
                description: "d".into(),
                parameters: json!({"type": "object", "additionalProperties": false}),
                strict: Some(true),
            },
        }];
        request.tool_choice = Some(ToolChoice::Specific {
            choice_type: "function".into(),
            function: crate::protocol::ToolChoiceFunction { name: "f".into() },
        });
        let body = request_to_gemini(&request).unwrap();
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "f");
        assert!(body["tools"][0]["functionDeclarations"][0]["parameters"]
            .get("additionalProperties")
            .is_none());
        assert_eq!(
            body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "f"
        );
    }

    #[test]
    fn merges_tool_results_and_resolves_name() {
        let call = ToolCall {
            id: "call-1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "weather".into(),
                arguments: "{\"city\":\"SH\"}".into(),
            },
        };
        let request = request_with(vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(vec![call]),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "call-1".into(),
                    function_name: String::new(),
                    content: "{\"temp\":20}".into(),
                    is_error: false,
                }),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "call-1".into(),
                    function_name: String::new(),
                    content: "plain".into(),
                    is_error: true,
                }),
            },
        ]);
        let body = request_to_gemini(&request).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(
            contents[1]["parts"].as_array().unwrap().len(),
            2,
            "连续 tool 结果合并为一条 user content"
        );
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            "weather"
        );
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["response"]["temp"],
            20
        );
        assert_eq!(
            contents[1]["parts"][1]["functionResponse"]["response"]["error"]["result"],
            "plain"
        );
    }

    #[test]
    fn structured_output_and_thinking_are_encoded() {
        let mut request = request_with(vec![Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }]);
        request.response_format = Some(json!({
            "type": "json_schema",
            "json_schema": {"name": "x", "schema": {"type": "object"}}
        }));
        request.thinking_config = Some(ThinkingConfig::Enabled {
            budget_tokens: 2048,
        });
        request.thinking_include_thoughts = Some(true);
        let body = request_to_gemini(&request).unwrap();
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            2048
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn effort_maps_to_budget_only_without_thinking_config() {
        let mut request = request_with(vec![Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }]);
        request.effort = Some(EffortLevel::High);
        let body = request_to_gemini(&request).unwrap();
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            24576
        );
    }

    #[test]
    fn parses_upstream_response_and_usage() {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "hi"},
                    {"functionCall": {"name": "f", "args": {"a": 1}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 3,
                "cachedContentTokenCount": 4,
                "totalTokenCount": 18
            },
            "modelVersion": "gemini-2.5-pro",
            "responseId": "r-1"
        });
        let response = parse_gemini_response(&body, "fallback").unwrap();
        assert_eq!(response.id, "r-1");
        assert_eq!(response.model, "gemini-2.5-pro");
        assert_eq!(response.usage.prompt_tokens, 10);
        assert_eq!(response.usage.completion_tokens, 8);
        assert_eq!(response.usage.cache_read_tokens, 4);
        let choice = &response.choices[0];
        assert_eq!(choice.finish, Some(FinishReason::Stop));
        assert_eq!(choice.finish_reason.as_deref(), Some("stop"));
        match &choice.message.as_ref().unwrap().content {
            MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "hi");
                assert_eq!(tool_calls[0].function.name, "f");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }
    }

    #[test]
    fn function_call_only_response_maps_tool_calls() {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "f", "args": {"a": 1}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let response = parse_gemini_response(&body, "m").unwrap();
        match &response.choices[0].message.as_ref().unwrap().content {
            MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert!(text.is_empty());
                assert_eq!(tool_calls[0].function.name, "f");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }
    }

    #[test]
    fn blocked_prompt_maps_content_filter() {
        let body = json!({
            "promptFeedback": {"blockReason": "SAFETY"},
            "usageMetadata": {"promptTokenCount": 2, "totalTokenCount": 2}
        });
        let response = parse_gemini_response(&body, "m").unwrap();
        assert_eq!(
            response.choices[0].finish,
            Some(FinishReason::ContentFilter)
        );
    }

    #[test]
    fn encodes_ir_response_back_to_gemini() {
        let response = UnifiedResponse {
            id: "resp-1".into(),
            object: "chat.completion".into(),
            created: 1,
            model: "gemini-2.5-pro".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content: MessageContent::Text("hello".into()),
                }),
                finish_reason: Some("length".into()),
                finish: Some(FinishReason::Length),
                logprobs: None,
            }],
            usage: crate::protocol::TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7,
                cache_read_tokens: 1,
                ..Default::default()
            },
        };
        let body = response_to_gemini(&response);
        assert_eq!(
            body["candidates"][0]["content"]["parts"][0]["text"],
            "hello"
        );
        assert_eq!(body["candidates"][0]["finishReason"], "MAX_TOKENS");
        assert_eq!(body["usageMetadata"]["promptTokenCount"], 3);
        assert_eq!(body["usageMetadata"]["cachedContentTokenCount"], 1);
        assert_eq!(body["responseId"], "resp-1");
    }

    #[test]
    fn missing_candidates_without_feedback_is_protocol_error() {
        assert!(parse_gemini_response(&json!({"candidates": []}), "m").is_err());
    }
}
