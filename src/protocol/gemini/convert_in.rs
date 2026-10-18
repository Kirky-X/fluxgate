// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Gemini `GenerateContentRequest` → `UnifiedRequest`。
//!
//! 入站只做「语法 → IR 语义」：Gemini 专有且不可降级的顶层键（`safetySettings`
//! /`cachedContent`/内置工具）由 API 层提前判定原生直通，不进入本模块。

use super::types::{normalize_model, FunctionCall, FunctionResponse, Part};
use crate::error::GateError;
use crate::i18n::t_args;
use crate::protocol::unified::{
    absorb_vendor_extensions, validate_tool_schemas, ContentBlock, ImageSource, Message,
    MessageContent, Role, SamplingParams, ThinkingConfig, Tool, ToolCall, ToolCallFunction,
    ToolChoice, ToolChoiceFunction, ToolFunction, ToolResult, UnifiedRequest,
};
use serde_json::Value;
use std::collections::HashMap;

/// 入站解析：`model` 来自 path，`stream` 来自 action。
pub fn to_unified_request(
    body: &Value,
    model: &str,
    stream: bool,
) -> Result<UnifiedRequest, GateError> {
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| validation("api-gemini-contents-invalid"))?;

    let mut messages = Vec::new();
    for content in contents {
        messages.extend(parse_content(content)?);
    }
    if messages.is_empty() {
        return Err(validation("api-gemini-contents-invalid"));
    }

    let tools = parse_tools(body)?;
    validate_tool_schemas(&tools)?;
    let tool_choice = parse_tool_choice(body);
    let generation = generation_config(body);

    Ok(UnifiedRequest {
        model: normalize_model(model).to_string(),
        messages,
        system_prompt: parse_system_instruction(body),
        temperature: generation
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|v| v as f32),
        max_tokens: generation
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32),
        stream,
        stop: generation
            .get("stopSequences")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        top_p: generation
            .get("topP")
            .and_then(Value::as_f64)
            .map(|v| v as f32),
        frequency_penalty: generation
            .get("frequencyPenalty")
            .and_then(Value::as_f64)
            .map(|v| v as f32),
        presence_penalty: generation
            .get("presencePenalty")
            .and_then(Value::as_f64)
            .map(|v| v as f32),
        logit_bias: HashMap::new(),
        user: None,
        sampling: generation
            .get("topK")
            .and_then(Value::as_u64)
            .map(|top_k| SamplingParams {
                top_k: Some(top_k.min(u32::MAX as u64) as u32),
                ..Default::default()
            }),
        thinking_config: parse_thinking_config(generation),
        thinking_include_thoughts: generation
            .get("thinkingConfig")
            .and_then(|c| c.get("includeThoughts"))
            .and_then(Value::as_bool),
        effort: None,
        turn_index: None,
        tools,
        tool_choice,
        parallel_tool_calls: None,
        response_format: parse_response_format(generation),
        // 本变更仅 IR 形状适配：新槽入站解析不属 Gemini 功能面
        metadata: None,
        service_tier: None,
        verbosity: None,
        max_completion_tokens: None,
        vendor_extensions: absorb_vendor_extensions(body, super::types::GEMINI_KNOWN_KEYS),
    })
}

fn parse_content(content: &Value) -> Result<Vec<Message>, GateError> {
    let role = content.get("role").and_then(Value::as_str);
    let parts = content
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| validation("api-gemini-parts-invalid"))?;
    if parts.is_empty() {
        return Err(validation("api-gemini-parts-invalid"));
    }
    let assistant = matches!(role, Some("model") | Some("assistant"));
    let mut messages = Vec::new();

    if assistant {
        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut calls = Vec::new();
        for part in parts {
            let part: Part = serde_json::from_value(part.clone())
                .map_err(|_| validation("api-gemini-part-invalid"))?;
            if let Some(chunk) = part.text.as_deref() {
                // thought summary 属输出通道，请求历史中的 thought 文本丢弃
                if !part.thought {
                    text.push_str(chunk);
                }
            } else if let Some(blob) = part.inline_data {
                blocks.push(ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: blob.mime_type,
                        data: blob.data,
                    },
                    detail: None,
                });
            } else if let Some(file) = part.file_data {
                blocks.push(ContentBlock::Image {
                    source: ImageSource::Url { url: file.file_uri },
                    detail: None,
                });
            } else if let Some(call) = part.function_call {
                calls.push(tool_call_from_function(call)?);
            } else {
                return Err(validation("api-gemini-part-unsupported"));
            }
        }
        let text_empty = text.is_empty();
        let content = match (text_empty, blocks.is_empty(), calls.is_empty()) {
            (false, true, true) => MessageContent::Text(text),
            (true, false, true) => MessageContent::MultiModal(blocks),
            (false, false, true) => {
                let mut all = vec![ContentBlock::Text { text }];
                all.extend(blocks);
                MessageContent::MultiModal(all)
            }
            (true, true, false) => MessageContent::ToolCalls(calls),
            (false, true, false) => MessageContent::TextWithToolCalls {
                text,
                tool_calls: calls,
                reasoning: None,
            },
            // 图片 + 工具调用同消息：IR 无该组合槽，显式拒绝防静默丢图
            _ => return Err(validation("api-gemini-part-unsupported")),
        };
        messages.push(Message {
            role: Role::Assistant,
            content,
        });
        return Ok(messages);
    }

    // user/function：文本与图片聚合为一条 user 消息；functionResponse 按
    // 出现顺序拆成独立 Tool 消息（避免与文本混在同一 content 时丢失顺序）
    let mut pending = Vec::new();
    for part in parts {
        let part: Part = serde_json::from_value(part.clone())
            .map_err(|_| validation("api-gemini-part-invalid"))?;
        if let Some(chunk) = part.text.as_deref() {
            pending.push(ContentBlock::Text {
                text: chunk.to_string(),
            });
        } else if let Some(blob) = part.inline_data {
            pending.push(ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: blob.mime_type,
                    data: blob.data,
                },
                detail: None,
            });
        } else if let Some(file) = part.file_data {
            pending.push(ContentBlock::Image {
                source: ImageSource::Url { url: file.file_uri },
                detail: None,
            });
        } else if let Some(response) = part.function_response {
            if !pending.is_empty() {
                messages.push(Message {
                    role: Role::User,
                    content: blocks_to_content(&mut pending),
                });
            }
            messages.push(tool_message_from_response(response)?);
        } else if let Some(call) = part.function_call {
            // user 角色携带 functionCall 非规范；按 assistant 工具调用
            // 语义提升，避免静默丢弃
            let call = tool_call_from_function(call)?;
            if !pending.is_empty() {
                messages.push(Message {
                    role: Role::User,
                    content: blocks_to_content(&mut pending),
                });
            }
            messages.push(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(vec![call]),
            });
        } else {
            return Err(validation("api-gemini-part-unsupported"));
        }
    }
    if !pending.is_empty() {
        messages.push(Message {
            role: Role::User,
            content: blocks_to_content(&mut pending),
        });
    }
    Ok(messages)
}

fn blocks_to_content(blocks: &mut Vec<ContentBlock>) -> MessageContent {
    let take = std::mem::take(blocks);
    if take.len() == 1 {
        if let ContentBlock::Text { text } = &take[0] {
            return MessageContent::Text(text.clone());
        }
    }
    MessageContent::MultiModal(take)
}

fn tool_call_from_function(call: FunctionCall) -> Result<ToolCall, GateError> {
    if call.name.trim().is_empty() {
        return Err(validation("api-gemini-tool-name-required"));
    }
    Ok(ToolCall {
        id: format!("call_{}", uuid::Uuid::new_v4()),
        call_type: "function".into(),
        function: ToolCallFunction {
            name: call.name,
            arguments: call
                .args
                .map(|args| args.to_string())
                .unwrap_or_else(|| "{}".into()),
        },
    })
}

fn tool_message_from_response(response: FunctionResponse) -> Result<Message, GateError> {
    if response.name.trim().is_empty() {
        return Err(validation("api-gemini-function-response-name-missing"));
    }
    Ok(Message {
        role: Role::Tool,
        content: MessageContent::ToolResult(ToolResult {
            tool_call_id: response.name.clone(),
            function_name: response.name,
            content: response
                .response
                .map(|value| value.to_string())
                .unwrap_or_else(|| "{}".into()),
            is_error: false,
        }),
    })
}

fn parse_system_instruction(body: &Value) -> Option<String> {
    let instruction = body
        .get("systemInstruction")
        .or_else(|| body.get("system_instruction"))?;
    let text = instruction
        .get("parts")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

fn parse_tools(body: &Value) -> Result<Vec<Tool>, GateError> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in tools {
        let declarations = entry
            .get("functionDeclarations")
            .and_then(Value::as_array)
            .ok_or_else(|| validation("api-gemini-tool-unsupported"))?;
        for declaration in declarations {
            let name = declaration
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| validation("api-gemini-tool-name-required"))?;
            out.push(Tool {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: name.to_string(),
                    description: declaration
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    parameters: declaration
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                    strict: None,
                },
                ..Default::default()
            });
        }
    }
    Ok(out)
}

fn parse_tool_choice(body: &Value) -> Option<ToolChoice> {
    let config = body
        .get("toolConfig")
        .or_else(|| body.get("tool_config"))?
        .get("functionCallingConfig")?;
    let mode = config.get("mode").and_then(Value::as_str)?;
    match mode {
        "AUTO" => Some(ToolChoice::Strategy("auto".into())),
        "NONE" => Some(ToolChoice::Strategy("none".into())),
        "ANY" => {
            let names = config
                .get("allowedFunctionNames")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if names.len() == 1 {
                Some(ToolChoice::Specific {
                    choice_type: "function".into(),
                    function: ToolChoiceFunction {
                        name: names[0].clone(),
                    },
                })
            } else {
                Some(ToolChoice::Strategy("required".into()))
            }
        }
        _ => None,
    }
}

fn generation_config(body: &Value) -> &Value {
    static EMPTY: Value = Value::Null;
    body.get("generationConfig")
        .or_else(|| body.get("generation_config"))
        .unwrap_or(&EMPTY)
}

fn parse_thinking_config(generation: &Value) -> Option<ThinkingConfig> {
    let thinking = generation.get("thinkingConfig")?;
    let budget = thinking.get("thinkingBudget").and_then(Value::as_i64)?;
    if budget == 0 {
        Some(ThinkingConfig::Disabled)
    } else if budget > 0 {
        Some(ThinkingConfig::Enabled {
            budget_tokens: budget as u64,
            display: None,
        })
    } else {
        Some(ThinkingConfig::Adaptive { display: None })
    }
}

fn parse_response_format(generation: &Value) -> Option<Value> {
    let mime = generation
        .get("responseMimeType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !mime.eq_ignore_ascii_case("application/json") {
        return None;
    }
    let schema = generation
        .get("responseSchema")
        .or_else(|| generation.get("responseJsonSchema"));
    match schema {
        Some(schema) => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {"name": "gemini_response", "schema": schema}
        })),
        None => Some(serde_json::json!({"type": "json_object"})),
    }
}

fn validation(key: &str) -> GateError {
    GateError::Validation {
        field: t_args(key, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unified(body: Value) -> UnifiedRequest {
        to_unified_request(&body, "models/gemini-2.5-pro", false).expect("to_unified")
    }

    #[test]
    fn text_system_and_generation_config() {
        let body = json!({
            "systemInstruction": {"parts": [{"text": "be nice"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hello"}]},
                {"role": "model", "parts": [{"text": "hi"}]}
            ],
            "generationConfig": {
                "temperature": 0.2,
                "topP": 0.9,
                "topK": 32,
                "maxOutputTokens": 64,
                "stopSequences": ["END"],
                "presencePenalty": 0.1,
                "frequencyPenalty": 0.2
            }
        });
        let request = unified(body);
        assert_eq!(request.model, "gemini-2.5-pro");
        assert_eq!(request.system_prompt.as_deref(), Some("be nice"));
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, Role::User);
        assert_eq!(request.messages[1].role, Role::Assistant);
        assert!((request.temperature.unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(request.max_tokens, Some(64));
        assert_eq!(request.stop, vec!["END"]);
        assert_eq!(request.sampling.as_ref().unwrap().top_k, Some(32));
        assert!((request.frequency_penalty.unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn tools_and_tool_choice() {
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "tools": [{"functionDeclarations": [
                {"name": "get_weather", "description": "weather", "parameters": {"type": "object"}}
            ]}],
            "toolConfig": {"functionCallingConfig": {
                "mode": "ANY", "allowedFunctionNames": ["get_weather"]
            }}
        });
        let request = unified(body);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].function.name, "get_weather");
        match request.tool_choice {
            Some(ToolChoice::Specific { function, .. }) => {
                assert_eq!(function.name, "get_weather");
            }
            other => panic!("expected Specific, got {other:?}"),
        }
    }

    #[test]
    fn any_without_single_name_maps_required() {
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "ANY"}}
        });
        assert!(matches!(
            unified(body).tool_choice,
            Some(ToolChoice::Strategy(s)) if s == "required"
        ));
    }

    #[test]
    fn function_call_and_response_roundtrip() {
        let body = json!({
            "contents": [
                {"role": "user", "parts": [{"text": "weather?"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "SH"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "get_weather", "response": {"temp": 20}}}]}
            ]
        });
        let request = unified(body);
        assert_eq!(request.messages.len(), 3);
        assert!(matches!(
            request.messages[1].content,
            MessageContent::ToolCalls(ref calls) if calls[0].function.name == "get_weather"
        ));
        match &request.messages[2].content {
            MessageContent::ToolResult(result) => {
                assert_eq!(result.function_name, "get_weather");
                assert!(result.content.contains("\"temp\":20"));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn multimodal_inline_and_file_data() {
        let body = json!({
            "contents": [{"role": "user", "parts": [
                {"text": "look"},
                {"inlineData": {"mimeType": "image/png", "data": "AAA"}},
                {"fileData": {"mimeType": "image/jpeg", "fileUri": "gs://b/x"}}
            ]}]
        });
        let request = unified(body);
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 3);
                assert!(matches!(&blocks[1], ContentBlock::Image {
                    source: ImageSource::Base64 { media_type, .. }, ..
                } if media_type == "image/png"));
                assert!(matches!(&blocks[2], ContentBlock::Image {
                    source: ImageSource::Url { url }, ..
                } if url == "gs://b/x"));
            }
            other => panic!("expected MultiModal, got {other:?}"),
        }
    }

    #[test]
    fn structured_output_and_thinking() {
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {"type": "object", "properties": {"x": {"type": "string"}}},
                "thinkingConfig": {"thinkingBudget": 2048, "includeThoughts": true}
            }
        });
        let request = unified(body);
        assert_eq!(
            request.response_format.as_ref().unwrap()["type"],
            "json_schema"
        );
        assert!(matches!(
            request.thinking_config,
            Some(ThinkingConfig::Enabled {
                budget_tokens: 2048,
                ..
            })
        ));
        assert_eq!(request.thinking_include_thoughts, Some(true));
    }

    #[test]
    fn invalid_shapes_are_rejected() {
        assert!(to_unified_request(&json!({}), "m", false).is_err());
        assert!(to_unified_request(&json!({"contents": [{}]}), "m", false).is_err());
        assert!(to_unified_request(
            &json!({"contents": [{"role": "user", "parts": []}]}),
            "m",
            false
        )
        .is_err());
        assert!(to_unified_request(
            &json!({"contents": [{"role": "user", "parts": [{"executableCode": {}}]}]}),
            "m",
            false
        )
        .is_err());
    }
}
