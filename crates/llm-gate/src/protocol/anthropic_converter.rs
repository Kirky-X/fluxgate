// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// Anthropic 协议已显式映射进 IR 的顶层键（其余字段吸收进 vendor_extensions）。
///
/// 不含 `effort`（M2）：Anthropic 无原生 effort，入站吸收进扩展表、出站经
/// 扩展表显式透传（denylist 不含 effort = opt-in）；该常量同时用作出站
/// merge 的 denylist（known_keys 反用）——协议键永不由扩展表补入/覆写。
const ANTHROPIC_KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system",
    "system_prompt",
    "max_tokens",
    "max_tokens_to_sample",
    "stream",
    "stop_sequences",
    "temperature",
    "top_p",
    "top_k",
    "tools",
    "tool_choice",
    "thinking",
    "turn_index",
    "sampling",
    "thinking_config",
    "vendor_extensions",
];

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

    fn to_unified(&self, body: Value) -> Result<UnifiedRequest, GateError> {
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or(GateError::Validation {
                field: "model".into(),
            })?
            .to_lowercase();

        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or(GateError::Validation {
                field: "messages".into(),
            })?
            .iter()
            .map(parse_anthropic_message)
            .collect::<Result<Vec<_>, _>>()?;

        // recId=8④：顶层 system 参数 → 首条 System 消息（此前
        // `get("system")` 全仓零命中，system 指令整体丢失）。
        // Anthropic 的 system 支持 string 或 content blocks 数组两种形状。
        // T003：同一文本双写 system_prompt（规范槽，单一事实源）；
        // Role::System 消息保留为兼容形态。
        let mut all_messages = Vec::new();
        let mut system_prompt: Option<String> = None;
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
                system_prompt = Some(text.clone());
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
        // T003：原生形状 any/tool/auto/none 直读（any 即「必须用工具」，
        // 映射 IR required）；OpenAI function 对象形状保留容错；标量兜底 auto。
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
                    match obj.get("type").and_then(|t| t.as_str()) {
                        Some("any") => ToolChoice::Strategy("required".to_string()),
                        Some("tool") => ToolChoice::Specific {
                            choice_type: "function".to_string(),
                            function: ToolChoiceFunction {
                                name: obj
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            },
                        },
                        // auto/none 等原词进 Strategy，出站还原同名形状
                        Some(other) => ToolChoice::Strategy(other.to_string()),
                        None => ToolChoice::Strategy("auto".to_string()),
                    }
                }
            }
            _ => ToolChoice::Strategy("auto".to_string()),
        });

        // T003：temperature/top_p/top_k → sampling（core 对齐槽）；
        // temperature 同时保留 flat 槽双写（既有行为兼容）
        let wire_temperature = body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32);
        let wire_top_p = body.get("top_p").and_then(|v| v.as_f64()).map(|f| f as f32);
        let wire_top_k = body.get("top_k").and_then(|v| v.as_u64()).map(|u| u as u32);
        let sampling = if wire_temperature.is_some() || wire_top_p.is_some() || wire_top_k.is_some()
        {
            Some(SamplingParams {
                temperature: wire_temperature,
                top_k: wire_top_k,
                top_p: wire_top_p,
                frequency_penalty: None,
                presence_penalty: None,
            })
        } else {
            None
        };

        // T003：thinking → thinking_config（enabled 无预算 → Adaptive，
        // 「启用但不定预算」在 IR 的最近表达）；budget_tokens u64 对齐 core
        let thinking_config =
            body.get("thinking")
                .and_then(|v| v.as_object())
                .and_then(|t| match t.get("type").and_then(|v| v.as_str()) {
                    Some("enabled") => {
                        Some(match t.get("budget_tokens").and_then(|v| v.as_u64()) {
                            Some(budget) => ThinkingConfig::Enabled {
                                budget_tokens: budget,
                            },
                            None => ThinkingConfig::Adaptive,
                        })
                    }
                    Some("disabled") => Some(ThinkingConfig::Disabled),
                    _ => None,
                });

        // M2：effort 不进 IR（Anthropic 无原生概念）——入站落入
        // vendor_extensions，出站经扩展表显式透传（opt-in）

        Ok(UnifiedRequest {
            model,
            messages: all_messages,
            system_prompt,
            temperature: wire_temperature,
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
            top_p: wire_top_p,
            frequency_penalty: None,
            presence_penalty: None,
            logit_bias: HashMap::new(),
            user: None,
            sampling,
            thinking_config,
            effort: None,
            turn_index: None,
            tools,
            tool_choice,
            vendor_extensions: absorb_vendor_extensions(&body, ANTHROPIC_KNOWN_KEYS),
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, GateError> {
        // recId=8④：Role::System 消息提升为 Anthropic 顶层 system 参数
        //（Anthropic messages 数组不接受 system 角色，直接放入会被上游拒收）。
        // T003：system_prompt 为规范槽，出站优先合并——system_prompt 在前，
        // 与 Role::System 消息文本去重合并（相同文本只保留一份）。
        let mut system_parts = system_message_parts(&request.messages);
        if let Some(system_prompt) = request.system_prompt.as_ref().filter(|s| !s.is_empty()) {
            system_parts.retain(|text| text != system_prompt);
            system_parts.insert(0, system_prompt.clone());
        }
        let system_text = system_parts.join("\n\n");
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

        // T003：Anthropic tool_choice 出站写原生形状（required→any；
        // Specific→tool；unknown 策略保守回退 auto），不再写 OpenAI 形状
        if let Some(tool_choice) = request.tool_choice {
            // L3："none" 省略字段——Anthropic 传统词汇为 auto/any/tool，
            // 无 none 形状；缺省即 auto 语义，不写未知词防上游 400
            let choice = match &tool_choice {
                ToolChoice::Strategy(s) => match s.as_str() {
                    "required" | "any" => Some(json!({"type": "any"})),
                    "none" => None,
                    _ => Some(json!({"type": "auto"})),
                },
                ToolChoice::Specific { function, .. } => {
                    Some(json!({"type": "tool", "name": function.name}))
                }
            };
            if let Some(choice) = choice {
                body["tool_choice"] = choice;
            }
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
        // T003：sampling 补 Anthropic 缺省槽——top_k 直出；temperature/top_p
        // 仅在 flat 槽未设置时补（flat 优先，不覆写）
        if let Some(sampling) = &request.sampling {
            if let Some(top_k) = sampling.top_k {
                body["top_k"] = json!(top_k);
            }
            if request.temperature.is_none() {
                if let Some(temp) = sampling.temperature {
                    body["temperature"] = json!(temp);
                }
            }
            if request.top_p.is_none() {
                if let Some(top_p) = sampling.top_p {
                    body["top_p"] = json!(top_p);
                }
            }
        }
        // T003：thinking_config → thinking 块。Enabled 带预算出站；
        // Adaptive 与 Disabled 保守省略（Anthropic enabled 必须带
        // budget_tokens，缺预算会被 400 拒收；disabled 为协议缺省——
        // 待裁决项，见任务返回）
        if let Some(ThinkingConfig::Enabled { budget_tokens }) = request.thinking_config {
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget_tokens});
        }
        // M2：effort 不出站——Anthropic 无原生 effort，first-party 严格
        // 校验会 400（原「透传给兼容上游」的前提失实）；effort 仅经
        // vendor_extensions 显式携带时可达上游（denylist 不含 effort =
        // opt-in，400 风险自担）
        // F1/M1：vendor_extensions 顶层透传，denylist = 本协议 known_keys
        // 反用——协议键永不由扩展表补入/覆写，非协议键仍透传（D2 语义）
        merge_vendor_extensions(&mut body, &request.vendor_extensions, ANTHROPIC_KNOWN_KEYS);

        Ok(body)
    }

    fn response_to_external(&self, response: UnifiedResponse) -> Result<Value, GateError> {
        let choice = response.choices.first().ok_or(GateError::Validation {
            field: "choices".into(),
        })?;

        let content = choice
            .message
            .as_ref()
            .map(message_to_anthropic)
            .unwrap_or_else(|| json!({ "content": "" }));

        // T003：finish 结构化槽优先（未知词只在原始槽时走字符串映射）
        let stop_reason = choice
            .finish
            .map(|finish| finish_reason_to_anthropic_stop_reason(finish.to_wire()))
            .or_else(|| {
                choice
                    .finish_reason
                    .as_deref()
                    .map(finish_reason_to_anthropic_stop_reason)
            })
            .unwrap_or_else(|| "end_turn".into());

        // T003：IR cache 字段 → cache_read/creation_input_tokens（非零才携带；
        // cost 无 Anthropic 槽，不产出）
        let mut usage = json!({
            "input_tokens": response.usage.prompt_tokens,
            "output_tokens": response.usage.completion_tokens,
        });
        if response.usage.cache_read_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(response.usage.cache_read_tokens);
        }
        if response.usage.cache_creation_tokens > 0 {
            usage["cache_creation_input_tokens"] = json!(response.usage.cache_creation_tokens);
        }

        Ok(json!({
            "id": response.id,
            "type": "message",
            "role": "assistant",
            "content": content,
            "model": response.model,
            "stop_reason": stop_reason,
            "stop_sequence": Value::Null,
            "usage": usage,
        }))
    }

    fn parse_response(&self, response: Value) -> Result<UnifiedResponse, GateError> {
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
            return Err(GateError::Protocol {
                source: t_args(
                    "protocol-anthropic-error",
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
        // OpenAI 词汇；未知值原样保留）。T003：已知词双写结构化槽。
        let stop_reason = response
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(anthropic_stop_reason_to_finish_reason);
        let finish = stop_reason.as_deref().and_then(FinishReason::from_wire);

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

        // T003：cache_read/creation_input_tokens → IR cache 字段（u64 对齐 core）
        let cache_read_tokens = usage
            .and_then(|v| v.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_creation_tokens = usage
            .and_then(|v| v.get("cache_creation_input_tokens"))
            .and_then(|v| v.as_u64())
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
                finish,
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                cost_usd: None,
                cost_cny: None,
            },
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, GateError> {
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
                            ContentBlock::Image { source, detail: _ } => match source {
                                ImageSource::Base64 { media_type, data } => json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data,
                                    },
                                }),
                                ImageSource::Url { url } => json!({
                                    "type": "image",
                                    "source": {
                                        "type": "url",
                                        "url": url,
                                    },
                                }),
                            },
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
                    // T004：IR finish 词表 → Anthropic stop_reason 词表
                    //（stop→end_turn 等；未知词原样透传）
                    "stop_reason": finish_reason
                        .as_deref()
                        .map(finish_reason_to_anthropic_stop_reason),
                }))
            }
            // T004：无状态单块路径——首片段（name 存在性，本方法无跨帧
            // 状态只能启发式判定）→ content_block_start(tool_use)，
            // 续片段 → content_block_delta(input_json_delta)。规范化交错
            // 编码走 [`AnthropicStreamEncoder`]（显式缓冲状态）
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                if let Some(name) = name {
                    Ok(json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {},
                        },
                    }))
                } else {
                    Ok(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": arguments.unwrap_or_default(),
                        },
                    }))
                }
            }
            StreamChunk::Usage {
                prompt_tokens: _,
                completion_tokens,
                total_tokens: _,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
            } => {
                // T004：finish_reason 随 Usage 到达时透传进 delta.stop_reason
                //（IR 词表 → Anthropic 词表；未知词原样透传）
                let delta = json!({
                    "stop_reason": finish_reason
                        .as_deref()
                        .map(finish_reason_to_anthropic_stop_reason),
                    "stop_sequence": Value::Null,
                });
                let mut usage = json!({
                    "output_tokens": completion_tokens,
                });
                if cache_read_tokens > 0 {
                    usage["cache_read_input_tokens"] = json!(cache_read_tokens);
                }
                if cache_creation_tokens > 0 {
                    usage["cache_creation_input_tokens"] = json!(cache_creation_tokens);
                }
                Ok(json!({
                    "type": "message_delta",
                    "delta": delta,
                    "usage": usage,
                }))
            }
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
/// - `content_block_start`（tool_use）/ `content_block_delta`
///   （input_json_delta）→ [`StreamChunk::ToolCall`]（T004 表示统一：
///   首片段带 id/name，续片段为 arguments 增量；片段类别由帧类型显式
///   判定，并行工具 = 多 index 交错流，消费方按显式聚合状态拼接）；
/// - `message_delta`（usage 尾帧）→ [`StreamChunk::Usage`]（token 记账
///   依赖它；其 `delta.stop_reason` 同帧映射进 Usage 的 `finish_reason`，
///   流式 stop_reason 透传）；
/// - `ping` → [`StreamChunk::Heartbeat`]；
/// - `message_start`/`content_block_start`（text）/`content_block_stop`/
///   `message_stop` → `None`（生命周期事件：块收口语义由消费方聚合状态
///   持有、终止按 stop_reason/流尾判定，事件序由出站侧
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
        "content_block_start" => {
            let block = json.get("content_block")?;
            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                // text 等非工具块为生命周期事件，由出站状态机重建
                return None;
            }
            // T004（F3 收口）：片段类别由帧类型显式判定——
            // content_block_start 恒为首片段（id/name 必带），不依赖
            // name 存在性启发式；参数增量在 content_block_delta
            Some(StreamChunk::ToolCall {
                index: json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                id: block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                name: Some(
                    block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                ),
                arguments: None,
            })
        }
        "content_block_delta" => {
            let delta = json.get("delta")?;
            // T004：工具参数增量 → ToolCall 续片段（id/name 由首片段携带；
            // 片段类别由 delta.type 显式判定，不依赖 name 存在性）
            if delta.get("type").and_then(|v| v.as_str()) == Some("input_json_delta") {
                let partial = delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                return Some(StreamChunk::ToolCall {
                    index: json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    id: String::new(),
                    name: None,
                    arguments: Some(partial.to_string()),
                });
            }
            // 文本增量（非 text 非工具增量不产出事件）
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
        "content_block_stop" => {
            // T004：块收口为生命周期事件，不映射 IR——消费方按显式聚合
            // 状态（index → {id, name, arguments 缓冲}）持有调用，终止以
            // stop_reason（message_delta）或流尾判定；事件序由出站状态机重建
            None
        }
        "message_delta" => {
            // T003：delta.stop_reason 同帧映射（end_turn/stop_sequence→Stop、
            // max_tokens→Length、tool_use→ToolCalls；未知词原样透传）
            let finish_reason = json
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(anthropic_stop_reason_to_finish_reason);
            let usage = json.get("usage");
            let output = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let input = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let cache_read_tokens = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64())
                .map(|u| u as u32)
                .unwrap_or(0);
            let cache_creation_tokens = usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_u64())
                .map(|u| u as u32)
                .unwrap_or(0);
            Some(StreamChunk::Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
            })
        }
        "ping" => Some(StreamChunk::Heartbeat),
        _ => None,
    }
}

/// Anthropic 出站流式状态机（recId=3 阶段二③；T004 工具流统一）。
///
/// IR StreamChunk 流不携带 Anthropic 流式协议的生命周期事件 —— 直接逐块
/// [`ProtocolConverter::stream_chunk_to_external`] 输出的
/// `content_block_delta` 不构成合法 Anthropic 流（缺
/// message_start/content_block_start/content_block_stop/message_stop）。
/// 本状态机在 IR 事件之上补齐完整生命周期：
///
/// ```text
/// message_start（首个 Content/Usage/工具块前发一次）
///   [工具块刷写（见下）]
///   content_block_start（首个 text delta 前发）
///     content_block_delta*（每个 text delta）
///   content_block_stop（finish_reason 或 Usage 到达时闭合）
/// message_delta（Usage 到达：stop_reason + output_tokens）
/// message_stop（finish 时）
/// ```
///
/// T004 工具流：[`StreamChunk::ToolCall`] 片段先进入按 IR 工具槽位
/// （index）组织的缓冲——Anthropic 顺序块语义下同一时刻至多一个 open
/// block，并行多 id/index 交错无法直接映射为事件，缓冲到刷写点（下一个
/// Content/Usage 到达或 finish 收尾）后按槽位序串行化为
/// content_block_start(tool_use) + input_json_delta 分片* +
/// content_block_stop；刷写点先于文本块（工具块在前为归一化顺序，文本块
/// 如在其后重新开块，块序仍合法）。块序号由编码器内部计数器分配，与
/// IR chunk index 无关。
#[derive(Debug)]
pub struct AnthropicStreamEncoder {
    message_started: bool,
    block_open: bool,
    block_index: u32,
    stop_reason: Option<String>,
    /// 待刷写工具块缓冲：IR 工具槽位 index → 缓冲（T004 交错状态跟踪）
    pending_tools: BTreeMap<u32, ToolBuffer>,
}

/// 待刷写工具块缓冲（T004：并行多 id/index 交错的显式状态跟踪，
/// 取代 name 存在性判定）。
#[derive(Debug, Default)]
struct ToolBuffer {
    id: String,
    name: Option<String>,
    /// 按到达序保留的 arguments 分片（刷写时逐片还原 input_json_delta）
    arguments_shards: Vec<String>,
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
            pending_tools: BTreeMap::new(),
        }
    }

    /// 刷写待发工具块缓冲（T004）：按 IR 工具槽位序产出合法事件序列——
    /// 必要时先闭合当前 open 的文本块，随后
    /// content_block_start(tool_use) + input_json_delta 分片* +
    /// content_block_stop；块序号取编码器内部计数器并逐块递增。
    fn flush_pending_tools(&mut self, events: &mut Vec<String>) {
        // F1：mem::take 拿所有权直接迭代（BTreeMap 键序 = 槽位序），
        // 免临时 key Vec 与逐项 remove
        for (slot, buffer) in std::mem::take(&mut self.pending_tools) {
            // LOW-4：id 与 name 均空的退化槽为畸形输入，跳过并显性化
            //（与 openai 入站解析对畸形片段的丢弃行为对称）
            if buffer.id.is_empty() && buffer.name.as_deref().unwrap_or("").is_empty() {
                tracing::warn!(slot, "丢弃畸形工具缓冲：id 与 name 均空");
                continue;
            }
            if self.block_open {
                events.push(self.content_block_stop_event());
                self.block_open = false;
                self.block_index += 1;
            }
            if !self.message_started {
                events.push(self.message_start_event());
            }
            events.push(
                json!({
                    "type": "content_block_start",
                    "index": self.block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": buffer.id,
                        "name": buffer.name.unwrap_or_default(),
                        "input": {},
                    },
                })
                .to_string(),
            );
            for shard in buffer.arguments_shards {
                events.push(
                    json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": shard,
                        },
                    })
                    .to_string(),
                );
            }
            events.push(self.content_block_stop_event());
            self.block_index += 1;
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
                // MEDIUM 修复：非规范回退形状 Content{ToolCalls}（openai
                // 多片段帧回退产物）的工具载荷不再静默丢弃——整段 arguments
                // 合成为一片 ToolCall 入既有缓冲，走刷写路径产出合法 tool_use 块
                if let MessageContent::ToolCalls(calls) = &delta {
                    for call in calls {
                        let slot = self
                            .pending_tools
                            .keys()
                            .copied()
                            .next_back()
                            .map_or(0, |k| k + 1);
                        let buffer = self.pending_tools.entry(slot).or_default();
                        buffer.id = call.id.clone();
                        buffer.name = Some(call.function.name.clone()).filter(|n| !n.is_empty());
                        if !call.function.arguments.is_empty() {
                            buffer
                                .arguments_shards
                                .push(call.function.arguments.clone());
                        }
                    }
                }
                let text = extract_delta_text(&delta);
                if let Some(reason) = finish_reason {
                    self.stop_reason = Some(reason);
                }

                let mut events = Vec::new();
                if !self.message_started {
                    events.push(self.message_start_event());
                }
                // T004：文本块打开前先刷写工具缓冲（工具块在前为归一化
                // 顺序；文本块如在其后重新开块，块序仍合法）
                self.flush_pending_tools(&mut events);
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
                finish_reason,
                ..
            } => {
                let mut events = Vec::new();
                // T003：终止原因与 usage 同帧到达时（Anthropic 上游回流），
                // 先更新 stop_reason 再发 message_delta
                if let Some(reason) = finish_reason {
                    self.stop_reason = Some(reason);
                }
                if !self.message_started {
                    events.push(self.message_start_event_with_input(prompt_tokens));
                }
                // T004：message_delta 前必须刷写完所有工具块（顺序块语义）
                self.flush_pending_tools(&mut events);
                if self.block_open {
                    events.push(self.content_block_stop_event());
                    self.block_open = false;
                }
                events.push(
                    json!({
                        "type": "message_delta",
                        "delta": {
                            // T004：IR finish 词表 → Anthropic stop_reason 词表
                            //（stop→end_turn、tool_calls→tool_use 等；未知词透传）
                            "stop_reason": self
                                .stop_reason
                                .as_deref()
                                .map(finish_reason_to_anthropic_stop_reason),
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
            // T004：工具片段先缓冲——并行多 id/index 交错不能直接映射为
            // Anthropic 事件（顺序块语义），缓冲到刷写点（下一个
            // Content/Usage 到达或 finish 收尾）按槽位序串行化。
            // 无状态单块路径见 [`ProtocolConverter::stream_chunk_to_external`]。
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                let buffer = self.pending_tools.entry(index).or_default();
                if !id.is_empty() {
                    buffer.id = id;
                }
                // LOW-2：name 覆盖仅在非空时——Some("") 不得降级既有合法 name
                if name.as_deref().is_some_and(|n| !n.is_empty()) {
                    buffer.name = name;
                }
                if let Some(shard) = arguments {
                    buffer.arguments_shards.push(shard);
                }
                Vec::new()
            }
            StreamChunk::Heartbeat => vec![json!({ "type": "ping" }).to_string()],
        }
    }

    /// 流收尾：补齐未闭合的 content_block_stop 与 message_stop。
    ///
    /// 本方法只发 message_stop，不发 message_delta——stop_reason 仅随
    /// Usage 帧到达时经 message_delta 上线；无 Usage 帧的流其 stop_reason
    /// 不上线为已知边界（T004 审查 LOW-3 如实记录）。
    /// 空流产出完整 message_start + message_stop（合法的最小 Anthropic 流）。
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.message_started {
            events.push(self.message_start_event());
        }
        // T004：收尾前刷写残留工具缓冲
        self.flush_pending_tools(&mut events);
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
        // 旧表示遗留（仅非规范回退形状可达）：ToolCalls 载荷已由 Content
        // 臂合成进工具缓冲（不产出流式文本）；ToolResult 仍不产出
        _ => String::new(),
    }
}

fn parse_anthropic_message(json: &Value) -> Result<Message, GateError> {
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
            return Err(GateError::Validation {
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

/// 提取 System 消息文本片段（T003：返回分片列表供出站去重合并——
/// system_prompt 优先在前，相同文本剔除）。非文本内容取文本块拼接，
/// 空文本与纯非文本消息被过滤。
fn system_message_parts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| match &m.content {
            MessageContent::Text(s) if !s.is_empty() => Some(s.clone()),
            MessageContent::MultiModal(blocks) => {
                let joined = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if joined.is_empty() {
                    None
                } else {
                    Some(joined)
                }
            }
            _ => None,
        })
        .collect()
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
            // T003：base64 与 url 两种 source 形态；未知形态不猜变体
            let source = match source.get("type").and_then(|v| v.as_str())? {
                "base64" => ImageSource::Base64 {
                    media_type: source.get("media_type")?.as_str()?.to_string(),
                    data: source.get("data")?.as_str()?.to_string(),
                },
                "url" => ImageSource::Url {
                    url: source.get("url")?.as_str()?.to_string(),
                },
                _ => return None,
            };
            Some(ContentBlock::Image {
                source,
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
                    ContentBlock::Image { source, detail: _ } => match source {
                        ImageSource::Base64 { media_type, data } => json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            },
                        }),
                        ImageSource::Url { url } => json!({
                            "type": "image",
                            "source": {
                                "type": "url",
                                "url": url,
                            },
                        }),
                    },
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
