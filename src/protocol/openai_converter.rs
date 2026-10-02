// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::NexusFlowError;
use crate::i18n::{t, t_args};
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct OpenAIConverter;

impl Default for OpenAIConverter {
    fn default() -> Self {
        Self::new()
    }
}

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
            .map(|arr| arr.iter().filter_map(parse_openai_tool).collect())
            .unwrap_or_default();

        // 解析工具选择策略
        let tool_choice = body.get("tool_choice").map(parse_tool_choice);

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

    fn response_to_external(&self, response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        let choices = response
            .choices
            .into_iter()
            .map(|choice| {
                json!({
                    "index": choice.index,
                    "message": choice.message.map(message_to_openai),
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
                .map(String::from)
                .unwrap_or_else(|| t("protocol-unknown-error"));
            let error_type = error_obj
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(NexusFlowError::Protocol {
                source: t_args(
                    "protocol-openai-error",
                    &[("type", error_type), ("message", message)],
                ),
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
                        // recId=8③：message.tool_calls 解析（此前只读
                        // content 字符串，工具调用响应在 IR 中静默降级为纯文本）
                        let content = match parse_tool_calls_field(&Value::Object(m.clone())) {
                            Some(tool_calls) => match content {
                                MessageContent::Text(s) if !s.is_empty() => {
                                    MessageContent::TextWithToolCalls {
                                        text: s,
                                        tool_calls,
                                    }
                                }
                                _ => MessageContent::ToolCalls(tool_calls),
                            },
                            None => content,
                        };
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
                    EMPTY.get_or_init(serde_json::Map::new)
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

    fn parse_stream_frame(&self, data: &str) -> Option<StreamChunk> {
        parse_sse_frame_to_chunk(data)
    }
}

/// 解析 OpenAI 上游 SSE 帧的 `data` 负载 → IR StreamChunk（recId=3）。
///
/// 输入是分帧器剥出的 `data:` 负载（`chat.completion.chunk` JSON 或
/// `[DONE]`）：
/// - `[DONE]` → `None`（终止标记，不映射 IR）；
/// - 顶层 `usage` 帧（`stream_options.include_usage` 尾帧，choices 空）
///   → [`StreamChunk::Usage`]；
/// - `choices[0].delta.content` → [`StreamChunk::Content`]（Text delta）；
/// - `choices[0].delta.tool_calls` → [`StreamChunk::Content`]（ToolCalls
///   delta，与 recId=8 联动：流式工具片段不静默丢弃）；
/// - `choices[0].finish_reason` 非空时挂到 Content 的 `finish_reason`；
/// - role-only 首帧（仅 `delta.role`，无内容无终止）→ `None` 跳过。
pub fn parse_sse_frame_to_chunk(data: &str) -> Option<StreamChunk> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return None;
    }
    // 错误 JSON 不映射 IR（manager 层分帧器的 is_error 已先行拦截终止）
    let json: Value = serde_json::from_str(trimmed).ok()?;
    if json.get("error").is_some() {
        return None;
    }

    // usage 尾帧：choices 为空且带 usage 对象
    if let Some(usage) = json.get("usage").filter(|u| u.is_object()) {
        let choices_empty = json
            .get("choices")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if choices_empty {
            let prompt = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let completion = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let total = usage
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .map(|u| u as u32)
                .unwrap_or(prompt + completion);
            return Some(StreamChunk::Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total,
            });
        }
    }

    let choice = json.get("choices")?.as_array()?.first()?.as_object()?;
    let index = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let delta = choice.get("delta");

    // delta.tool_calls：流式工具调用片段聚合（recId=8⑦ 联动回归）
    let tool_call_delta = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|v| v.as_array())
        .filter(|arr| !arr.is_empty())
        .map(|arr| {
            MessageContent::ToolCalls(
                arr.iter()
                    .filter_map(parse_stream_tool_call_delta)
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|m| matches!(m, MessageContent::ToolCalls(calls) if !calls.is_empty()));

    let text_delta = delta
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| MessageContent::Text(s.to_string()));

    let delta_content = tool_call_delta.or(text_delta);

    // role-only 首帧（无内容无终止）：跳过，不产出空事件
    let has_payload = delta_content.is_some() || finish_reason.is_some();
    if !has_payload {
        return None;
    }

    Some(StreamChunk::Content {
        index,
        delta: delta_content.unwrap_or(MessageContent::Text(String::new())),
        finish_reason,
    })
}

/// 解析流式 tool_calls delta 片段（OpenAI 流式形状：id/name 仅首片段携带，
/// arguments 逐片段增量）。片段缺省字段以空串占位，聚合语义后置于
/// recId=8 的工具流式回归。
fn parse_stream_tool_call_delta(json: &Value) -> Option<ToolCall> {
    let obj = json.as_object()?;
    let function = obj.get("function").and_then(|f| f.as_object());
    Some(ToolCall {
        id: obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        call_type: obj
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("function")
            .to_string(),
        function: ToolCallFunction {
            name: function
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            arguments: function
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
    })
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
            } else if let Some(tool_calls) = parse_tool_calls_field(json) {
                // recId=8②：String content + 顶层 tool_calls →
                // TextWithToolCalls（此前 String 分支提前返回，tool_calls 丢失）
                if s.is_empty() {
                    MessageContent::ToolCalls(tool_calls)
                } else {
                    MessageContent::TextWithToolCalls {
                        text: s.clone(),
                        tool_calls,
                    }
                }
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
                                    arguments: arguments_as_string(func.get("arguments"))?,
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
            // （recId=8②：提取逻辑提为共用 helper）
            if let Some(tool_calls) = parse_tool_calls_field(json) {
                MessageContent::ToolCalls(tool_calls)
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

    let (content, tool_call_id, top_level_tool_calls) = match msg.content {
        MessageContent::Text(s) => (Value::String(s), None, None),
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
            (Value::Array(arr), None, None)
        }
        MessageContent::ToolCalls(tool_calls) => {
            // recId=8：OpenAI 出站规范 —— 工具调用在顶层 tool_calls 字段，
            // content 为 null（此前塞进 content 数组，OpenAI 上游拒收）
            (Value::Null, None, Some(tool_calls))
        }
        MessageContent::ToolResult(result) => (
            Value::String(result.content.clone()),
            Some(result.tool_call_id),
            None,
        ),
        MessageContent::TextWithToolCalls { text, tool_calls } => {
            // recId=8：文本与工具调用并列 —— content 为文本 + 顶层 tool_calls
            (Value::String(text), None, Some(tool_calls))
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

    // recId=8：assistant 工具调用输出为顶层 tool_calls
    if let Some(calls) = top_level_tool_calls {
        let calls_arr: Vec<Value> = calls
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
        obj["tool_calls"] = Value::Array(calls_arr);
    }

    obj
}

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

/// 提取消息顶层 `tool_calls` 数组（recId=8②：String content 分支与
/// content 缺失分支共用；空数组视为无工具调用）。
fn parse_tool_calls_field(json: &Value) -> Option<Vec<ToolCall>> {
    let arr = json.get("tool_calls")?.as_array()?;
    let calls: Vec<ToolCall> = arr.iter().filter_map(parse_tool_call).collect();
    if calls.is_empty() {
        None
    } else {
        Some(calls)
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
            arguments: arguments_as_string(func.get("arguments"))?,
        },
    })
}

/// 提取工具调用参数字符串（OpenAI 规范 arguments 为含 JSON 文本的字符串）。
///
/// 字符串原样取用；非字符串形状（对象等）保留其 JSON 表示。不可对任意
/// `Value` 直接 `to_string()` —— 它会序列化整个值，把字符串再包一层
/// 引号造成双重编码（IR 中 arguments 损坏，出站 `input` 退化为字符串）。
fn arguments_as_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
        None => None,
    }
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
