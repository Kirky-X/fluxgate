// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::NexusFlowError;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct AnthropicConverter;

impl AnthropicConverter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ProtocolConverter for AnthropicConverter {
    fn input_protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    fn output_protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    fn to_unified(&self, body: Value) -> Result<UnifiedRequest, NexusFlowError> {
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or(NexusFlowError::Validation {
                field: "model".into(),
            })?
            .to_lowercase();

        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or(NexusFlowError::Validation {
                field: "messages".into(),
            })?
            .iter()
            .map(parse_anthropic_message)
            .collect::<Result<Vec<_>, _>>()?;

        // 解析 Anthropic 格式的工具定义
        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|t| parse_anthropic_tool(t)).collect())
            .unwrap_or_default();

        // Anthropic 的 tool_choice 映射到统一格式
        let tool_choice = body.get("tool_choice").map(|tc| match tc {
            Value::String(s) => ToolChoice::Strategy(s.clone()),
            Value::Object(obj) => {
                if let Some(func) = obj.get("function").and_then(|f| f.as_object()) {
                    ToolChoice::Specific {
                        choice_type: "function".to_string(),
                        function: ToolChoiceFunction {
                            name: func
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string(),
                        },
                    }
                } else {
                    ToolChoice::Strategy("auto".to_string())
                }
            }
            _ => ToolChoice::Strategy("auto".to_string()),
        });

        Ok(UnifiedRequest {
            model,
            messages,
            temperature: body
                .get("temperature")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            max_tokens: body
                .get("max_tokens")
                .or(body.get("max_tokens_to_sample"))
                .and_then(|v| v.as_u64())
                .map(|u| u as u32),
            stream: body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            stop: body
                .get("stop_sequences")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            logit_bias: HashMap::new(),
            user: None,
            tools,
            tool_choice,
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, NexusFlowError> {
        // 将 UnifiedRequest 转换为 Anthropic 格式
        let messages: Vec<Value> = request
            .messages
            .into_iter()
            .map(|msg| message_to_anthropic(&msg))
            .collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
        });

        // Anthropic 使用 tools 字段
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(tool_to_anthropic).collect();
            body["tools"] = json!(tools);
        }

        // Anthropic tool_choice 转换
        if let Some(tool_choice) = request.tool_choice {
            body["tool_choice"] = match tool_choice {
                ToolChoice::Strategy(s) => Value::String(s),
                ToolChoice::Specific { function, .. } => json!({
                    "type": "function",
                    "function": { "name": function.name }
                }),
            };
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }
        if let Some(max) = request.max_tokens {
            body["max_tokens"] = json!(max);
        }
        if request.stream {
            body["stream"] = json!(true);
        }
        if !request.stop.is_empty() {
            body["stop_sequences"] = json!(request.stop);
        }

        Ok(body)
    }

    fn from_unified(&self, response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        let choice = response.choices.first().ok_or(NexusFlowError::Validation {
            field: "choices".into(),
        })?;

        let content = choice
            .message
            .as_ref()
            .map(|m| message_to_anthropic(m))
            .unwrap_or_else(|| json!({ "content": "" }));

        Ok(json!({
            "id": response.id,
            "type": "message",
            "role": "assistant",
            "content": content,
            "model": response.model,
            "stop_reason": choice.finish_reason.clone().unwrap_or_else(|| "stop".into()),
            "stop_sequence": Value::Null,
            "usage": {
                "input_tokens": response.usage.prompt_tokens,
                "output_tokens": response.usage.completion_tokens,
            },
        }))
    }

    fn parse_response(&self, response: Value) -> Result<UnifiedResponse, NexusFlowError> {
        // Check for error response
        if let Some(error_obj) = response.get("error").and_then(|v| v.as_object()) {
            let message = error_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            let error_type = error_obj
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(NexusFlowError::Protocol {
                source: format!("Anthropic error: {} - {}", error_type, message),
                inner: None,
            });
        }

        let id = response
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let model = response
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let content = response.get("content").unwrap_or(&Value::Null);

        let message = match content {
            Value::Array(arr) if !arr.is_empty() => {
                // Check if this is a tool_use response
                let has_tool_use = arr
                    .iter()
                    .any(|item| item.get("type").and_then(|t| t.as_str()) == Some("tool_use"));

                if has_tool_use {
                    // Parse tool calls
                    let mut tool_calls = Vec::new();
                    let mut text_parts = Vec::new();

                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str());
                        match item_type {
                            Some("tool_use") => {
                                let id = item
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = item
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input = item
                                    .get("input")
                                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                                    .unwrap_or_default();

                                tool_calls.push(ToolCall {
                                    id,
                                    call_type: "function".to_string(),
                                    function: ToolCallFunction {
                                        name,
                                        arguments: input,
                                    },
                                });
                            }
                            Some("text") => {
                                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                    text_parts.push(text.to_string());
                                }
                            }
                            _ => {}
                        }
                    }

                    let message_content = if !tool_calls.is_empty() && !text_parts.is_empty() {
                        // Has both text and tool calls
                        MessageContent::TextWithToolCalls {
                            text: text_parts.join("\n"),
                            tool_calls,
                        }
                    } else if !tool_calls.is_empty() {
                        MessageContent::ToolCalls(tool_calls)
                    } else {
                        MessageContent::Text(text_parts.join("\n"))
                    };

                    Some(Message {
                        role: Role::Assistant,
                        content: message_content,
                    })
                } else {
                    // Pure text response
                    let first = &arr[0];
                    let text = first
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(Message {
                        role: Role::Assistant,
                        content: MessageContent::Text(text),
                    })
                }
            }
            Value::String(s) => Some(Message {
                role: Role::Assistant,
                content: MessageContent::Text(s.clone()),
            }),
            _ => None,
        };

        let stop_reason = response
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let usage = response.get("usage").and_then(|v| v.as_object());

        let prompt_tokens = usage
            .and_then(|v| v.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
            .unwrap_or(0);

        let completion_tokens = usage
            .and_then(|v| v.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
            .unwrap_or(0);

        Ok(UnifiedResponse {
            id,
            object: "message".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model,
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason: stop_reason,
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, NexusFlowError> {
        match chunk {
            StreamChunk::Content {
                index: _,
                delta,
                finish_reason,
            } => {
                let delta_text = match &delta {
                    MessageContent::Text(s) => s.clone(),
                    _ => String::new(),
                };

                let _content = match delta {
                    MessageContent::Text(s) => vec![json!({
                        "type": "text",
                        "text": s,
                    })],
                    MessageContent::MultiModal(blocks) => blocks
                        .iter()
                        .map(|block| match block {
                            ContentBlock::Text { text } => json!({
                                "type": "text",
                                "text": text,
                            }),
                            ContentBlock::Image { source, detail: _ } => json!({
                                "type": "image",
                                "source": {
                                    "type": source.type_.clone(),
                                    "media_type": source.media_type.clone(),
                                    "data": source.data.clone(),
                                },
                            }),
                            _ => json!({ "type": "unknown" }),
                        })
                        .collect(),
                    MessageContent::ToolCalls(_) => vec![json!({"type": "text", "text": ""})],
                    MessageContent::ToolResult(result) => {
                        vec![json!({"type": "tool_result", "content": result.content})]
                    }
                    MessageContent::TextWithToolCalls { text, .. } => {
                        vec![json!({"type": "text", "text": text})]
                    }
                };

                Ok(json!({
                    "type": "content_block_delta",
                    "delta": {
                        "type": "text_delta",
                        "text": delta_text,
                    },
                    "content_block": {
                        "type": "text",
                        "text": delta_text,
                    },
                    "stop_reason": finish_reason,
                }))
            }
            StreamChunk::Usage {
                prompt_tokens: _,
                completion_tokens,
                total_tokens: _,
            } => Ok(json!({
                "type": "message_delta",
                "usage": {
                    "output_tokens": completion_tokens,
                },
            })),
            StreamChunk::Heartbeat => Ok(json!({ "type": "ping" })),
        }
    }
}

fn parse_anthropic_message(json: &Value) -> Result<Message, NexusFlowError> {
    let role = json.get("role").and_then(|v| v.as_str()).unwrap_or("user");

    let role = match role {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => Role::User,
    };

    let content = match json.get("content") {
        Some(Value::String(s)) => MessageContent::Text(s.clone()),
        Some(Value::Array(arr)) => {
            let blocks = arr
                .iter()
                .filter_map(|block| parse_anthropic_block(block))
                .collect();
            MessageContent::MultiModal(blocks)
        }
        None => MessageContent::Text(String::new()),
        _ => {
            return Err(NexusFlowError::Validation {
                field: "content".into(),
            })
        }
    };

    Ok(Message { role, content })
}

fn parse_anthropic_block(json: &Value) -> Option<ContentBlock> {
    let type_ = json.get("type")?.as_str()?;

    match type_ {
        "text" => Some(ContentBlock::Text {
            text: json.get("text")?.as_str()?.to_string(),
        }),
        "image" => {
            let source = json.get("source")?.as_object()?;
            Some(ContentBlock::Image {
                source: ImageSource {
                    type_: source.get("type")?.as_str()?.to_string(),
                    media_type: source.get("media_type")?.as_str()?.to_string(),
                    data: source.get("data")?.as_str()?.to_string(),
                },
                detail: None,
            })
        }
        _ => None,
    }
}

fn message_to_anthropic(msg: &Message) -> Value {
    let content = match &msg.content {
        MessageContent::Text(s) => {
            json!([{
                "type": "text",
                "text": s,
            }])
        }
        MessageContent::MultiModal(blocks) => {
            let arr: Vec<Value> = blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => json!({
                        "type": "text",
                        "text": text,
                    }),
                    ContentBlock::Image { source, detail: _ } => json!({
                        "type": "image",
                        "source": {
                            "type": source.type_,
                            "media_type": source.media_type,
                            "data": source.data,
                        },
                    }),
                    _ => json!({ "type": "unknown" }),
                })
                .collect();
            json!(arr)
        }
        MessageContent::ToolCalls(tool_calls) => {
            let arr: Vec<Value> = tool_calls.iter().map(|tc| {
                json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null),
                })
            }).collect();
            json!(arr)
        }
        MessageContent::ToolResult(result) => json!([{
            "type": "tool_result",
            "tool_call_id": result.tool_call_id,
            "content": result.content,
        }]),
        MessageContent::TextWithToolCalls { text, tool_calls } => {
            let mut arr: Vec<Value> = vec![json!({
                "type": "text",
                "text": text,
            })];
            arr.extend(tool_calls.iter().map(|tc| {
                json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null),
                })
            }));
            json!(arr)
        }
    };

    let role_str = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user", // Anthropic 使用 user 角色发送工具结果
    };

    json!({
        "role": role_str,
        "content": content,
    })
}

/// 解析 Anthropic 格式的工具定义
fn parse_anthropic_tool(json: &Value) -> Option<Tool> {
    let obj = json.as_object()?;

    // Anthropic 使用 name 而不是 function.name
    let name = obj.get("name")?.as_str()?.to_string();
    let description = obj
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();

    // 解析输入模式
    let parameters = obj
        .get("input_schema")
        .and_then(|s| s.as_object())
        .map(|schema| {
            let properties: HashMap<String, ToolParameterProperty> = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|props| {
                    props
                        .iter()
                        .filter_map(|(name, prop)| {
                            let prop_obj = prop.as_object()?;
                            Some((
                                name.clone(),
                                ToolParameterProperty {
                                    param_type: prop_obj.get("type")?.as_str()?.to_string(),
                                    description: prop_obj
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    enum_values: prop_obj
                                        .get("enum")
                                        .and_then(|e| e.as_array())
                                        .map(|arr| {
                                            arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                },
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();

            ToolParameters {
                param_type: schema
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("object")
                    .to_string(),
                properties,
                required: schema
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .unwrap_or_default();

    Some(Tool {
        tool_type: "function".to_string(),
        function: ToolFunction {
            name,
            description,
            parameters,
        },
    })
}

/// 将工具转换为 Anthropic 格式
fn tool_to_anthropic(tool: &Tool) -> Value {
    let mut params_obj = json!({
        "type": tool.function.parameters.param_type,
    });

    if !tool.function.parameters.properties.is_empty() {
        let properties: serde_json::Map<String, Value> = tool
            .function
            .parameters
            .properties
            .iter()
            .map(|(name, prop)| {
                let mut prop_obj = json!({
                    "type": prop.param_type,
                });
                if !prop.description.is_empty() {
                    prop_obj["description"] = json!(prop.description);
                }
                if !prop.enum_values.is_empty() {
                    prop_obj["enum"] = json!(prop.enum_values);
                }
                (name.clone(), prop_obj)
            })
            .collect();
        params_obj["properties"] = json!(properties);
    }

    if !tool.function.parameters.required.is_empty() {
        params_obj["required"] = json!(tool.function.parameters.required);
    }

    json!({
        "name": tool.function.name,
        "description": tool.function.description,
        "input_schema": params_obj,
    })
}
