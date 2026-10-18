// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use super::*;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use serde_json::{json, Value};
use std::collections::HashMap;

/// OpenAI 协议已显式映射进 IR 的顶层键（其余字段吸收进 vendor_extensions）；
/// 同时用作出站 merge 的 denylist：协议键永不由扩展表补入/覆写
const OPENAI_KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system_prompt",
    "temperature",
    "max_tokens",
    "max_completion_tokens",
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
    "parallel_tool_calls",
    "response_format",
    "metadata",
    "service_tier",
    "verbosity",
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

        // reasoning_effort → effort（全词表 none/minimal/low/medium/high/
        // xhigh/max）
        let reasoning_effort_raw = body.get("reasoning_effort");
        let effort = reasoning_effort_raw
            .and_then(|v| v.as_str())
            .and_then(EffortLevel::from_wire);

        // 扩展表：未识别顶层字段防丢；reasoning_effort 词表外值原样保留
        // （不猜映射、不静默丢弃）
        let mut vendor_extensions = absorb_vendor_extensions(&body, OPENAI_KNOWN_KEYS);
        if effort.is_none() {
            if let Some(raw) = reasoning_effort_raw {
                vendor_extensions.insert("reasoning_effort".to_string(), raw.clone());
            }
        }

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
            stop: match body.get("stop") {
                // OpenAI wire 允许 string 或 array 两种
                // 形态（均予接受）。
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            },
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
            thinking_include_thoughts: None,
            effort,
            turn_index: None,
            tools,
            tool_choice,
            parallel_tool_calls: body.get("parallel_tool_calls").and_then(|v| v.as_bool()),
            response_format: body.get("response_format").cloned(),
            metadata: body.get("metadata").filter(|v| v.is_object()).cloned(),
            // OpenAI service_tier 词表开放（auto/flex/priority/…），原样入槽
            service_tier: body
                .get("service_tier")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            verbosity: body
                .get("verbosity")
                .and_then(|v| v.as_str())
                .and_then(Verbosity::from_wire),
            max_completion_tokens: body
                .get("max_completion_tokens")
                .and_then(|v| v.as_u64())
                .map(|u| u as u32),
            // 未识别顶层字段（top_k/seed 等）吸收进扩展表，出站原样透传防丢
            vendor_extensions,
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, GateError> {
        // 逐字段解构：messages/system_prompt 先行合并消费，其余字段
        // 以本地值参与组包（避免部分移动后的整结构体借用）
        let UnifiedRequest {
            model,
            messages,
            system_prompt,
            tools,
            tool_choice,
            temperature,
            max_tokens,
            stream,
            stop,
            top_p,
            frequency_penalty,
            presence_penalty,
            max_completion_tokens,
            user,
            logit_bias,
            metadata,
            service_tier,
            verbosity,
            effort,
            thinking_config,
            thinking_include_thoughts,
            sampling,
            parallel_tool_calls,
            response_format,
            vendor_extensions,
            ..
        } = request;

        let messages: Vec<Value> = merge_system_prompt_messages(messages, system_prompt)
            .into_iter()
            .map(message_to_openai)
            .collect();

        let mut body = json!({
            "model": model,
            "messages": messages,
        });

        // 添加工具定义
        if !tools.is_empty() {
            let tools: Vec<Value> = tools
                .iter()
                .enumerate()
                .map(|(index, tool)| tool_to_openai(tool, index))
                .collect::<Result<_, _>>()?;
            body["tools"] = json!(tools);
        }

        // 添加工具选择策略
        if let Some(tool_choice) = tool_choice {
            body["tool_choice"] = tool_choice_to_openai(tool_choice);
        }

        if let Some(parallel) = parallel_tool_calls {
            body["parallel_tool_calls"] = json!(parallel);
        }
        if let Some(response_format) = response_format {
            body["response_format"] = response_format;
        }

        if let Some(temp) = temperature {
            body["temperature"] = json!(temp);
        }
        if let Some(max) = max_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(max) = max_completion_tokens {
            body["max_completion_tokens"] = json!(max);
        }
        if stream {
            body["stream"] = json!(true);
        }
        if !stop.is_empty() {
            body["stop"] = json!(stop);
        }
        if let Some(top_p) = top_p {
            body["top_p"] = json!(top_p);
        }
        if let Some(fp) = frequency_penalty {
            body["frequency_penalty"] = json!(fp);
        }
        if let Some(pp) = presence_penalty {
            body["presence_penalty"] = json!(pp);
        }
        if let Some(user) = user {
            body["user"] = json!(user);
        }
        if !logit_bias.is_empty() {
            body["logit_bias"] = logit_bias_to_openai(&logit_bias);
        }
        if let Some(metadata) = metadata.filter(|m| m.is_object()) {
            body["metadata"] = metadata;
        }
        if let Some(service_tier) = service_tier {
            body["service_tier"] = json!(service_tier);
        }
        if let Some(verbosity) = verbosity {
            body["verbosity"] = json!(verbosity.to_wire());
        }
        // effort → reasoning_effort（OpenAI wire 词汇，小写）
        if let Some(effort) = effort {
            body["reasoning_effort"] = json!(effort.to_wire());
        }
        apply_sampling_overrides(
            &mut body,
            temperature,
            top_p,
            frequency_penalty,
            presence_penalty,
            sampling.as_ref(),
        );
        // thinking_config 无 OpenAI 等价槽：显性化丢弃（跨厂商 fallback 到
        // OpenAI 时思考预算不透传，属已知降级而非错误；display 一并丢弃）
        if thinking_config.is_some() || thinking_include_thoughts.is_some() {
            drop_unsupported(
                Protocol::OpenAI,
                "thinking_config",
                "no OpenAI slot (thoughts surface via reasoning_effort/reasoning_content)",
            );
        }
        // F1/：vendor_extensions 顶层透传，denylist = 本协议 known_keys
        // 反用——协议键（stream/reasoning_effort 等）永不由扩展表补入/覆写，
        // 非协议键仍透传（D2 语义；跨协议严格上游 400 风险为显式 opt-in，D7）
        merge_vendor_extensions(&mut body, &vendor_extensions, OPENAI_KNOWN_KEYS);

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
                let mut out = json!({
                    "index": choice.index,
                    "message": choice.message.map(message_to_openai),
                    "finish_reason": choice.finish_reason,
                });
                // logprobs 原样回写（缺省不写出，保持旧形状）
                if let Some(logprobs) = choice.logprobs {
                    out["logprobs"] = logprobs;
                }
                out
            })
            .collect::<Vec<_>>();

        let mut usage = json!({
            "prompt_tokens": response.usage.prompt_tokens,
            "completion_tokens": response.usage.completion_tokens,
            "total_tokens": response.usage.total_tokens,
        });
        // details 明细优先回写；details 缺省时把 legacy cache_read_tokens
        // 并入 cached_tokens（非零才携带；cache_creation/cost 无 OpenAI 槽，
        // 不产出）
        match response.usage.prompt_tokens_details.as_ref() {
            Some(details) => {
                usage["prompt_tokens_details"] = json!(details);
            }
            None if response.usage.cache_read_tokens > 0 => {
                usage["prompt_tokens_details"] =
                    json!({"cached_tokens": response.usage.cache_read_tokens});
            }
            None => {}
        }
        if let Some(details) = response.usage.completion_tokens_details.as_ref() {
            usage["completion_tokens_details"] = json!(details);
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
                        let text = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        // message.tool_calls 解析（同时读
                        // content 与 tool_calls，避免工具调用响应在 IR 中降级为纯文本）
                        let tool_calls =
                            parse_tool_calls_field(&Value::Object(m.clone())).unwrap_or_default();
                        // reasoning_content（DeepSeek/兼容端点）→ Reasoning /
                        // TextWithToolCalls.reasoning：文本或工具任一存在时组合
                        // 进 TextWithToolCalls 保住 reasoning；仅思考时 Reasoning
                        let reasoning = m
                            .get("reasoning_content")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty());
                        let content = match reasoning {
                            Some(reasoning) => {
                                if text.is_empty() && tool_calls.is_empty() {
                                    MessageContent::Reasoning {
                                        text: reasoning.to_string(),
                                        signature: None,
                                    }
                                } else {
                                    MessageContent::TextWithToolCalls {
                                        text: text.to_string(),
                                        tool_calls,
                                        reasoning: Some(reasoning.to_string()),
                                    }
                                }
                            }
                            None => {
                                if tool_calls.is_empty() {
                                    MessageContent::Text(text.to_string())
                                } else if text.is_empty() {
                                    MessageContent::ToolCalls(tool_calls)
                                } else {
                                    MessageContent::TextWithToolCalls {
                                        text: text.to_string(),
                                        tool_calls,
                                        reasoning: None,
                                    }
                                }
                            }
                        };
                        Message { role, content }
                    });

                let finish_reason = choice_obj
                    .get("finish_reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                // 已知词双写结构化槽；未知词只留原始透传槽
                let finish = finish_reason.as_deref().and_then(FinishReason::from_wire);

                Some(Choice {
                    index: index as u32,
                    message,
                    finish_reason,
                    finish,
                    // logprobs 原样进 IR（形状由 OpenAI wire 定义）
                    logprobs: choice_obj.get("logprobs").filter(|v| !v.is_null()).cloned(),
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

        // prompt_tokens_details / completion_tokens_details → IR 明细槽；
        // cached_tokens 同时镜像进 legacy cache_read_tokens 槽（跨协议载体；
        // OpenAI 无 cache_creation 概念，恒 0）
        let prompt_tokens_details = parse_prompt_tokens_details(usage.get("prompt_tokens_details"));
        let completion_tokens_details =
            parse_completion_tokens_details(usage.get("completion_tokens_details"));
        let cache_read_tokens = prompt_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
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
                prompt_tokens_details,
                completion_tokens_details,
            },
            stop_sequence: None,
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, GateError> {
        match chunk {
            StreamChunk::Content {
                index,
                delta,
                finish_reason,
                logprobs,
            } => {
                let mut delta_out = delta_to_openai(delta);
                // delta.logprobs 原样回写（缺省不写出，保持旧形状）
                if let Some(logprobs) = logprobs {
                    delta_out["logprobs"] = logprobs;
                }
                Ok(json!({
                    "id": format!("chunk-{}", index),
                    "object": "chat.completion.chunk",
                    "created": chrono::Utc::now().timestamp() as u64,
                    "model": "unknown",
                    "choices": [{
                        "index": index,
                        "delta": delta_out,
                        "finish_reason": finish_reason,
                    }],
                }))
            }
            // 流式工具调用片段 → delta.tool_calls（id/name 仅首片段携带）
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
                // cache_creation 无 OpenAI 槽；finish_reason 有值时写入本帧
                // choices 终止槽（OpenAI wire 合法形状，跨厂商终止原因可达客户端）；
                // None 保持空 choices 原样透传
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

/// OpenAI 出站流编码器：流级一致性归一化。无状态单块映射会导致各块
/// id/created 发散、首块缺 role，本编码器持有流作用域状态：
/// 首块固定流 id 与 created 复用到全流；首个 delta 帧（content /
/// tool_calls / reasoning_content 任一出现）补 "role": "assistant"；
/// finish_reason 原样透传。每流一个实例
#[derive(Debug)]
pub struct OpenAIStreamEncoder {
    stream_id: Option<String>,
    created: Option<u64>,
    role_sent: bool,
}

impl Default for OpenAIStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAIStreamEncoder {
    pub fn new() -> Self {
        Self {
            stream_id: None,
            created: None,
            role_sent: false,
        }
    }

    /// 归一化单个已映射 chunk：id/created 收敛到流固定值；首个 delta 帧
    /// （content / tool_calls / reasoning_content 任一出现）补 role。
    /// 非 chunk 帧（心跳等无 id 帧）原样返回。
    pub fn normalize(&mut self, mut value: Value) -> Value {
        if value.get("id").and_then(|v| v.as_str()).is_none() || value.get("created").is_none() {
            return value;
        }
        if self.stream_id.is_none() {
            self.stream_id = Some(format!(
                "chatcmpl-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            self.created = Some(chrono::Utc::now().timestamp() as u64);
        }
        value["id"] = Value::String(self.stream_id.clone().unwrap_or_default());
        value["created"] = Value::Number(self.created.unwrap_or_default().into());
        if !self.role_sent {
            if let Some(delta) = value
                .get_mut("choices")
                .and_then(|c| c.as_array_mut())
                .and_then(|a| a.first_mut())
                .and_then(|c| c.get_mut("delta"))
            {
                let has_content = delta.get("content").is_some()
                    || delta.get("tool_calls").is_some()
                    || delta.get("reasoning_content").is_some();
                if has_content && delta.get("role").is_none() {
                    delta["role"] = Value::String("assistant".to_string());
                    self.role_sent = true;
                }
            }
        }
        value
    }
}

/// usage 尾帧（`stream_options.include_usage`）解析：`choices` 为空且带
/// `usage` 对象 → [`StreamChunk::Usage`]；无 usage 或 usage 与 choices
/// 同帧 → `None`，交调用方继续走 delta 路径（行为逐位不变）。
fn parse_usage_tail_frame(json: &Value) -> Option<StreamChunk> {
    let usage = json.get("usage").filter(|u| u.is_object())?;
    let choices_empty = json
        .get("choices")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);
    if !choices_empty {
        return None;
    }
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
    // cached_tokens 同帧读取（cache_creation 无 OpenAI 概念）
    let cache_read_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .map(|u| u as u32)
        .unwrap_or(0);
    Some(StreamChunk::Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cache_read_tokens,
        cache_creation_tokens: 0,
        finish_reason: None,
    })
}

/// 解析 OpenAI 上游 SSE 帧的 `data` 负载 → IR StreamChunk。
///
/// 输入是分帧器剥出的 `data:` 负载（`chat.completion.chunk` JSON 或
/// `[DONE]`）：
/// - `[DONE]` → `None`（终止标记，不映射 IR）；
/// - 顶层 `usage` 帧（`stream_options.include_usage` 尾帧，choices 空）
///   → [`StreamChunk::Usage`]；
/// - `choices[0].delta.tool_calls`（规范单片段帧）→
///   [`StreamChunk::ToolCall`]（表示统一：id/name 仅首片段携带，
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

    // usage 尾帧：choices 为空且带 usage 对象（其余形态返回 None 继续解析）
    if let Some(chunk) = parse_usage_tail_frame(&json) {
        return Some(chunk);
    }

    let choice = json.get("choices")?.as_array()?.first()?.as_object()?;
    let index = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let delta = choice.get("delta");

    // 表示统一——tool_calls delta → ToolCall 片段流。
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

    // reasoning_content 增量 → Reasoning（DeepSeek/兼容端点形状）。
    // 非空才产出：思考流帧通常带 content:""，不可让它遮蔽 reasoning；
    // 同帧 text+reasoning 的非规范形状下 reasoning 优先（单 chunk 交付约束）
    let reasoning_delta = delta
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| MessageContent::Reasoning {
            text: s.to_string(),
            signature: None,
        });

    let delta_content = tool_call_delta.or(reasoning_delta).or(text_delta);

    // delta.logprobs 原样进 IR（形状由 OpenAI wire 定义）
    let logprobs = delta
        .and_then(|d| d.get("logprobs"))
        .filter(|v| !v.is_null())
        .cloned();

    // role-only 首帧（无内容无终止）：跳过，不产出空事件
    let has_payload = delta_content.is_some() || finish_reason.is_some() || logprobs.is_some();
    if !has_payload {
        return None;
    }

    Some(StreamChunk::Content {
        index,
        delta: delta_content.unwrap_or(MessageContent::Text(String::new())),
        finish_reason,
        logprobs,
    })
}

/// 解析流式 tool_calls 单片段 → ToolCall 片段字段（表示统一）。
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

/// 旧聚合表示路径的片段解析（仅服务非规范形状回退——单帧多片段 /
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

/// 将 UnifiedRequest 转换为 OpenAI 格式时的 system 消息合并：
/// system_prompt 与 messages 内 Role::System 并存时，system_prompt
/// 优先合并去重：相同文本只保留一份，不同文本并入首条 system 消息
/// （system_prompt 在前）；仅当 system_prompt 存在时重排，否则消息
/// 原位映射（行为兼容）。
fn merge_system_prompt_messages(
    mut source: Vec<Message>,
    system_prompt: Option<String>,
) -> Vec<Message> {
    let Some(system_prompt) = system_prompt.filter(|s| !s.is_empty()) else {
        return source;
    };
    let mut parts = vec![system_prompt.clone()];
    source.retain(|msg| {
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
    merged.extend(source);
    merged
}

/// sampling 只补 flat 未设置的槽（flat 优先，不覆写）；
/// top_k 非 OpenAI 标准参数，直出供兼容上游取用。
fn apply_sampling_overrides(
    body: &mut Value,
    flat_temperature: Option<f32>,
    flat_top_p: Option<f32>,
    flat_frequency_penalty: Option<f32>,
    flat_presence_penalty: Option<f32>,
    sampling: Option<&SamplingParams>,
) {
    let Some(sampling) = sampling else {
        return;
    };
    if flat_temperature.is_none() {
        if let Some(temp) = sampling.temperature {
            body["temperature"] = json!(temp);
        }
    }
    if flat_top_p.is_none() {
        if let Some(top_p) = sampling.top_p {
            body["top_p"] = json!(top_p);
        }
    }
    if flat_frequency_penalty.is_none() {
        if let Some(fp) = sampling.frequency_penalty {
            body["frequency_penalty"] = json!(fp);
        }
    }
    if flat_presence_penalty.is_none() {
        if let Some(pp) = sampling.presence_penalty {
            body["presence_penalty"] = json!(pp);
        }
    }
    if let Some(top_k) = sampling.top_k {
        body["top_k"] = json!(top_k);
    }
}

fn parse_openai_message(json: &Value) -> Result<Message, GateError> {
    let role = parse_message_role(json)?;

    let content = match json.get("content") {
        Some(Value::String(s)) => parse_string_content(json, &role, s),
        Some(Value::Array(arr)) => parse_array_content(arr),
        None | Some(Value::Null) => {
            // content 字段缺失或为 null，检查是否有 tool_calls
            // （提取逻辑提为共用 helper）
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

/// role 字段解析：四合法角色之外一律 Validation 错误（缺失同样报 role）。
fn parse_message_role(json: &Value) -> Result<Role, GateError> {
    let role = json
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or(GateError::Validation {
            field: "role".into(),
        })?;

    match role {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        _ => Err(GateError::Validation {
            field: "role".into(),
        }),
    }
}

/// String content：工具角色消息 → ToolResult；带顶层 tool_calls →
/// TextWithToolCalls（保留顶层 tool_calls）；
/// 否则纯文本。
fn parse_string_content(json: &Value, role: &Role, s: &str) -> MessageContent {
    // 检查是否有 tool_call_id（工具角色消息）
    if *role == Role::Tool {
        let tool_call_id = json
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return MessageContent::ToolResult(ToolResult {
            tool_call_id,
            function_name: String::new(),
            content: s.to_string(),
            is_error: false,
        });
    }
    if let Some(tool_calls) = parse_tool_calls_field(json) {
        if s.is_empty() {
            MessageContent::ToolCalls(tool_calls)
        } else {
            MessageContent::TextWithToolCalls {
                text: s.to_string(),
                tool_calls,
                reasoning: None,
            }
        }
    } else {
        MessageContent::Text(s.to_string())
    }
}

/// Array content：含工具调用块 → ToolCalls（解析为空回退 MultiModal）；
/// 纯多模态块 → MultiModal。
fn parse_array_content(arr: &[Value]) -> MessageContent {
    // 检查是否包含工具调用块
    let has_tool_calls = arr.iter().any(|block| {
        block.get("type").and_then(|t| t.as_str()) == Some("tool_call")
            || block.get("function").is_some()
    });
    if has_tool_calls {
        let tool_calls: Vec<ToolCall> = arr.iter().filter_map(parse_tool_call_block).collect();
        if !tool_calls.is_empty() {
            return MessageContent::ToolCalls(tool_calls);
        }
    }
    let blocks = arr.iter().filter_map(parse_content_block).collect();
    MessageContent::MultiModal(blocks)
}

/// 单个 content 数组块 → ToolCall：type=="tool_call" 走标准解析；
/// 含 function 对象为兼容格式（从 function 对象解析）；其余块跳过。
fn parse_tool_call_block(block: &Value) -> Option<ToolCall> {
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
            // URL 保真——https 直链 → Url 形态（不再硬编码 jpeg、
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
        "file" => {
            // OpenAI file 部件：file_data（data URL/直链）→ Document；
            // file_id 无跨协议等价 → 显性丢弃（debug 可检索）
            let file = json.get("file")?.as_object()?;
            let file_data = file.get("file_data").and_then(|v| v.as_str());
            let Some(file_data) = file_data else {
                drop_unsupported(Protocol::OpenAI, "file_id", "no cross-protocol equivalent");
                return None;
            };
            let (source, media_type) = match file_data.strip_prefix("data:") {
                Some(rest) => match rest.split_once(";base64,") {
                    Some((media_type, data)) => (
                        ImageSource::Base64 {
                            media_type: media_type.to_string(),
                            data: data.to_string(),
                        },
                        media_type.to_string(),
                    ),
                    None => (
                        ImageSource::Url {
                            url: file_data.to_string(),
                        },
                        "application/octet-stream".to_string(),
                    ),
                },
                None => (
                    ImageSource::Url {
                        url: file_data.to_string(),
                    },
                    "application/octet-stream".to_string(),
                ),
            };
            Some(ContentBlock::Document {
                source,
                media_type,
                title: file
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                cache_control: None,
            })
        }
        "input_audio" => {
            let audio = json.get("input_audio")?.as_object()?;
            Some(ContentBlock::Audio {
                data: audio.get("data")?.as_str()?.to_string(),
                format: audio.get("format")?.as_str()?.to_string(),
            })
        }
        _ => {
            // 未知块型无 IR 等价 → 显性丢弃（debug 可检索）
            drop_unsupported(
                Protocol::OpenAI,
                &format!("content[].{type_}"),
                "unknown content block type",
            );
            None
        }
    }
}

fn message_to_openai(msg: Message) -> Value {
    let role_str = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    let (content, tool_call_id, top_level_tool_calls, reasoning) = match msg.content {
        MessageContent::Text(s) => (Value::String(s), None, None, None),
        MessageContent::MultiModal(blocks) => {
            let arr: Vec<Value> = blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => json!({
                        "type": "text",
                        "text": text,
                    }),
                    ContentBlock::Image { source, detail } => {
                        // Base64 → data URI；Url → 直链原样（不再包
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
                    ContentBlock::Document {
                        source,
                        title,
                        cache_control,
                        ..
                    } => {
                        // cache_control 无 OpenAI 槽 → 显性丢弃
                        if cache_control.is_some() {
                            drop_unsupported(
                                Protocol::OpenAI,
                                "cache_control",
                                "no OpenAI slot on file parts",
                            );
                        }
                        // base64 → data URL 重建；Url → 直链原样；
                        // filename ← title
                        let file_data = match &source {
                            ImageSource::Base64 { media_type, data } => {
                                format!("data:{media_type};base64,{data}")
                            }
                            ImageSource::Url { url } => url.clone(),
                        };
                        let mut file = json!({"file_data": file_data});
                        if let Some(title) = title {
                            file["filename"] = json!(title);
                        }
                        json!({
                            "type": "file",
                            "file": file,
                        })
                    }
                    ContentBlock::Audio { data, format } => json!({
                        "type": "input_audio",
                        "input_audio": {
                            "data": data,
                            "format": format,
                        },
                    }),
                    ContentBlock::Type(t) => {
                        // 未知块型（IR Type 保真槽）无 OpenAI 等价 → 显性丢弃
                        drop_unsupported(
                            Protocol::OpenAI,
                            &format!("content[].{t}"),
                            "content block type has no OpenAI equivalent",
                        );
                        json!({})
                    }
                })
                .collect();
            (Value::Array(arr), None, None, None)
        }
        MessageContent::ToolCalls(tool_calls) => {
            // OpenAI 出站规范 —— 工具调用在顶层 tool_calls 字段，
            // content 为 null（不可塞进 content 数组，OpenAI 上游会拒收）
            (Value::Null, None, Some(tool_calls), None)
        }
        MessageContent::ToolResult(result) => (
            Value::String(result.content.clone()),
            Some(result.tool_call_id),
            None,
            None,
        ),
        MessageContent::TextWithToolCalls {
            text,
            tool_calls,
            reasoning,
        } => {
            // 文本与工具调用并列 —— content 为文本 + 顶层 tool_calls；
            // tool_calls 为空（text+reasoning 组合）时不写空数组。
            // reasoning 走非标 reasoning_content 字段（官方客户端
            // extra=allow 容忍）
            let calls = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            };
            (Value::String(text), None, calls, reasoning)
        }
        // 思考内容 → 非标 reasoning_content 字段，content 置空
        //（signature 无 OpenAI 槽，显性化丢弃）
        MessageContent::Reasoning { text, signature } => {
            if signature.is_some() {
                drop_unsupported(
                    Protocol::OpenAI,
                    "reasoning.signature",
                    "no OpenAI slot for thinking signature",
                );
            }
            (Value::String(String::new()), None, None, Some(text))
        }
    };

    let mut obj = json!({
        "role": role_str,
        "content": content,
    });

    // 思考内容非标字段回写（DeepSeek/兼容端点形状）
    if let Some(reasoning) = reasoning {
        obj["reasoning_content"] = json!(reasoning);
    }

    // 工具角色消息需要 tool_call_id
    if let Some(id) = tool_call_id {
        obj["tool_call_id"] = json!(id);
    }

    // assistant 工具调用输出为顶层 tool_calls
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
        // 流式思考增量 → delta.reasoning_content（非标字段）；
        // signature 无 OpenAI 槽 → 显性丢弃
        MessageContent::Reasoning { text, signature } => {
            if signature.is_some() {
                drop_unsupported(
                    Protocol::OpenAI,
                    "reasoning.signature",
                    "no OpenAI slot for thinking signature",
                );
            }
            json!({ "reasoning_content": text })
        }
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

/// OpenAI `usage.prompt_tokens_details` → IR 明细（对象缺省/空对象视为缺省）。
fn parse_prompt_tokens_details(value: Option<&Value>) -> Option<PromptTokensDetails> {
    let obj = value?.as_object()?;
    if obj.is_empty() {
        return None;
    }
    Some(PromptTokensDetails {
        audio_tokens: obj
            .get("audio_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_tokens: obj
            .get("cached_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

/// OpenAI `usage.completion_tokens_details` → IR 明细（对象缺省/空对象视为缺省）。
fn parse_completion_tokens_details(value: Option<&Value>) -> Option<CompletionTokensDetails> {
    let obj = value?.as_object()?;
    if obj.is_empty() {
        return None;
    }
    Some(CompletionTokensDetails {
        reasoning_tokens: obj
            .get("reasoning_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        audio_tokens: obj
            .get("audio_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        accepted_prediction_tokens: obj
            .get("accepted_prediction_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        rejected_prediction_tokens: obj
            .get("rejected_prediction_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

/// IR logit_bias → OpenAI wire。整数值归一为整数（`-100` 而非
/// `-100.0`，保真往返）；非整数值按浮点原样写出。
fn logit_bias_to_openai(bias: &HashMap<String, f32>) -> Value {
    let mut out = serde_json::Map::new();
    for (key, value) in bias {
        let number = if value.is_finite() && value.fract() == 0.0 && value.abs() <= i64::MAX as f32
        {
            Value::from(*value as i64)
        } else {
            Value::from(*value)
        };
        out.insert(key.clone(), number);
    }
    Value::Object(out)
}

/// 提取消息顶层 `tool_calls` 数组（String content 分支与
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

/// 解析 OpenAI 格式的工具定义（M1 / D7.14 闭环：parameters 原样
/// `serde_json::Value` 保真透传——不做结构化升降级，嵌套 schema / items /
/// anyOf / $ref / 缺 type 属性等全量无损）。
fn parse_openai_tool(json: &Value) -> Option<Tool> {
    let obj = json.as_object()?;
    let tool_type = obj
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("function");
    if tool_type != "function" {
        // 非 function（custom 等）原样保真进 raw；function 字段缺省
        return Some(Tool {
            tool_type: tool_type.to_string(),
            function: ToolFunction::default(),
            raw: Some(json.clone()),
            cache_control: None,
        });
    }
    let func = obj.get("function")?.as_object()?;
    let parameters = func.get("parameters").cloned().unwrap_or(Value::Null);

    Some(Tool {
        tool_type: "function".to_string(),
        function: ToolFunction {
            name: func.get("name")?.as_str()?.to_string(),
            description: func
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
            parameters,
            strict: func.get("strict").and_then(|v| v.as_bool()),
        },
        ..Default::default()
    })
}

/// 将工具转换为 OpenAI 格式：`raw`（custom 等非 function 保真形态）原样
/// 回写；非本协议原生非 function 类型（Anthropic server tool 等跨协议形态）
/// 显式 400，不静默降级。
fn tool_to_openai(tool: &Tool, index: usize) -> Result<Value, GateError> {
    if tool.tool_type != "function" {
        if tool.tool_type == "custom" {
            if let Some(raw) = tool.raw.as_ref() {
                return Ok(raw.clone());
            }
        }
        return Err(GateError::Validation {
            field: format!("tools[{index}].type"),
        });
    }
    let mut function = json!({
        "name": tool.function.name,
        "description": tool.function.description,
    });
    if tool.function.parameters.is_object() {
        function["parameters"] = tool.function.parameters.clone();
    }
    if let Some(strict) = tool.function.strict {
        function["strict"] = json!(strict);
    }
    Ok(json!({
        "type": tool.tool_type,
        "function": function,
    }))
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
                // Anthropic 形态兼容（跨协议客户端常带）：any → required，
                // tool(+name) → 具名；none 保留；其余回落 auto。
                // 显式映射 any/tool，避免静默改变工具调度语义。
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
                    Some("none") => ToolChoice::Strategy("none".to_string()),
                    _ => ToolChoice::Strategy("auto".to_string()),
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 组装最小合法入站请求体（额外字段合并进顶层）。
    fn inbound(extra: Value) -> UnifiedRequest {
        let mut body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
        });
        for (key, value) in extra.as_object().expect("extra 必须为对象") {
            body[key] = value.clone();
        }
        OpenAIConverter::new()
            .to_unified(body)
            .expect("to_unified 应成功")
    }

    #[test]
    fn inbound_reasoning_effort_full_vocabulary() {
        for (wire, level) in [
            ("none", EffortLevel::None),
            ("minimal", EffortLevel::Minimal),
            ("low", EffortLevel::Low),
            ("medium", EffortLevel::Medium),
            ("high", EffortLevel::High),
            ("xhigh", EffortLevel::XHigh),
            ("max", EffortLevel::Max),
        ] {
            let request = inbound(json!({"reasoning_effort": wire}));
            assert_eq!(request.effort, Some(level), "wire={wire}");
            assert!(
                !request.vendor_extensions.contains_key("reasoning_effort"),
                "合法词不落扩展表（wire={wire}）"
            );
        }
    }

    #[test]
    fn inbound_unknown_reasoning_effort_preserves_raw_value() {
        // 词表外字符串：不猜变体，原值留在扩展表
        let request = inbound(json!({"reasoning_effort": "ultra"}));
        assert_eq!(request.effort, None);
        assert_eq!(
            request.vendor_extensions.get("reasoning_effort"),
            Some(&json!("ultra")),
            "未知词原值留在扩展表防丢"
        );

        // 非字符串形态同样保留
        let request = inbound(json!({"reasoning_effort": 3}));
        assert_eq!(request.effort, None);
        assert_eq!(
            request.vendor_extensions.get("reasoning_effort"),
            Some(&json!(3))
        );
    }

    #[test]
    fn inbound_parses_metadata_service_tier_verbosity_and_max_completion_tokens() {
        let request = inbound(json!({
            "metadata": {"user_id": "u1", "trace": "t"},
            "service_tier": "flex",
            "verbosity": "high",
            "max_completion_tokens": 4096,
            "max_tokens": 512
        }));
        assert_eq!(
            request.metadata,
            Some(json!({"user_id": "u1", "trace": "t"}))
        );
        assert_eq!(request.service_tier.as_deref(), Some("flex"));
        assert_eq!(request.verbosity, Some(Verbosity::High));
        assert_eq!(request.max_completion_tokens, Some(4096));
        // 双槽独立并存
        assert_eq!(request.max_tokens, Some(512));
        // 协议键不落扩展表（known_keys 扩表生效）
        for key in [
            "metadata",
            "service_tier",
            "verbosity",
            "max_completion_tokens",
        ] {
            assert!(!request.vendor_extensions.contains_key(key), "key={key}");
        }
    }

    #[test]
    fn inbound_non_object_metadata_and_unknown_verbosity_are_dropped() {
        let request = inbound(json!({"metadata": "oops", "verbosity": "chatty"}));
        assert_eq!(request.metadata, None, "非对象 metadata 不入槽");
        assert_eq!(request.verbosity, None, "词表外 verbosity 不猜变体");
    }

    #[test]
    fn outbound_rewrites_logit_bias_and_new_slots() {
        let converter = OpenAIConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}],
                "logit_bias": {"50256": -100, "12345": 39.5},
                "max_completion_tokens": 4096,
                "metadata": {"trace": "t1"},
                "service_tier": "auto",
                "verbosity": "low",
                "reasoning_effort": "xhigh"
            }))
            .expect("to_unified");
        let body = converter.request_to_external(unified).expect("to_external");
        // 整数保真：-100 而非 -100.0；非整数原样
        assert_eq!(body["logit_bias"], json!({"50256": -100, "12345": 39.5}));
        assert_eq!(body["max_completion_tokens"], 4096);
        assert_eq!(body["metadata"], json!({"trace": "t1"}));
        assert_eq!(body["service_tier"], "auto");
        assert_eq!(body["verbosity"], "low");
        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn outbound_new_slots_absent_by_default() {
        let converter = OpenAIConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        let body = converter.request_to_external(unified).expect("to_external");
        for key in [
            "logit_bias",
            "max_completion_tokens",
            "metadata",
            "service_tier",
            "verbosity",
            "reasoning_effort",
        ] {
            assert!(body.get(key).is_none(), "缺省不写出: {key}");
        }
    }

    #[test]
    fn usage_details_roundtrip() {
        let converter = OpenAIConverter::new();
        let response = converter
            .parse_response(json!({
                "id": "chatcmpl-1",
                "created": 123,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 64,
                    "total_tokens": 164,
                    "prompt_tokens_details": {"cached_tokens": 40, "audio_tokens": 2},
                    "completion_tokens_details": {"reasoning_tokens": 64}
                }
            }))
            .expect("parse_response");
        // cached_tokens 镜像进 legacy 槽（跨协议载体）
        assert_eq!(response.usage.cache_read_tokens, 40);
        assert_eq!(
            response.usage.prompt_tokens_details,
            Some(PromptTokensDetails {
                audio_tokens: 2,
                cached_tokens: 40
            })
        );
        assert_eq!(
            response.usage.completion_tokens_details,
            Some(CompletionTokensDetails {
                reasoning_tokens: 64,
                ..Default::default()
            })
        );
        // 出站：details 原样保留
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(
            body["usage"]["prompt_tokens_details"],
            json!({"audio_tokens": 2, "cached_tokens": 40})
        );
        assert_eq!(
            body["usage"]["completion_tokens_details"]["reasoning_tokens"],
            64
        );
    }

    #[test]
    fn usage_legacy_cache_read_folds_into_cached_tokens_when_details_absent() {
        let converter = OpenAIConverter::new();
        let response = UnifiedResponse {
            id: "resp-1".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "gpt-4o".into(),
            choices: vec![],
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 1,
                total_tokens: 11,
                cache_read_tokens: 7,
                ..Default::default()
            },
            stop_sequence: None,
        };
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(
            body["usage"]["prompt_tokens_details"],
            json!({"cached_tokens": 7}),
            "details 缺省时 legacy cache_read 并入 cached_tokens"
        );
        assert!(body["usage"].get("completion_tokens_details").is_none());
    }

    #[test]
    fn choices_logprobs_roundtrip() {
        let converter = OpenAIConverter::new();
        let response = converter
            .parse_response(json!({
                "id": "chatcmpl-2",
                "created": 1,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                    "logprobs": {
                        "content": [{
                            "token": "hi",
                            "logprob": -0.1,
                            "bytes": null,
                            "top_logprobs": []
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .expect("parse_response");
        assert!(
            response.choices[0].logprobs.is_some(),
            "choices[].logprobs 应解析进 IR"
        );
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(body["choices"][0]["logprobs"]["content"][0]["token"], "hi");

        // 缺省时不写出（旧形状不变）
        let response = converter
            .parse_response(json!({
                "id": "chatcmpl-3",
                "created": 1,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .expect("parse_response");
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert!(body["choices"][0].get("logprobs").is_none());
    }

    #[test]
    fn reasoning_content_message_roundtrip() {
        let converter = OpenAIConverter::new();
        // 入站：仅 reasoning_content → Reasoning（文本空、无工具）
        let response = converter
            .parse_response(json!({
                "id": "chatcmpl-r1",
                "created": 1,
                "model": "deepseek-r1",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "reasoning_content": "think"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .expect("parse_response");
        let message = response.choices[0].message.as_ref().expect("message");
        match &message.content {
            MessageContent::Reasoning { text, signature } => {
                assert_eq!(text, "think");
                assert!(signature.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
        // 出站：reasoning_content 回写、content 空串
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(body["choices"][0]["message"]["reasoning_content"], "think");
        assert_eq!(body["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn reasoning_content_with_text_preserves_reasoning_without_tool_calls_key() {
        let converter = OpenAIConverter::new();
        // content + reasoning_content（无工具）→ TextWithToolCalls（保住 reasoning）
        let response = converter
            .parse_response(json!({
                "id": "chatcmpl-r2",
                "created": 1,
                "model": "deepseek-r1",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "answer",
                        "reasoning_content": "think"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .expect("parse_response");
        let message = response.choices[0].message.as_ref().expect("message");
        match &message.content {
            MessageContent::TextWithToolCalls {
                text,
                tool_calls,
                reasoning,
            } => {
                assert_eq!(text, "answer");
                assert!(tool_calls.is_empty());
                assert_eq!(reasoning.as_deref(), Some("think"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(body["choices"][0]["message"]["content"], "answer");
        assert_eq!(body["choices"][0]["message"]["reasoning_content"], "think");
        assert!(
            body["choices"][0]["message"].get("tool_calls").is_none(),
            "空 tool_calls 不写出空数组"
        );
    }

    #[test]
    fn stream_reasoning_content_roundtrip() {
        let converter = OpenAIConverter::new();
        // 入站：delta.reasoning_content → Content{delta: Reasoning}；
        // 同帧 content:"" 不遮蔽 reasoning
        let chunk = parse_sse_frame_to_chunk(
            r#"{"choices":[{"index":0,"delta":{"content":"","reasoning_content":"th"},"finish_reason":null}]}"#,
        )
        .expect("chunk");
        match chunk {
            StreamChunk::Content {
                delta: MessageContent::Reasoning { text, .. },
                ..
            } => assert_eq!(text, "th"),
            other => panic!("unexpected: {other:?}"),
        }
        // 出站：Reasoning → delta.reasoning_content
        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::Reasoning {
                    text: "th".into(),
                    signature: None,
                },
                finish_reason: None,
                logprobs: None,
            })
            .expect("to external");
        assert_eq!(out["choices"][0]["delta"]["reasoning_content"], "th");
    }

    #[test]
    fn file_and_audio_blocks_parse_into_document_and_audio() {
        let converter = OpenAIConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "read this"},
                        {"type": "file", "file": {
                            "file_data": "data:application/pdf;base64,QUJD",
                            "filename": "doc.pdf"
                        }},
                        {"type": "input_audio", "input_audio": {"data": "QUJD", "format": "wav"}}
                    ]
                }]
            }))
            .expect("to_unified");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 3);
                match &blocks[1] {
                    ContentBlock::Document {
                        source,
                        media_type,
                        title,
                        cache_control,
                    } => {
                        assert_eq!(media_type, "application/pdf");
                        assert_eq!(title.as_deref(), Some("doc.pdf"));
                        assert!(cache_control.is_none());
                        match source {
                            ImageSource::Base64 { media_type, data } => {
                                assert_eq!(media_type, "application/pdf");
                                assert_eq!(data, "QUJD");
                            }
                            other => panic!("unexpected: {other:?}"),
                        }
                    }
                    other => panic!("unexpected: {other:?}"),
                }
                match &blocks[2] {
                    ContentBlock::Audio { data, format } => {
                        assert_eq!(data, "QUJD");
                        assert_eq!(format, "wav");
                    }
                    other => panic!("unexpected: {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }

        // file_id 无跨协议等价 → 块丢弃（debug 显性化）
        let request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "x"},
                        {"type": "file", "file": {"file_id": "file-abc"}}
                    ]
                }]
            }))
            .expect("to_unified");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 1, "file_id 块被丢弃");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn document_and_audio_blocks_write_back_to_openai() {
        let msg = Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![
                ContentBlock::Document {
                    source: ImageSource::Base64 {
                        media_type: "application/pdf".into(),
                        data: "QUJD".into(),
                    },
                    media_type: "application/pdf".into(),
                    title: Some("doc.pdf".into()),
                    cache_control: None,
                },
                ContentBlock::Audio {
                    data: "QUJD".into(),
                    format: "wav".into(),
                },
            ]),
        };
        let out = message_to_openai(msg);
        assert_eq!(out["content"][0]["type"], "file");
        assert_eq!(
            out["content"][0]["file"]["file_data"],
            "data:application/pdf;base64,QUJD"
        );
        assert_eq!(out["content"][0]["file"]["filename"], "doc.pdf");
        assert_eq!(out["content"][1]["type"], "input_audio");
        assert_eq!(
            out["content"][1]["input_audio"],
            json!({"data": "QUJD", "format": "wav"})
        );

        // url 形态直链回写
        let msg = Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![ContentBlock::Document {
                source: ImageSource::Url {
                    url: "https://example.com/doc.pdf".into(),
                },
                media_type: "application/pdf".into(),
                title: None,
                cache_control: None,
            }]),
        };
        let out = message_to_openai(msg);
        assert_eq!(
            out["content"][0]["file"]["file_data"],
            "https://example.com/doc.pdf"
        );
        assert!(out["content"][0]["file"].get("filename").is_none());
    }

    #[test]
    fn custom_tool_raw_fidelity_roundtrip() {
        let converter = OpenAIConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{
                    "type": "custom",
                    "custom": {
                        "name": "fmt",
                        "format": {"type": "grammar"}
                    }
                }]
            }))
            .expect("to_unified");
        assert_eq!(request.tools[0].tool_type, "custom");
        assert!(request.tools[0].raw.is_some(), "custom 原对象保真进 raw");

        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["tools"][0]["type"], "custom");
        assert_eq!(body["tools"][0]["custom"]["name"], "fmt");
        assert_eq!(body["tools"][0]["custom"]["format"]["type"], "grammar");
    }

    #[test]
    fn cross_protocol_non_function_tool_returns_validation_400() {
        let converter = OpenAIConverter::new();
        // Anthropic server tool（raw 存在但非 OpenAI 原生）→ 显式 400
        let mut request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        request.tools = vec![Tool {
            tool_type: "web_search_20250305".into(),
            raw: Some(json!({"type": "web_search_20250305", "name": "web_search"})),
            ..Default::default()
        }];
        let err = converter
            .request_to_external(request)
            .expect_err("跨协议非 function 工具必须 400");
        match err {
            crate::error::GateError::Validation { field } => {
                assert_eq!(field, "tools[0].type");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // custom 但 raw 缺失（IR 手工构造的坏形状）→ 同样 400
        let mut request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        request.tools = vec![Tool {
            tool_type: "custom".into(),
            ..Default::default()
        }];
        let err = converter
            .request_to_external(request)
            .expect_err("custom 无 raw 必须 400");
        match err {
            crate::error::GateError::Validation { field } => {
                assert_eq!(field, "tools[0].type");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn stream_logprobs_parse_and_write_back() {
        let converter = OpenAIConverter::new();
        // 入站 delta.logprobs → Content.logprobs
        let chunk = parse_sse_frame_to_chunk(
            r#"{"choices":[{"index":0,"delta":{"content":"hi","logprobs":{"content":[{"token":"hi","logprob":-0.2}]}},"finish_reason":null}]}"#,
        )
        .expect("chunk");
        match &chunk {
            StreamChunk::Content { logprobs, .. } => {
                assert_eq!(logprobs.as_ref().unwrap()["content"][0]["token"], "hi");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // 出站 delta.logprobs 回写
        let out = converter
            .stream_chunk_to_external(chunk)
            .expect("to external");
        assert_eq!(
            out["choices"][0]["delta"]["logprobs"]["content"][0]["logprob"],
            -0.2
        );

        // 缺省不写出（旧形状不变）
        let chunk = parse_sse_frame_to_chunk(
            r#"{"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}"#,
        )
        .expect("chunk");
        let out = converter
            .stream_chunk_to_external(chunk)
            .expect("to external");
        assert!(out["choices"][0]["delta"].get("logprobs").is_none());
    }

    #[test]
    fn stream_encoder_adds_role_on_reasoning_first_frame() {
        let converter = OpenAIConverter::new();
        let mut encoder = OpenAIStreamEncoder::new();
        // Reasoning 首帧补 role（否则思考流在首个文本帧前无 role）
        let encoded = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::Reasoning {
                    text: "th".into(),
                    signature: None,
                },
                finish_reason: None,
                logprobs: None,
            })
            .expect("to external");
        let normalized = encoder.normalize(encoded);
        assert_eq!(normalized["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(normalized["choices"][0]["delta"]["reasoning_content"], "th");

        // 后续 content 帧不重复补 role
        let encoded = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::Text("hi".into()),
                finish_reason: None,
                logprobs: None,
            })
            .expect("to external");
        let normalized = encoder.normalize(encoded);
        assert!(normalized["choices"][0]["delta"].get("role").is_none());
    }

    #[test]
    fn drop_paths_are_reachable_without_panic() {
        let converter = OpenAIConverter::new();
        // 未知内容块型（入站）→ 丢弃 + debug，不 panic
        let request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "x"},
                        {"type": "mystery_block", "data": 1}
                    ]
                }]
            }))
            .expect("to_unified");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => assert_eq!(blocks.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }

        // reasoning 带 signature 出站（无 OpenAI 槽）→ 丢弃 + debug
        let out = message_to_openai(Message {
            role: Role::Assistant,
            content: MessageContent::Reasoning {
                text: "t".into(),
                signature: Some("sig".into()),
            },
        });
        assert_eq!(out["reasoning_content"], "t");
        assert_eq!(out["content"], "");

        // thinking_config（含 display）出站 → 丢弃 + debug
        let mut request = converter
            .to_unified(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        request.thinking_config = Some(ThinkingConfig::Adaptive { display: None });
        request.thinking_include_thoughts = Some(true);
        let body = converter.request_to_external(request).expect("to external");
        assert!(body.get("thinking_config").is_none());

        // IR Type 保真块出站 → 丢弃 + debug（空对象占位）
        let out = message_to_openai(Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![
                ContentBlock::Text { text: "a".into() },
                ContentBlock::Type("redacted_thinking".into()),
            ]),
        });
        assert_eq!(out["content"].as_array().unwrap().len(), 2);
        assert_eq!(out["content"][1], json!({}));
    }
}
