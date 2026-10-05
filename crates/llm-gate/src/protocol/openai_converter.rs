// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::*;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use serde_json::{json, Value};
use std::collections::HashMap;

/// OpenAI 协议已显式映射进 IR 的顶层键（其余字段吸收进 vendor_extensions）。
///
/// 同时用作出站 merge 的 denylist（F1/M1，known_keys 反用）：协议键永不
/// 由扩展表补入/覆写——stream 缺位注入、effort 经扩展表物化均被拦截。
const OPENAI_KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system_prompt",
    "temperature",
    "max_tokens",
    "stream",
    "stop",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "logit_bias",
    "user",
    "sampling",
    "thinking_config",
    "effort",
    "turn_index",
    "tools",
    "tool_choice",
    "vendor_extensions",
    "reasoning_effort",
];

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
            .map(parse_openai_message)
            .collect::<Result<Vec<_>, _>>()?;

        // 解析工具定义
        let tools: Vec<Tool> = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(parse_openai_tool).collect())
            .unwrap_or_default();
        // 工具 schema 体积/深度上限（A3 审查 F4 纵深）：超限请求级 400
        validate_tool_schemas(&tools)?;

        // 解析工具选择策略
        let tool_choice = body.get("tool_choice").map(parse_tool_choice);

        // T003：reasoning_effort → effort（OpenAI wire 词汇即 low/medium/high）
        let effort = body
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .and_then(EffortLevel::from_wire);

        Ok(UnifiedRequest {
            model,
            messages,
            // OpenAI 无顶层 system 参数（系统指令以 Role::System 消息表达），
            // system_prompt 恒 None，出站反向合并见 request_to_external
            system_prompt: None,
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
            // flat 槽已承接 temperature/top_p/penalties；sampling 不双写
            // （OpenAI 无 top_k 概念，该类非标参数走 vendor_extensions 透传）
            sampling: None,
            thinking_config: None,
            effort,
            turn_index: None,
            tools,
            tool_choice,
            // 未识别顶层字段（top_k/seed 等）吸收进扩展表，出站原样透传防丢
            vendor_extensions: absorb_vendor_extensions(&body, OPENAI_KNOWN_KEYS),
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, GateError> {
        // 将 UnifiedRequest 转换为 OpenAI 格式。
        // system_prompt 与 messages 内 Role::System 并存时，system_prompt
        // 优先合并去重：相同文本只保留一份，不同文本并入首条 system 消息
        // （system_prompt 在前）；仅当 system_prompt 存在时重排，否则消息
        // 原位映射（行为兼容）。
        let mut source_messages = request.messages;
        let messages: Vec<Value> =
            if let Some(system_prompt) = request.system_prompt.filter(|s| !s.is_empty()) {
                let mut parts = vec![system_prompt.clone()];
                source_messages.retain(|msg| {
                    if msg.role != Role::System {
                        return true;
                    }
                    match &msg.content {
                        MessageContent::Text(text) if !text.is_empty() => {
                            if *text == system_prompt {
                                // 与 system_prompt 相同文本：去重剔除
                                false
                            } else {
                                // 其余 System 文本并入首条 system 消息
                                parts.push(text.clone());
                                false
                            }
                        }
                        // 非文本 System 消息原位保留（不参与合并）
                        _ => true,
                    }
                });
                let mut merged = vec![Message {
                    role: Role::System,
                    content: MessageContent::Text(parts.join("\n\n")),
                }];
                merged.extend(source_messages);
                merged.into_iter().map(message_to_openai).collect()
            } else {
                source_messages.into_iter().map(message_to_openai).collect()
            };

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
        // T003：effort → reasoning_effort（OpenAI wire 词汇，小写）
        if let Some(effort) = request.effort {
            body["reasoning_effort"] = json!(effort.to_wire());
        }
        // T003：sampling 只补 flat 未设置的槽（flat 优先，不覆写）；
        // top_k 非 OpenAI 标准参数，直出供兼容上游取用
        if let Some(sampling) = &request.sampling {
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
            if request.frequency_penalty.is_none() {
                if let Some(fp) = sampling.frequency_penalty {
                    body["frequency_penalty"] = json!(fp);
                }
            }
            if request.presence_penalty.is_none() {
                if let Some(pp) = sampling.presence_penalty {
                    body["presence_penalty"] = json!(pp);
                }
            }
            if let Some(top_k) = sampling.top_k {
                body["top_k"] = json!(top_k);
            }
        }
        // L1：thinking_config 无 OpenAI 等价槽，出站静默丢弃（跨厂商
        // fallback 到 OpenAI 时思考预算不透传，属已知降级而非错误）
        // F1/M1：vendor_extensions 顶层透传，denylist = 本协议 known_keys
        // 反用——协议键（stream/reasoning_effort 等）永不由扩展表补入/覆写，
        // 非协议键仍透传（D2 语义；跨协议严格上游 400 风险为显式 opt-in，D7）
        merge_vendor_extensions(&mut body, &request.vendor_extensions, OPENAI_KNOWN_KEYS);

        Ok(body)
    }

    fn response_to_external(&self, response: UnifiedResponse) -> Result<Value, GateError> {
        let choices = response
            .choices
            .into_iter()
            .map(|mut choice| {
                // 结构化槽优先回填原始槽（finish 有值且原始槽为空时）
                if choice.finish_reason.is_none() {
                    choice.finish_reason = choice.finish.map(|f| f.to_wire().to_string());
                }
                json!({
                    "index": choice.index,
                    "message": choice.message.map(message_to_openai),
                    "finish_reason": choice.finish_reason,
                })
            })
            .collect::<Vec<_>>();

        let mut usage = json!({
            "prompt_tokens": response.usage.prompt_tokens,
            "completion_tokens": response.usage.completion_tokens,
            "total_tokens": response.usage.total_tokens,
        });
        // T003：cache_read_tokens → prompt_tokens_details.cached_tokens
        //（非零才携带；cache_creation/cost 无 OpenAI 槽，不产出）
        if response.usage.cache_read_tokens > 0 {
            usage["prompt_tokens_details"] =
                json!({"cached_tokens": response.usage.cache_read_tokens});
        }

        Ok(json!({
            "id": response.id,
            "object": "chat.completion",
            "created": response.created,
            "model": response.model,
            "choices": choices,
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
                // T003：已知词双写结构化槽；未知词只留原始透传槽
                let finish = finish_reason.as_deref().and_then(FinishReason::from_wire);

                Some(Choice {
                    index: index as u32,
                    message,
                    finish_reason,
                    finish,
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

        // T003：prompt_tokens_details.cached_tokens → cache_read_tokens
        //（OpenAI 无 cache_creation 概念，恒 0；u64 对齐 core Usage 宽度）
        let cache_read_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

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
                cache_read_tokens,
                cache_creation_tokens: 0,
                cost_usd: None,
                cost_cny: None,
            },
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, GateError> {
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
            // T003：流式工具调用片段 → delta.tool_calls（id/name 仅首片段携带）
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                let mut fragment = json!({
                    "index": index,
                    "type": "function",
                });
                if !id.is_empty() {
                    fragment["id"] = json!(id);
                }
                if name.is_some() || arguments.is_some() {
                    let mut function = json!({});
                    if let Some(name) = &name {
                        function["name"] = json!(name);
                    }
                    if let Some(arguments) = &arguments {
                        function["arguments"] = json!(arguments);
                    }
                    fragment["function"] = function;
                }
                Ok(json!({
                    "id": format!("chunk-{}", index),
                    "object": "chat.completion.chunk",
                    "created": chrono::Utc::now().timestamp() as u64,
                    "model": "unknown",
                    "choices": [{
                        "index": index,
                        "delta": {"tool_calls": [fragment]},
                        "finish_reason": Value::Null,
                    }],
                }))
            }
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
                cache_creation_tokens: _,
                finish_reason,
            } => {
                // cache_creation 无 OpenAI 槽。
                // 跨厂商合成（A4/T012，D7.13 扩项闭环）：finish_reason 有值
                // 时（Anthropic 上游 stop_reason 经 T007 映射进 IR Usage 帧）
                // 写入本帧 choices 终止槽——OpenAI wire 合法形状（usage 帧
                // 可携带收尾 choices），OpenAI 面客户端自此能拿到跨厂商终止
                // 原因；None（OpenAI 原生上游，终止先行于 usage 帧）保持
                // 空 choices 原样透传，零回归。
                let mut usage = json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens,
                });
                if cache_read_tokens > 0 {
                    usage["prompt_tokens_details"] = json!({"cached_tokens": cache_read_tokens});
                }
                // IR Usage.finish_reason 已是归一化 OpenAI wire 词
                //（anthropic_stop_reason_to_finish_reason；未知词原样保真）
                let choices = match &finish_reason {
                    Some(fr) => json!([{
                        "index": 0,
                        "delta": {},
                        "finish_reason": fr,
                    }]),
                    None => json!([]),
                };
                Ok(json!({
                    "id": "usage",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "unknown",
                    "choices": choices,
                    "usage": usage,
                }))
            }
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
/// - `choices[0].delta.tool_calls`（规范单片段帧）→
///   [`StreamChunk::ToolCall`]（T004 表示统一：id/name 仅首片段携带，
///   arguments 为增量分片；多片段帧或与 finish_reason 同帧回退
///   Content{ToolCalls} 旧聚合表示，无损交付）；
/// - `choices[0].delta.content` → [`StreamChunk::Content`]（Text delta）；
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
            // T003：cached_tokens 同帧读取（cache_creation 无 OpenAI 概念）
            let cache_read_tokens = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .map(|u| u as u32)
                .unwrap_or(0);
            return Some(StreamChunk::Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total,
                cache_read_tokens,
                cache_creation_tokens: 0,
                finish_reason: None,
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

    // T004：表示统一——tool_calls delta → ToolCall 片段流。
    // 规范形状：每帧恰一个片段（OpenAI 实践形状）→ [`StreamChunk::ToolCall`]
    // {index, id（续片缺省为空串）, name: Option, arguments: Option 增量}，
    // 消费方以显式聚合状态（index → {id, name, arguments 缓冲}）拼接。
    // 非规范形状回退旧聚合表示 [`MessageContent::ToolCalls`]（无损交付，
    // trait 每次 `parse_stream_frame` 只能产出一个 chunk）：
    // - 单帧多片段（数组 len > 1，部分兼容端点会合并并行工具首片段）；
    // - 片段与 finish_reason 同帧（ToolCall 片段无终止槽）。
    let tool_array = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|v| v.as_array())
        .filter(|arr| !arr.is_empty());
    if let Some(arr) = tool_array {
        if arr.len() == 1 && finish_reason.is_none() {
            if let Some((tool_index, id, name, arguments)) =
                parse_stream_tool_call_fragment(&arr[0])
            {
                return Some(StreamChunk::ToolCall {
                    index: tool_index,
                    id,
                    name,
                    arguments,
                });
            }
        }
    }

    // 回退路径：多片段帧 / 与 finish_reason 同帧 / 片段形状非法
    let tool_call_delta = tool_array
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

/// 解析流式 tool_calls 单片段 → ToolCall 片段字段（T004 表示统一）。
///
/// OpenAI 流式形状：id/name 仅首片段携带（续片缺省 → 空串/None），
/// arguments 为增量分片；index 为并行工具槽位（消费方聚合状态按它拼接）。
fn parse_stream_tool_call_fragment(
    json: &Value,
) -> Option<(u32, String, Option<String>, Option<String>)> {
    let obj = json.as_object()?;
    let function = obj.get("function").and_then(|f| f.as_object());
    let name = function
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let arguments = function
        .and_then(|f| f.get("arguments"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some((
        obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        obj.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name,
        arguments,
    ))
}

/// 旧聚合表示路径的片段解析（T004：仅服务非规范形状回退——单帧多片段 /
/// 与 finish_reason 同帧）。片段缺省字段以空串占位。
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

fn parse_openai_message(json: &Value) -> Result<Message, GateError> {
    let role = json
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or(GateError::Validation {
            field: "role".into(),
        })?;

    let role = match role {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => {
            return Err(GateError::Validation {
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
            return Err(GateError::Validation {
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
            // T003：URL 保真——https 直链 → Url 形态（不再硬编码 jpeg、
            // 不再把 URL 塞进 data）；data URI → Base64 形态（media_type
            // 从 URI 解析）
            let source = match url.strip_prefix("data:") {
                Some(rest) => match rest.split_once(";base64,") {
                    Some((media_type, data)) => ImageSource::Base64 {
                        media_type: media_type.to_string(),
                        data: data.to_string(),
                    },
                    None => ImageSource::Url { url },
                },
                None => ImageSource::Url { url },
            };
            Some(ContentBlock::Image {
                source,
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
                    ContentBlock::Image { source, detail } => {
                        // T003：Base64 → data URI；Url → 直链原样（不再包
                        // data: 前缀导致坏 URI）
                        let url = match &source {
                            ImageSource::Base64 { media_type, data } => {
                                format!("data:{media_type};base64,{data}")
                            }
                            ImageSource::Url { url } => url.clone(),
                        };
                        json!({
                            "type": "image_url",
                            "image_url": {
                                "url": url,
                                "detail": detail.clone().unwrap_or_else(|| "auto".into()),
                            },
                        })
                    }
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

/// 解析 OpenAI 格式的工具定义（T008 M1 / D7.14 闭环：parameters 原样
/// `serde_json::Value` 保真透传——不做结构化升降级，嵌套 schema / items /
/// anyOf / $ref / 缺 type 属性等全量无损）。
fn parse_openai_tool(json: &Value) -> Option<Tool> {
    let obj = json.as_object()?;
    let func = obj.get("function")?.as_object()?;
    let parameters = func.get("parameters").cloned().unwrap_or(Value::Null);

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

/// 将工具转换为 OpenAI 格式（parameters 原样保真；未携带（Null）时
/// 省略该键——OpenAI Chat Completions 中 parameters 为可选字段）。
fn tool_to_openai(tool: &Tool) -> Value {
    let mut function = json!({
        "name": tool.function.name,
        "description": tool.function.description,
    });
    if tool.function.parameters.is_object() {
        function["parameters"] = tool.function.parameters.clone();
    }
    json!({
        "type": tool.tool_type,
        "function": function,
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
