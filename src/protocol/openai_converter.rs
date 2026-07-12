// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::NexusFlowError;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct OpenAIConverter;

impl OpenAIConverter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ProtocolConverter for OpenAIConverter {
    fn input_protocol(&self) -> Protocol {
        Protocol::OpenAI
    }

    fn output_protocol(&self) -> Protocol {
        Protocol::OpenAI
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
            .map(parse_openai_message)
            .collect::<Result<Vec<_>, _>>()?;

        // 解析工具定义
        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|t| parse_openai_tool(t)).collect())
            .unwrap_or_default();

        // 解析工具选择策略
        let tool_choice = body.get("tool_choice").map(|tc| parse_tool_choice(tc));

        Ok(UnifiedRequest {
            model,
            messages,
            temperature: body
                .get("temperature")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            max_tokens: body
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .map(|u| u as u32),
            stream: body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            stop: body
                .get("stop")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|f| f as f32),
            frequency_penalty: body
                .get("frequency_penalty")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            presence_penalty: body
                .get("presence_penalty")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            logit_bias: parse_logit_bias(body.get("logit_bias")),
            user: body
                .get("user")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            tools,
            tool_choice,
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, NexusFlowError> {
        // 将 UnifiedRequest 转换为 OpenAI 格式
        let messages: Vec<Value> = request
            .messages
            .into_iter()
            .map(message_to_openai)
            .collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
        });

        // 添加工具定义
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(tool_to_openai).collect();
            body["tools"] = json!(tools);
        }

        // 添加工具选择策略
        if let Some(tool_choice) = request.tool_choice {
            body["tool_choice"] = tool_choice_to_openai(tool_choice);
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
            body["stop"] = json!(request.stop);
        }
        if let Some(top_p) = request.top_p {
            body["top_p"] = json!(top_p);
        }
        if let Some(fp) = request.frequency_penalty {
            body["frequency_penalty"] = json!(fp);
        }
        if let Some(pp) = request.presence_penalty {
            body["presence_penalty"] = json!(pp);
        }
        if let Some(user) = request.user {
            body["user"] = json!(user);
        }

        Ok(body)
    }

    fn from_unified(&self, response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        let choices = response
            .choices
            .into_iter()
            .map(|choice| {
                json!({
                    "index": choice.index,
                    "message": choice.message.map(|m| message_to_openai(m)),
                    "finish_reason": choice.finish_reason,
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "id": response.id,
            "object": "chat.completion",
            "created": response.created,
            "model": response.model,
            "choices": choices,
            "usage": {
                "prompt_tokens": response.usage.prompt_tokens,
                "completion_tokens": response.usage.completion_tokens,
                "total_tokens": response.usage.total_tokens,
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
                source: format!("OpenAI error: {} - {}", error_type, message),
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

        let created = response
            .get("created")
            .and_then(|v| v.as_u64())
            .unwrap_or(chrono::Utc::now().timestamp() as u64);

        let choices: Vec<Choice> = response
            .get("choices")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .enumerate()
            .filter_map(|(index, choice)| {
                let choice_obj = choice.as_object()?;
                let message = choice_obj
                    .get("message")
                    .and_then(|m| m.as_object())
                    .map(|m| {
                        let role = m
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("assistant");
                        let role = match role {
                            "system" => Role::System,
                            "user" => Role::User,
                            "assistant" => Role::Assistant,
                            _ => Role::User,
                        };
                        let content = m
                            .get("content")
                            .and_then(|v| v.as_str())
                            .map(|s| MessageContent::Text(s.to_string()))
                            .unwrap_or(MessageContent::Text(String::new()));
                        Message { role, content }
                    });

                let finish_reason = choice_obj
                    .get("finish_reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                Some(Choice {
                    index: index as u32,
                    message,
                    finish_reason,
                    logprobs: None,
                })
            })
            .collect();

        let usage = response
            .get("usage")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| {
                fn empty_map() -> &'static serde_json::Map<String, serde_json::Value> {
                    static EMPTY: std::sync::OnceLock<serde_json::Map<String, serde_json::Value>> =
                        std::sync::OnceLock::new();
                    EMPTY.get_or_init(|| serde_json::Map::new())
                }
                empty_map()
            });

        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
            .unwrap_or(0);

        let completion_tokens = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
            .unwrap_or(0);

        let total_tokens = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
            .unwrap_or(prompt_tokens + completion_tokens);

        Ok(UnifiedResponse {
            id,
            object: "chat.completion".to_string(),
            created,
            model,
            choices,
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, NexusFlowError> {
        match chunk {
            StreamChunk::Content {
                index,
                delta,
                finish_reason,
            } => Ok(json!({
                "id": format!("chunk-{}", index),
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp() as u64,
                "model": "unknown",
                "choices": [{
                    "index": index,
                    "delta": delta_to_openai(delta),
                    "finish_reason": finish_reason,
                }],
            })),
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => Ok(json!({
                "id": "usage",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "unknown",
                "choices": [],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens,
                },
            })),
            StreamChunk::Heartbeat => Ok(json!({":": ""})),
        }
    }
}

fn parse_openai_message(json: &Value) -> Result<Message, NexusFlowError> {
    let role = json
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or(NexusFlowError::Validation {
            field: "role".into(),
        })?;

    let role = match role {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => {
            return Err(NexusFlowError::Validation {
                field: "role".into(),
            })
        }
    };

    let content = match json.get("content") {
        Some(Value::String(s)) => {
            // 检查是否有 tool_call_id（工具角色消息）
            if role == Role::Tool {
                let tool_call_id = json
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                MessageContent::ToolResult(ToolResult {
                    tool_call_id,
                    function_name: String::new(),
                    content: s.clone(),
                    is_error: false,
                })
            } else {
                MessageContent::Text(s.clone())
            }
        }
        Some(Value::Array(arr)) => {
            // 检查是否包含工具调用块
            let has_tool_calls = arr.iter().any(|block| {
                block.get("type").and_then(|t| t.as_str()) == Some("tool_call")
                    || block.get("function").is_some()
            });

            if has_tool_calls {
                let tool_calls: Vec<ToolCall> = arr
                    .iter()
                    .filter_map(|block| {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_call") {
                            parse_tool_call(block)
                        } else if block.get("function").is_some() {
                            // 兼容格式：从 function 对象解析
                            let func = block.get("function")?;
                            Some(ToolCall {
                                id: block.get("id")?.as_str()?.to_string(),
                                call_type: "function".to_string(),
                                function: ToolCallFunction {
                                    name: func.get("name")?.as_str()?.to_string(),
                                    arguments: func.get("arguments")?.to_string(),
                                },
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                if !tool_calls.is_empty() {
                    MessageContent::ToolCalls(tool_calls)
                } else {
                    let blocks = arr.iter().filter_map(parse_content_block).collect();
                    MessageContent::MultiModal(blocks)
                }
            } else {
                let blocks = arr.iter().filter_map(parse_content_block).collect();
                MessageContent::MultiModal(blocks)
            }
        }
        None | Some(Value::Null) => {
            // content 字段缺失或为 null，检查是否有 tool_calls
            if let Some(tool_calls_arr) = json.get("tool_calls").and_then(|v| v.as_array()) {
                let tool_calls: Vec<ToolCall> =
                    tool_calls_arr.iter().filter_map(parse_tool_call).collect();
                if !tool_calls.is_empty() {
                    MessageContent::ToolCalls(tool_calls)
                } else {
                    MessageContent::Text(String::new())
                }
            } else {
                MessageContent::Text(String::new())
            }
        }
        _ => {
            return Err(NexusFlowError::Validation {
                field: "content".into(),
            })
        }
    };

    Ok(Message { role, content })
}

fn parse_content_block(json: &Value) -> Option<ContentBlock> {
    let type_ = json.get("type")?.as_str()?;

    match type_ {
        "text" => Some(ContentBlock::Text {
            text: json.get("text")?.as_str()?.to_string(),
        }),
        "image_url" => {
            let image_url = json.get("image_url")?.as_object()?;
            let url = image_url.get("url")?.as_str()?.to_string();
            Some(ContentBlock::Image {
                source: ImageSource {
                    type_: "url".into(),
                    media_type: "image/jpeg".into(),
                    data: url,
                },
                detail: image_url
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            })
        }
        _ => None,
    }
}

fn message_to_openai(msg: Message) -> Value {
    let role_str = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    let (content, tool_call_id) = match msg.content {
        MessageContent::Text(s) => (Value::String(s), None),
        MessageContent::MultiModal(blocks) => {
            let arr: Vec<Value> = blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => json!({
                        "type": "text",
                        "text": text,
                    }),
                    ContentBlock::Image { source, detail } => json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", source.media_type, source.data),
                            "detail": detail.clone().unwrap_or_else(|| "auto".into()),
                        },
                    }),
                    _ => json!({}),
                })
                .collect();
            (Value::Array(arr), None)
        }
        MessageContent::ToolCalls(tool_calls) => {
            let tool_calls_arr: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": tc.call_type,
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }
                    })
                })
                .collect();
            (Value::Array(tool_calls_arr), None)
        }
        MessageContent::ToolResult(result) => (
            Value::String(result.content.clone()),
            Some(result.tool_call_id),
        ),
        MessageContent::TextWithToolCalls { text, tool_calls } => {
            let mut arr: Vec<Value> = vec![json!({
                "type": "text",
                "text": text,
            })];
            arr.extend(tool_calls.iter().map(|tc| {
                json!({
                    "id": tc.id,
                    "type": tc.call_type,
                    "function": {
                        "name": tc.function.name,
                        "arguments": tc.function.arguments,
                    }
                })
            }));
            (Value::Array(arr), None)
        }
    };

    let mut obj = json!({
        "role": role_str,
        "content": content,
    });

    // 工具角色消息需要 tool_call_id
    if let Some(id) = tool_call_id {
        obj["tool_call_id"] = json!(id);
    }

    obj
}

#[allow(dead_code)]
fn delta_to_openai(delta: MessageContent) -> Value {
    match delta {
        MessageContent::Text(s) => json!({ "content": s }),
        MessageContent::MultiModal(blocks) => {
            if let Some(ContentBlock::Text { text }) = blocks.first() {
                json!({ "content": text })
            } else {
                json!({ "content": "" })
            }
        }
        MessageContent::ToolCalls(_) => json!({ "content": "" }),
        MessageContent::ToolResult(result) => json!({ "content": result.content }),
        MessageContent::TextWithToolCalls { text, .. } => json!({ "content": text }),
    }
}

fn parse_logit_bias(value: Option<&Value>) -> HashMap<String, f32> {
    match value {
        Some(Value::Object(obj)) => obj
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f as f32)))
            .collect(),
        _ => HashMap::new(),
    }
}

/// 解析 OpenAI 格式的工具调用
fn parse_tool_call(json: &Value) -> Option<ToolCall> {
    let obj = json.as_object()?;
    let func = obj.get("function")?.as_object()?;

    Some(ToolCall {
        id: obj.get("id")?.as_str()?.to_string(),
        call_type: obj
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("function")
            .to_string(),
        function: ToolCallFunction {
            name: func.get("name")?.as_str()?.to_string(),
            arguments: func.get("arguments")?.to_string(),
        },
    })
}

/// 解析 OpenAI 格式的工具定义
fn parse_openai_tool(json: &Value) -> Option<Tool> {
    let obj = json.as_object()?;
    let func = obj.get("function")?.as_object()?;

    // 解析参数
    let parameters = func
        .get("parameters")
        .and_then(|p| p.as_object())
        .map(|params| {
            let properties: HashMap<String, ToolParameterProperty> = params
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
                param_type: params
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("object")
                    .to_string(),
                properties,
                required: params
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
        tool_type: obj
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("function")
            .to_string(),
        function: ToolFunction {
            name: func.get("name")?.as_str()?.to_string(),
            description: func
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
            parameters,
        },
    })
}

/// 将工具转换为 OpenAI 格式
fn tool_to_openai(tool: &Tool) -> Value {
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
        "type": tool.tool_type,
        "function": {
            "name": tool.function.name,
            "description": tool.function.description,
            "parameters": params_obj,
        }
    })
}

/// 解析工具选择策略
fn parse_tool_choice(json: &Value) -> ToolChoice {
    match json {
        Value::String(s) => ToolChoice::Strategy(s.clone()),
        Value::Object(obj) => {
            if let Some(func) = obj.get("function").and_then(|f| f.as_object()) {
                ToolChoice::Specific {
                    choice_type: obj
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("function")
                        .to_string(),
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
    }
}

/// 将工具选择策略转换为 OpenAI 格式
fn tool_choice_to_openai(tool_choice: ToolChoice) -> Value {
    match tool_choice {
        ToolChoice::Strategy(s) => Value::String(s),
        ToolChoice::Specific {
            choice_type,
            function,
        } => json!({
            "type": choice_type,
            "function": {
                "name": function.name,
            }
        }),
    }
}
