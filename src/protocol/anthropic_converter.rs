// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::NexusFlowError;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct AnthropicConverter;

impl Default for AnthropicConverter {
    fn default() -> Self {
        Self::new()
    }
}

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

        // recId=8④：顶层 system 参数 → 首条 System 消息（此前
        // `get("system")` 全仓零命中，system 指令整体丢失）。
        // Anthropic 的 system 支持 string 或 content blocks 数组两种形状。
        let mut all_messages = Vec::new();
        if let Some(system) = body.get("system") {
            let text = match system {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            if !text.is_empty() {
                all_messages.push(Message {
                    role: Role::System,
                    content: MessageContent::Text(text),
                });
            }
        }
        all_messages.extend(messages);

        // 解析 Anthropic 格式的工具定义
        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(parse_anthropic_tool).collect())
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
            messages: all_messages,
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
        // recId=8④：Role::System 消息提升为 Anthropic 顶层 system 参数
        // （Anthropic messages 数组不接受 system 角色，直接放入会被上游拒收）
        let system_text = extract_system_text(&request.messages);
        let messages: Vec<Value> = request
            .messages
            .iter()
            .filter(|msg| msg.role != Role::System)
            .map(message_to_anthropic)
            .collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
        });

        if !system_text.is_empty() {
            body["system"] = json!(system_text);
        }

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

    fn response_to_external(&self, response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        let choice = response.choices.first().ok_or(NexusFlowError::Validation {
            field: "choices".into(),
        })?;

        let content = choice
            .message
            .as_ref()
            .map(message_to_anthropic)
            .unwrap_or_else(|| json!({ "content": "" }));

        Ok(json!({
            "id": response.id,
            "type": "message",
            "role": "assistant",
            "content": content,
            "model": response.model,
            "stop_reason": choice
                .finish_reason
                .as_deref()
                .map(finish_reason_to_anthropic_stop_reason)
                .unwrap_or_else(|| "end_turn".into()),
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
                    // Parse tool calls（recId=8①：与 parse_anthropic_message
                    // 的 tool_use 聚合共用同一 helper）
                    let mut tool_calls = Vec::new();
                    let mut text_parts = Vec::new();

                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str());
                        match item_type {
                            Some("tool_use") => {
                                if let Some(call) = parse_tool_use_block(item) {
                                    tool_calls.push(call);
                                }
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

        // recId=8⑤：stop_reason → finish_reason 静态映射（IR 统一存
        // OpenAI 词汇；未知值原样保留）
        let stop_reason = response
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(anthropic_stop_reason_to_finish_reason);

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

    fn parse_stream_frame(&self, data: &str) -> Option<StreamChunk> {
        parse_sse_frame_to_chunk(data)
    }
}

/// 解析 Anthropic 上游 SSE 帧的 `data` 负载 → IR StreamChunk（recId=3）。
///
/// Anthropic 流式事件的 `data` JSON 自带 `type` 字段：
/// - `content_block_delta`（text_delta）→ [`StreamChunk::Content`]；
/// - `message_delta`（usage.output_tokens 尾帧）→ [`StreamChunk::Usage`]
///   （token 记账依赖它；其 `delta.stop_reason` 的流式传递后置与 recId=8
///   工具流式一起回归）；
/// - `ping` → [`StreamChunk::Heartbeat`]；
/// - `message_start`/`content_block_start`/`content_block_stop`/
///   `message_stop` → `None`（生命周期事件，由出站侧
///   [`AnthropicStreamEncoder`] 状态机重建）；
/// - `error` → `None`（manager 层分帧器 is_error 先行拦截终止）。
pub fn parse_sse_frame_to_chunk(data: &str) -> Option<StreamChunk> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return None;
    }
    let json: Value = serde_json::from_str(trimmed).ok()?;
    if json.get("error").is_some() {
        return None;
    }

    match json.get("type").and_then(|v| v.as_str())? {
        "content_block_delta" => {
            let delta = json.get("delta")?;
            // 首版仅 text delta（工具流式后置与 recId=8 一起回归）
            let text = delta
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Some(StreamChunk::Content {
                index: json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                delta: MessageContent::Text(text.to_string()),
                finish_reason: None,
            })
        }
        "message_delta" => {
            let usage = json.get("usage");
            let output = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let input = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            Some(StreamChunk::Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
            })
        }
        "ping" => Some(StreamChunk::Heartbeat),
        _ => None,
    }
}

/// Anthropic 出站流式状态机（recId=3 阶段二③）。
///
/// IR StreamChunk 流不携带 Anthropic 流式协议的生命周期事件 —— 直接逐块
/// [`ProtocolConverter::stream_chunk_to_external`] 输出的
/// `content_block_delta` 不构成合法 Anthropic 流（缺
/// message_start/content_block_start/content_block_stop/message_stop）。
/// 本状态机在 IR 事件之上补齐完整生命周期：
///
/// ```text
/// message_start（首个 Content/Usage 前发一次）
///   content_block_start（首个 text delta 前发）
///     content_block_delta*（每个 text delta）
///   content_block_stop（finish_reason 或 Usage 到达时闭合）
/// message_delta（Usage 到达：stop_reason + output_tokens）
/// message_stop（finish 时）
/// ```
///
/// 首版仅 text delta + usage + 生命周期事件；工具流式后置与 recId=8
/// 一起回归。
#[derive(Debug)]
pub struct AnthropicStreamEncoder {
    message_started: bool,
    block_open: bool,
    block_index: u32,
    stop_reason: Option<String>,
}

impl Default for AnthropicStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicStreamEncoder {
    pub fn new() -> Self {
        Self {
            message_started: false,
            block_open: false,
            block_index: 0,
            stop_reason: None,
        }
    }

    /// 输入一个 IR chunk，产出 0..n 个合法 Anthropic SSE 事件帧文本
    /// （不含定界空行，调用方负责追加 `\n\n`）。
    pub fn encode_chunk(&mut self, chunk: StreamChunk) -> Vec<String> {
        match chunk {
            StreamChunk::Content {
                delta,
                finish_reason,
                ..
            } => {
                let text = extract_delta_text(&delta);
                if let Some(reason) = finish_reason {
                    self.stop_reason = Some(reason);
                }

                let mut events = Vec::new();
                if !self.message_started {
                    events.push(self.message_start_event());
                }
                if !text.is_empty() {
                    if !self.block_open {
                        events.push(self.content_block_start_event());
                    }
                    events.push(
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {
                                "type": "text_delta",
                                "text": text,
                            },
                        })
                        .to_string(),
                    );
                }
                // finish_reason 到达即闭合当前 content block
                // （message_delta 由 Usage 帧或 finish 兜底发出）
                if self.stop_reason.is_some() && self.block_open {
                    events.push(self.content_block_stop_event());
                    self.block_open = false;
                }
                events
            }
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                let mut events = Vec::new();
                if !self.message_started {
                    events.push(self.message_start_event_with_input(prompt_tokens));
                }
                if self.block_open {
                    events.push(self.content_block_stop_event());
                    self.block_open = false;
                }
                events.push(
                    json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": self.stop_reason,
                            "stop_sequence": Value::Null,
                        },
                        "usage": {
                            "output_tokens": completion_tokens,
                        },
                    })
                    .to_string(),
                );
                events
            }
            StreamChunk::Heartbeat => vec![json!({ "type": "ping" }).to_string()],
        }
    }

    /// 流收尾：补齐未闭合的 content_block_stop 与 message_stop。
    /// 正常流（已收 message_delta）只补 message_stop；空流产出完整
    /// message_start + message_stop（合法的最小 Anthropic 流）。
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.message_started {
            events.push(self.message_start_event());
        }
        if self.block_open {
            events.push(self.content_block_stop_event());
            self.block_open = false;
        }
        events.push(json!({ "type": "message_stop" }).to_string());
        events
    }

    fn message_start_event(&mut self) -> String {
        self.message_start_event_with_input(0)
    }

    fn message_start_event_with_input(&mut self, input_tokens: u32) -> String {
        self.message_started = true;
        json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "unknown",
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": 0,
                },
            },
        })
        .to_string()
    }

    fn content_block_start_event(&mut self) -> String {
        self.block_open = true;
        json!({
            "type": "content_block_start",
            "index": self.block_index,
            "content_block": {
                "type": "text",
                "text": "",
            },
        })
        .to_string()
    }

    fn content_block_stop_event(&mut self) -> String {
        json!({
            "type": "content_block_stop",
            "index": self.block_index,
        })
        .to_string()
    }
}

/// 从 IR delta 提取文本（首版 text delta 之外的内容块不产出流式文本）。
fn extract_delta_text(delta: &MessageContent) -> String {
    match delta {
        MessageContent::Text(s) => s.clone(),
        MessageContent::MultiModal(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        MessageContent::TextWithToolCalls { text, .. } => text.clone(),
        // 工具流式后置（recId=8）：ToolCalls/ToolResult delta 首版不产出文本
        _ => String::new(),
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
            // recId=8①：tool_use / tool_result 块聚合（此前经
            // parse_anthropic_block 静默丢弃，多轮工具对话历史断裂）。
            // 不改 ContentBlock 枚举以控制爆炸半径 —— 聚合发生在本层。
            let has_tool_use = arr
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
            let has_tool_result = arr
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));

            if has_tool_result {
                // user 消息内的工具结果 → 规范化为 Role::Tool + ToolResult
                // （出站 OpenAI 要求 role:"tool"+tool_call_id）
                return Ok(Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult(aggregate_anthropic_tool_result(arr)),
                });
            }
            if role == Role::Assistant && has_tool_use {
                // assistant 的 tool_use 聚合 → ToolCalls / TextWithToolCalls
                // （与 parse_response 已有的聚合方式对齐）
                let mut tool_calls = Vec::new();
                let mut text_parts = Vec::new();
                for block in arr {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("tool_use") => {
                            if let Some(call) = parse_tool_use_block(block) {
                                tool_calls.push(call);
                            }
                        }
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                let message_content = if !tool_calls.is_empty() && !text_parts.is_empty() {
                    MessageContent::TextWithToolCalls {
                        text: text_parts.join("\n"),
                        tool_calls,
                    }
                } else if !tool_calls.is_empty() {
                    MessageContent::ToolCalls(tool_calls)
                } else {
                    MessageContent::Text(text_parts.join("\n"))
                };
                return Ok(Message {
                    role,
                    content: message_content,
                });
            }

            let blocks = arr.iter().filter_map(parse_anthropic_block).collect();
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

/// 解析 Anthropic tool_use 块 → ToolCall（recId=8① 提取，
/// parse_response 与 parse_anthropic_message 共用）。
fn parse_tool_use_block(block: &Value) -> Option<ToolCall> {
    let id = block.get("id")?.as_str()?.to_string();
    let name = block.get("name")?.as_str()?.to_string();
    let input = block
        .get("input")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();
    Some(ToolCall {
        id,
        call_type: "function".to_string(),
        function: ToolCallFunction {
            name,
            arguments: input,
        },
    })
}

/// 聚合 content 数组中的 tool_result 块 → ToolResult（recId=8①）。
///
/// Anthropic 的并行工具调用会在同一 user 消息放多个 tool_result 块：
/// 首块的 tool_use_id 为准，各块 content（string 或 blocks 数组）以换行
/// 拼接；UnifiedRequest 的 Message 只承载单个 ToolResult，多结果合并是
/// 首版折衷。
fn aggregate_anthropic_tool_result(arr: &[Value]) -> ToolResult {
    let mut tool_call_id = String::new();
    let mut contents: Vec<String> = Vec::new();
    let mut is_error = false;

    for block in arr
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
    {
        if tool_call_id.is_empty() {
            tool_call_id = block
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
        match block.get("content") {
            Some(Value::String(s)) => contents.push(s.clone()),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        contents.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
        if block.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
            is_error = true;
        }
    }

    ToolResult {
        tool_call_id,
        function_name: String::new(),
        content: contents.join("\n"),
        is_error,
    }
}

/// 提取 System 消息文本 → Anthropic 顶层 system 参数（recId=8④）。
/// 多条 System 消息以空行连接。
fn extract_system_text(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| match &m.content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::MultiModal(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Anthropic stop_reason → OpenAI finish_reason 静态映射（recId=8⑤）。
/// IR 的 finish_reason 统一存 OpenAI 词汇；未知值原样保留。
fn anthropic_stop_reason_to_finish_reason(s: &str) -> String {
    match s {
        "tool_use" => "tool_calls".to_string(),
        "max_tokens" => "length".to_string(),
        "end_turn" | "stop_sequence" => "stop".to_string(),
        other => other.to_string(),
    }
}

/// OpenAI finish_reason → Anthropic stop_reason 静态映射（recId=8⑤ 反向）。
/// end_turn 与 stop_sequence 都归一为 stop，反向取 end_turn（信息损失为
/// 静态映射表固有；未知值原样保留）。
fn finish_reason_to_anthropic_stop_reason(s: &str) -> String {
    match s {
        "tool_calls" => "tool_use".to_string(),
        "length" => "max_tokens".to_string(),
        "stop" => "end_turn".to_string(),
        other => other.to_string(),
    }
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
            // Anthropic 规范字段为 tool_use_id（评审 major：此前误写
            // tool_call_id，缺必填字段会被上游 400 拒收；recId=8① 使本
            // 分支首次可达）
            "tool_use_id": result.tool_call_id,
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
