// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::model_catalog::CacheBreakpointPolicy;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// Anthropic 协议已显式映射进 IR 的顶层键（其余字段吸收进 vendor_extensions）。
///
/// 该常量同时用作出站 merge 的 denylist（known_keys 反用）：协议键永不由
/// 扩展表补入/覆写。
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
    "output_config",
    "metadata",
    "service_tier",
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

    /// 出站编码 + 自动缓存断点注入 — anthropic 型 vendor 转换路径专用
    /// 入口（策略解析在 VendorManager 侧按 `[[model_catalog]]` 条目/内置
    /// 默认完成；透传路径字节保真不注入）。
    pub fn request_to_external_with_cache_policy(
        &self,
        request: UnifiedRequest,
        policy: &CacheBreakpointPolicy,
    ) -> Result<Value, GateError> {
        let mut body = self.request_to_external(request)?;
        let injected = inject_cache_control(&mut body, policy);
        tracing::debug!(
            model = %body.get("model").and_then(|v| v.as_str()).unwrap_or("?"),
            injected,
            "{}",
            t("protocol-anthropic-cache-breakpoints-injected")
        );
        Ok(body)
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

        // 顶层 system 参数 → 首条 System 消息；同一文本双写 system_prompt
        // （规范槽，单一事实源）；Role::System 消息保留为兼容形态。
        // Anthropic 的 system 支持 string 或 content blocks 数组两种形状。
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
        let tools: Vec<Tool> = body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(parse_anthropic_tool).collect())
            .unwrap_or_default();
        // 工具 schema 体积/深度上限：超限请求级 400
        validate_tool_schemas(&tools)?;

        // Anthropic 的 tool_choice 映射到统一格式（形状详解见 parse_anthropic_tool_choice）
        let tool_choice = body.get("tool_choice").map(parse_anthropic_tool_choice);
        // disable_parallel_tool_use=true → IR parallel_tool_calls=Some(false)
        //（false/缺省 → None；出站反向回写，tool_choice 缺省时补 auto）
        let parallel_tool_calls = body
            .get("tool_choice")
            .and_then(|v| v.as_object())
            .and_then(|tc| tc.get("disable_parallel_tool_use"))
            .and_then(|v| v.as_bool())
            .filter(|enabled| *enabled)
            .map(|_| false);

        // temperature/top_p/top_k → sampling（core 对齐槽）；
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

        // thinking → thinking_config（enabled 无预算 → Adaptive）
        let thinking_config = parse_anthropic_thinking(&body);

        // metadata → IR metadata（对象原样）+ metadata.user_id → IR user；
        // service_tier 仅 auto/standard_only 入 IR，词表外值显性丢弃
        let metadata = body
            .get("metadata")
            .and_then(|v| v.as_object())
            .map(|m| Value::Object(m.clone()));
        let user = metadata
            .as_ref()
            .and_then(|m| m.get("user_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let service_tier = match body.get("service_tier").and_then(|v| v.as_str()) {
            Some("auto") => Some("auto".to_string()),
            Some("standard_only") => Some("standard_only".to_string()),
            Some(other) => {
                drop_unsupported(
                    Protocol::Anthropic,
                    "service_tier",
                    &format!("unsupported service_tier value '{other}'"),
                );
                None
            }
            None => None,
        };

        // output_config：effort 与 format 双向映射（Anthropic 词表
        // low/medium/high/xhigh/max；none/minimal 与其它词表外值显性丢弃）；
        // 未知子键回填 vendor_extensions["output_config"] 防丢
        let output_config = body.get("output_config").and_then(|v| v.as_object());
        let effort = match output_config
            .and_then(|oc| oc.get("effort"))
            .and_then(|v| v.as_str())
        {
            Some("low") => Some(EffortLevel::Low),
            Some("medium") => Some(EffortLevel::Medium),
            Some("high") => Some(EffortLevel::High),
            Some("xhigh") => Some(EffortLevel::XHigh),
            Some("max") => Some(EffortLevel::Max),
            Some(other) => {
                drop_unsupported(
                    Protocol::Anthropic,
                    "output_config.effort",
                    &format!("unsupported effort value '{other}'"),
                );
                None
            }
            None => None,
        };
        let response_format = output_config
            .and_then(|oc| oc.get("format"))
            .and_then(parse_anthropic_output_format);
        let mut vendor_extensions = absorb_vendor_extensions(&body, ANTHROPIC_KNOWN_KEYS);
        if let Some(oc) = output_config {
            let unknown = oc
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "effort" | "format"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<String, Value>>();
            if !unknown.is_empty() {
                vendor_extensions.insert("output_config".into(), Value::Object(unknown));
            }
        }

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
            sampling,
            thinking_config,
            thinking_include_thoughts: None,
            effort,
            turn_index: None,
            tools,
            tool_choice,
            parallel_tool_calls,
            response_format,
            metadata,
            service_tier,
            // verbosity/max_completion_tokens 无 Anthropic 原生槽：保持 None
            //（OpenAI 侧类型化字段；跨协议经 vendor_extensions 显式携带）
            verbosity: None,
            max_completion_tokens: None,
            user,
            vendor_extensions,
        })
    }

    fn request_to_external(&self, request: UnifiedRequest) -> Result<Value, GateError> {
        // Role::System 消息提升为 Anthropic 顶层 system 参数（messages
        // 数组不接受 system 角色）；system_prompt 为规范槽，出站优先
        // 合并，与 Role::System 消息文本去重（相同文本只保留一份）。
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

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .enumerate()
                .map(|(index, tool)| tool_to_anthropic(tool, index))
                .collect::<Result<_, _>>()?;
            body["tools"] = json!(tools);
        }

        // Anthropic tool_choice 出站写原生形状（required→any；Specific→tool；
        // none 原生 none；unknown 策略保守回退 auto）。parallel_tool_calls
        // 映射为 disable_parallel_tool_use（Some(false)→true、Some(true)→
        // 显式 false、None 不写；需写开关且 tool_choice 缺省时补 auto）
        let disable_parallel = match request.parallel_tool_calls {
            Some(false) => Some(true),
            Some(true) => Some(false),
            None => None,
        };
        let mut choice = match request.tool_choice.as_ref() {
            Some(ToolChoice::Strategy(s)) => match s.as_str() {
                "required" | "any" => Some(json!({"type": "any"})),
                "none" => Some(json!({"type": "none"})),
                _ => Some(json!({"type": "auto"})),
            },
            Some(ToolChoice::Specific { function, .. }) => {
                Some(json!({"type": "tool", "name": function.name}))
            }
            None => disable_parallel.map(|_| json!({"type": "auto"})),
        };
        if let Some(choice) = choice.as_mut() {
            if let Some(flag) = disable_parallel {
                choice["disable_parallel_tool_use"] = json!(flag);
            }
        }
        if let Some(choice) = choice {
            body["tool_choice"] = choice;
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
        // sampling 补 Anthropic 缺省槽：top_k 直出；temperature/top_p 仅
        // 在 flat 槽未设置时补（flat 优先，不覆写）
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
        // thinking_config → thinking 块（D3 四变体）：Enabled{budget>0} 写原生
        // enabled（budget_tokens 必需）；Enabled{budget:0}/Adaptive →
        // adaptive；BetweenTools → between_tools；Disabled 省略字段
        //（Anthropic 缺省即关闭；显式 disabled 会把默认语义反转，故不写）；
        // display 一并回写
        let display_wire = |display: &Option<ThinkingDisplay>| {
            display.as_ref().map(|d| match d {
                ThinkingDisplay::Summarized => "summarized",
                ThinkingDisplay::Omitted => "omitted",
            })
        };
        match request.thinking_config.as_ref() {
            Some(ThinkingConfig::Enabled {
                budget_tokens,
                display,
            }) if *budget_tokens > 0 => {
                let mut thinking = json!({
                    "type": "enabled",
                    "budget_tokens": budget_tokens,
                });
                if let Some(wire) = display_wire(display) {
                    thinking["display"] = json!(wire);
                }
                body["thinking"] = thinking;
            }
            Some(ThinkingConfig::Enabled { display, .. })
            | Some(ThinkingConfig::Adaptive { display }) => {
                let mut thinking = json!({"type": "adaptive"});
                if let Some(wire) = display_wire(display) {
                    thinking["display"] = json!(wire);
                }
                body["thinking"] = thinking;
            }
            Some(ThinkingConfig::BetweenTools) => {
                body["thinking"] = json!({"type": "between_tools"});
            }
            Some(ThinkingConfig::Disabled) | None => {}
        }
        // output_config：effort（None 不写、Minimal 降级 low）与 format
        // （json_schema → 原生 output_config.format；json_object/未知 →
        // 显性丢弃）；vendor_extensions["output_config"] 未知子键回填合并
        let mut output_config_map = serde_json::Map::new();
        if let Some(Value::Object(unknown)) = request.vendor_extensions.get("output_config") {
            for (key, value) in unknown {
                output_config_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(effort) = request.effort {
            let wire = match effort {
                EffortLevel::None => None,
                EffortLevel::Minimal => Some("low"),
                other => Some(other.to_wire()),
            };
            if let Some(wire) = wire {
                output_config_map.insert("effort".into(), json!(wire));
            }
        }
        if let Some(format) = request.response_format.as_ref() {
            match format.get("type").and_then(|v| v.as_str()) {
                Some("json_schema") => {
                    if let Some(schema) = format.get("json_schema").and_then(|js| js.get("schema"))
                    {
                        output_config_map.insert(
                            "format".into(),
                            json!({"type": "json_schema", "schema": schema}),
                        );
                    }
                }
                other => {
                    drop_unsupported(
                        Protocol::Anthropic,
                        "response_format",
                        &format!(
                            "no Anthropic equivalent (type '{}')",
                            other.unwrap_or("unknown")
                        ),
                    );
                }
            }
        }
        if !output_config_map.is_empty() {
            body["output_config"] = Value::Object(output_config_map);
        }

        // metadata：IR 原样回写；IR user 且 metadata 无 user_id 时补
        // user_id；空对象不写
        let mut metadata_out = match request.metadata.as_ref() {
            Some(Value::Object(map)) => map.clone(),
            _ => serde_json::Map::new(),
        };
        if !metadata_out.contains_key("user_id") {
            if let Some(user) = request.user.as_ref() {
                metadata_out.insert("user_id".into(), json!(user));
            }
        }
        if !metadata_out.is_empty() {
            body["metadata"] = Value::Object(metadata_out);
        }
        // service_tier：仅 auto/standard_only 原名写；其它显性丢弃
        //（入站已在解析侧拦截词表外值，此处覆盖跨协议直供 IR 的场景）
        if let Some(service_tier) = request.service_tier.as_ref() {
            match service_tier.as_str() {
                "auto" | "standard_only" => body["service_tier"] = json!(service_tier),
                other => {
                    drop_unsupported(
                        Protocol::Anthropic,
                        "service_tier",
                        &format!("unsupported service_tier value '{other}'"),
                    );
                }
            }
        }
        // vendor_extensions 顶层透传，denylist = 本协议 known_keys 反用——
        // 协议键永不由扩展表补入/覆写，非协议键仍透传
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

        // finish 结构化槽优先
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
        // 命中停止序列时 stop_reason 必须为 "stop_sequence"（Anthropic
        // 语义：该值仅当停止序列命中时出现；IR finish=Stop 会平铺成
        // end_turn，需由 stop_sequence 实际值纠偏）
        let stop_reason = if response.stop_sequence.is_some() {
            "stop_sequence".to_string()
        } else {
            stop_reason
        };

        // IR cache 字段 → cache_read/creation_input_tokens（非零才携带；
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
            // 命中的停止序列实际值回写（R-011；未命中为 null）
            "stop_sequence": response.stop_sequence,
            "usage": usage,
        }))
    }

    fn parse_response(&self, response: Value) -> Result<UnifiedResponse, GateError> {
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
                // 全块聚合（R-007）：规则见
                // [`aggregate_anthropic_assistant_blocks`]
                let message_content = aggregate_anthropic_assistant_blocks(arr);

                Some(Message {
                    role: Role::Assistant,
                    content: message_content,
                })
            }
            Value::String(s) => Some(Message {
                role: Role::Assistant,
                content: MessageContent::Text(s.clone()),
            }),
            _ => None,
        };

        // stop_reason → finish_reason 静态映射（IR 统一存 OpenAI 词汇，
        // 未知词原样保留）；已知词双写结构化槽
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

        // cache_read/creation_input_tokens → IR cache 字段
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
                ..Default::default()
            },
            // 命中的停止序列实际值（Anthropic `stop_sequence`；未命中为 null）
            stop_sequence: response
                .get("stop_sequence")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }

    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<Value, GateError> {
        match chunk {
            StreamChunk::Content {
                index: _,
                delta,
                finish_reason,
                ..
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
                    // 思考块流式编码由 T028 的编码器路径承担；
                    // 此无状态单块路径对 Reasoning 不产出内容块
                    MessageContent::Reasoning { .. } => Vec::new(),
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
                    // IR finish 词表 → Anthropic stop_reason 词表
                    //（stop→end_turn 等；未知词原样透传）
                    "stop_reason": finish_reason
                        .as_deref()
                        .map(finish_reason_to_anthropic_stop_reason),
                }))
            }
            // 无状态单块路径——首片段（name 存在性，本方法无跨帧
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
                // finish_reason 随 Usage 到达时透传进 delta.stop_reason
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

/// 解析 Anthropic 上游 SSE 帧的 `data` 负载 → IR StreamChunk。
/// - `content_block_delta`（text_delta）→ Content；`thinking_delta`/
///   `signature_delta` → Reasoning（文本/思考各成块的 IR 语义）；
///   tool_use/input_json_delta
///   → ToolCall（首片段带 id/name，续片段为 arguments 增量，并行工具按 index 交错）；
/// - `message_delta`（usage 尾帧）→ Usage（stop_reason 同帧映射）；
/// - `ping` → Heartbeat；
/// - 生命周期事件（message_start/block_start(text)/block_stop/message_stop）与
///   `error` → None（事件序由出站 [`AnthropicStreamEncoder`] 状态机重建）
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
            let block_type = block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if block_type != "tool_use" {
                // text/thinking 非工具块为生命周期事件，由出站状态机重建；
                // 其余未知块型（redacted_thinking 等）显性丢弃（R-012）
                if !matches!(block_type, "text" | "thinking") {
                    drop_unsupported(
                        Protocol::Anthropic,
                        &format!("stream.content_block.{block_type}"),
                        "block type has no IR equivalent",
                    );
                }
                return None;
            }
            // （收口）：片段类别由帧类型显式判定——
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
            // 工具参数增量 → ToolCall 续片段（id/name 由首片段携带；
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
            // 思考增量 → Reasoning（R-008）：thinking_delta 带文本、
            // signature_delta 带签名，分别成 IR 块以便出站重建事件序
            match delta.get("type").and_then(|v| v.as_str()) {
                Some("thinking_delta") => {
                    let thinking = delta
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    return Some(StreamChunk::Content {
                        index: json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        delta: MessageContent::Reasoning {
                            text: thinking.to_string(),
                            signature: None,
                        },
                        finish_reason: None,
                        logprobs: None,
                    });
                }
                Some("signature_delta") => {
                    let signature = delta
                        .get("signature")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    return Some(StreamChunk::Content {
                        index: json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        delta: MessageContent::Reasoning {
                            text: String::new(),
                            signature: Some(signature.to_string()),
                        },
                        finish_reason: None,
                        logprobs: None,
                    });
                }
                _ => {}
            }
            // 未知增量型（citations_delta 等）显性丢弃，不产出空文本
            //（R-012）；text_delta 之外的已识别型均在上方分支提前返回
            if delta.get("type").and_then(|v| v.as_str()) != Some("text_delta") {
                let delta_type = delta
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                drop_unsupported(
                    Protocol::Anthropic,
                    &format!("stream.delta.{delta_type}"),
                    "delta type has no IR equivalent",
                );
                return None;
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
                logprobs: None,
            })
        }
        "content_block_stop" => {
            // 块收口为生命周期事件，不映射 IR——消费方按显式聚合
            // 状态（index → {id, name, arguments 缓冲}）持有调用，终止以
            // stop_reason（message_delta）或流尾判定；事件序由出站状态机重建
            None
        }
        "message_start" => {
            // 生命周期事件中唯一携带 input 用量的帧：映射为 Usage（output 取
            // 同帧值，通常 1 或 0），供转换链的配额结算/分账与出站 usage
            // 形状使用；事件序仍由出站状态机重建。缺 usage 的帧（测试/非
            // 标准上游）保持不映射 IR。
            let usage = json.get("message")?.get("usage")?;
            let input = usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let output = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let cache_read_tokens = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let cache_creation_tokens = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            Some(StreamChunk::Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason: None,
            })
        }
        "message_delta" => {
            // delta.stop_reason 同帧映射（end_turn/stop_sequence→Stop、
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

/// Anthropic 出站流式状态机：IR StreamChunk 流缺生命周期事件，本状态机
/// 在 IR 事件之上补齐：
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
/// 工具流：Anthropic 顺序块语义下并行多 id/index 交错不能直接映射为
/// 事件，ToolCall 片段按 IR 槽位缓冲到刷写点后串行化为合法块序列；
/// 刷写点先于文本块。块序号由编码器内部计数器分配
#[derive(Debug)]
pub struct AnthropicStreamEncoder {
    message_started: bool,
    block_open: bool,
    block_index: u32,
    stop_reason: Option<String>,
    /// 当前打开的块是否为 thinking 块（思考/文本不混流：切换块类型时
    /// 必须先闭合当前块再开新块）
    thinking_block_open: bool,
    /// 待刷写工具块缓冲：IR 工具槽位 index → 缓冲（交错状态跟踪）
    pending_tools: BTreeMap<u32, ToolBuffer>,
}

/// 待刷写工具块缓冲（并行多 id/index 交错的显式状态跟踪）
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
            thinking_block_open: false,
            pending_tools: BTreeMap::new(),
        }
    }

    /// 刷写待发工具块缓冲：按 IR 工具槽位序产出合法事件序列——
    /// 必要时先闭合当前 open 的文本块，随后
    /// content_block_start(tool_use) + input_json_delta 分片* +
    /// content_block_stop；块序号取编码器内部计数器并逐块递增。
    fn flush_pending_tools(&mut self, events: &mut Vec<String>) {
        // F1：mem::take 拿所有权直接迭代（BTreeMap 键序 = 槽位序），
        // 免临时 key Vec 与逐项 remove
        for (slot, buffer) in std::mem::take(&mut self.pending_tools) {
            // id 与 name 均空的退化槽为畸形输入，跳过并显性化
            //（与 openai 入站解析对畸形片段的丢弃行为对称）
            if buffer.id.is_empty() && buffer.name.as_deref().unwrap_or("").is_empty() {
                tracing::warn!(slot, "{}", t("protocol-tool-buffer-malformed"));
                continue;
            }
            if self.block_open {
                events.push(self.content_block_stop_event());
                self.block_open = false;
                self.thinking_block_open = false;
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
                // 思考增量单独成块（R-008）：thinking 块开闭状态跟踪，
                // thinking_delta / signature_delta 事件序重建；
                // 文本路径仍只走 extract_delta_text（不产出思考文本）
                if let MessageContent::Reasoning { text, signature } = &delta {
                    let mut events = Vec::new();
                    if !self.message_started {
                        events.push(self.message_start_event());
                    }
                    // 思考块前同样先刷写工具缓冲（块序归一化）
                    self.flush_pending_tools(&mut events);
                    // 已开的文本块需先闭合才能开思考块（块不混流）
                    if self.block_open && !self.thinking_block_open {
                        events.push(self.content_block_stop_event());
                        self.block_open = false;
                        self.block_index += 1;
                    }
                    if !text.is_empty() {
                        if !self.block_open {
                            events.push(self.thinking_block_start_event());
                        }
                        events.push(
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {
                                    "type": "thinking_delta",
                                    "thinking": text,
                                },
                            })
                            .to_string(),
                        );
                    }
                    if let Some(signature) = signature {
                        if !self.block_open {
                            events.push(self.thinking_block_start_event());
                        }
                        events.push(
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {
                                    "type": "signature_delta",
                                    "signature": signature,
                                },
                            })
                            .to_string(),
                        );
                    }
                    if let Some(reason) = finish_reason {
                        self.stop_reason = Some(reason);
                    }
                    if self.stop_reason.is_some() && self.block_open {
                        events.push(self.content_block_stop_event());
                        self.block_open = false;
                        self.thinking_block_open = false;
                        self.block_index += 1;
                    }
                    return events;
                }

                let text = extract_delta_text(&delta);
                if let Some(reason) = finish_reason {
                    self.stop_reason = Some(reason);
                }

                let mut events = Vec::new();
                if !self.message_started {
                    events.push(self.message_start_event());
                }
                // 文本块打开前先刷写工具缓冲（工具块在前为归一化
                // 顺序；文本块如在其后重新开块，块序仍合法）
                self.flush_pending_tools(&mut events);
                if !text.is_empty() {
                    // 思考块未闭合时文本不能续写其中：先闭再开
                    if self.block_open && self.thinking_block_open {
                        events.push(self.content_block_stop_event());
                        self.block_open = false;
                        self.thinking_block_open = false;
                        self.block_index += 1;
                    }
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
                // （message_delta 由 Usage 帧或 finish 兜底发出）。
                // 闭合即递增序号——后开块序号唯一（Anthropic 块序号语义）。
                if self.stop_reason.is_some() && self.block_open {
                    events.push(self.content_block_stop_event());
                    self.block_open = false;
                    self.thinking_block_open = false;
                    self.block_index += 1;
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
                // 终止原因与 usage 同帧到达时（Anthropic 上游回流），
                // 先更新 stop_reason 再发 message_delta
                if let Some(reason) = finish_reason {
                    self.stop_reason = Some(reason);
                }
                if !self.message_started {
                    events.push(self.message_start_event_with_input(prompt_tokens));
                }
                // message_delta 前必须刷写完所有工具块（顺序块语义）
                self.flush_pending_tools(&mut events);
                if self.block_open {
                    events.push(self.content_block_stop_event());
                    self.block_open = false;
                    self.thinking_block_open = false;
                    self.block_index += 1;
                }
                events.push(
                    json!({
                        "type": "message_delta",
                        "delta": {
                            // IR finish 词表 → Anthropic stop_reason 词表
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
            // 工具片段先缓冲（顺序块语义），到刷写点按槽位序串行化
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
                // name 覆盖仅在非空时——Some("") 不得降级既有合法 name
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
    /// 只发 message_stop 不发 message_delta（stop_reason 仅随 Usage 帧上线）；
    /// 空流产出完整 message_start + message_stop
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.message_started {
            events.push(self.message_start_event());
        }
        // 收尾前刷写残留工具缓冲
        self.flush_pending_tools(&mut events);
        if self.block_open {
            events.push(self.content_block_stop_event());
            self.block_open = false;
            self.thinking_block_open = false;
            self.block_index += 1;
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
        self.thinking_block_open = false;
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

    /// thinking 块开启事件（Reasoning 增量专用；块类型切换须先
    /// `content_block_stop` 闭合前块——由调用方状态机保证）
    fn thinking_block_start_event(&mut self) -> String {
        self.block_open = true;
        self.thinking_block_open = true;
        json!({
            "type": "content_block_start",
            "index": self.block_index,
            "content_block": {
                "type": "thinking",
                "thinking": "",
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
            // tool_use / tool_result 块聚合（经
            // parse_anthropic_block 聚合，保留多轮工具对话历史）。
            // 不改 ContentBlock 枚举以控制爆炸半径 —— 聚合发生在本层。
            let has_tool_use = arr.iter().any(|b| {
                matches!(
                    b.get("type").and_then(|t| t.as_str()),
                    Some("tool_use") | Some("server_tool_use")
                )
            });
            let has_thinking = arr
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"));
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
            if role == Role::Assistant && (has_tool_use || has_thinking) {
                // assistant 的 tool_use/thinking 块聚合 → 与响应解析
                // 同一 helper（保 reasoning/signature 语义一致）
                return Ok(Message {
                    role,
                    content: aggregate_anthropic_assistant_blocks(arr),
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

/// Anthropic assistant content 块数组 → IR MessageContent（响应解析与多轮
/// 消息历史共用）。
///
/// 规则（D3/R-007）：text 按序全量拼接（`\n` 连接）；thinking（含
/// signature）→ reasoning；tool_use/server_tool_use → ToolCall；
/// redacted_thinking 与未知块显性丢弃（debug 可检索）。组合规则：
/// text+calls → TextWithToolCalls{text,calls,reasoning}；仅 calls →
/// ToolCalls 保旧形状；仅 reasoning → Reasoning；text+reasoning（无
/// calls）→ TextWithToolCalls 空 calls 保住 reasoning（与出站 thinking
/// 块先行对称）。
fn aggregate_anthropic_assistant_blocks(arr: &[Value]) -> MessageContent {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut reasoning_signature: Option<String> = None;

    for item in arr {
        let item_type = item.get("type").and_then(|t| t.as_str());
        match item_type {
            Some("text") => {
                if item.get("citations").is_some() {
                    // 引用块无 IR 槽：显性丢弃（R-012）
                    drop_unsupported(
                        Protocol::Anthropic,
                        "content[].citations",
                        "citations have no IR slot",
                    );
                }
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            Some("thinking") => {
                if let Some(text) = item.get("thinking").and_then(|v| v.as_str()) {
                    reasoning_parts.push(text.to_string());
                }
                if let Some(sig) = item.get("signature").and_then(|v| v.as_str()) {
                    reasoning_signature = Some(sig.to_string());
                }
            }
            // server tool 与普通 tool_use 同槽聚合
            Some("tool_use") | Some("server_tool_use") => {
                if let Some(call) = parse_tool_use_block(item) {
                    tool_calls.push(call);
                }
            }
            other => {
                // redacted_thinking / 未知块：显性丢弃
                drop_unsupported(
                    Protocol::Anthropic,
                    &format!("content[].{}", other.unwrap_or("unknown")),
                    "block type has no IR equivalent",
                );
            }
        }
    }

    let text = text_parts.join("\n");
    let reasoning = if reasoning_parts.is_empty() {
        None
    } else {
        Some(reasoning_parts.join("\n"))
    };

    // 组合进 TextWithToolCalls 时签名无槽 → 显性丢弃
    let drop_signature = |reasoning_signature: &Option<String>| {
        if reasoning_signature.is_some() {
            drop_unsupported(
                Protocol::Anthropic,
                "thinking.signature",
                "TextWithToolCalls.reasoning has no signature slot",
            );
        }
    };

    if !tool_calls.is_empty() {
        if text.is_empty() && reasoning.is_none() {
            // 仅工具调用：保旧 ToolCalls 形状
            MessageContent::ToolCalls(tool_calls)
        } else {
            drop_signature(&reasoning_signature);
            MessageContent::TextWithToolCalls {
                text,
                tool_calls,
                reasoning,
            }
        }
    } else if let Some(reasoning) = reasoning {
        if text.is_empty() {
            MessageContent::Reasoning {
                text: reasoning,
                signature: reasoning_signature,
            }
        } else {
            drop_signature(&reasoning_signature);
            MessageContent::TextWithToolCalls {
                text,
                tool_calls: Vec::new(),
                reasoning: Some(reasoning),
            }
        }
    } else {
        MessageContent::Text(text)
    }
}

/// 解析 Anthropic tool_use 块 → ToolCall（parse_response 与
/// parse_anthropic_message 共用）
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

/// 聚合 content 数组中的 tool_result 块 → ToolResult：并行工具调用的
/// 多个 tool_result 以首块 tool_use_id 为准、content 换行拼接（Message
/// 只承载单个 ToolResult）；同消息 text 块一并按原文顺序并入
fn aggregate_anthropic_tool_result(arr: &[Value]) -> ToolResult {
    let mut tool_call_id = String::new();
    let mut contents: Vec<String> = Vec::new();
    let mut is_error = false;

    for block in arr.iter() {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    contents.push(t.to_string());
                }
            }
            Some("tool_result") => {
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
            _ => {}
        }
    }

    ToolResult {
        tool_call_id,
        function_name: String::new(),
        content: contents.join("\n"),
        is_error,
    }
}

/// 提取 System 消息文本片段（返回分片列表供出站去重合并——
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

/// Anthropic stop_reason → OpenAI finish_reason 静态映射。
/// IR 的 finish_reason 统一存 OpenAI 词汇；未知值原样保留。
fn anthropic_stop_reason_to_finish_reason(s: &str) -> String {
    match s {
        "tool_use" => "tool_calls".to_string(),
        "max_tokens" => "length".to_string(),
        "end_turn" | "stop_sequence" => "stop".to_string(),
        other => other.to_string(),
    }
}

/// OpenAI finish_reason → Anthropic stop_reason 静态映射（反向）。
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
        "text" => {
            if json.get("citations").is_some() {
                // 引用块无 IR 槽：显性丢弃（R-012）
                drop_unsupported(
                    Protocol::Anthropic,
                    "content[].citations",
                    "citations have no IR slot",
                );
            }
            Some(ContentBlock::Text {
                text: json.get("text")?.as_str()?.to_string(),
            })
        }
        "image" => {
            let source = json.get("source")?.as_object()?;
            // base64 与 url 两种 source 形态；未知形态不猜变体
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
        "document" => {
            let source = json.get("source")?.as_object()?;
            let (source, media_type) = match source.get("type").and_then(|v| v.as_str())? {
                "base64" => {
                    let media_type = source.get("media_type")?.as_str()?.to_string();
                    (
                        ImageSource::Base64 {
                            media_type: media_type.clone(),
                            data: source.get("data")?.as_str()?.to_string(),
                        },
                        media_type,
                    )
                }
                "url" => (
                    ImageSource::Url {
                        url: source.get("url")?.as_str()?.to_string(),
                    },
                    // url 形态不携带 media_type（Anthropic wire 定义）；
                    // 取缺省 document MIME 保 IR 槽完整
                    source
                        .get("media_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/pdf")
                        .to_string(),
                ),
                // 未知 source 形态不猜变体
                _ => return None,
            };
            Some(ContentBlock::Document {
                source,
                media_type,
                title: json
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                cache_control: json.get("cache_control").cloned(),
            })
        }
        other => {
            // 未知块型（redacted_thinking / 未来块等）：显性丢弃（R-012）
            drop_unsupported(
                Protocol::Anthropic,
                &format!("content[].{other}"),
                "block type has no IR equivalent",
            );
            None
        }
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
                    ContentBlock::Document {
                        source,
                        title,
                        cache_control,
                        ..
                    } => {
                        let mut block = json!({
                            "type": "document",
                            "source": match source {
                                ImageSource::Base64 { media_type, data } => json!({
                                    "type": "base64",
                                    "media_type": media_type,
                                    "data": data,
                                }),
                                ImageSource::Url { url } => json!({
                                    "type": "url",
                                    "url": url,
                                }),
                            },
                        });
                        if let Some(title) = title {
                            block["title"] = json!(title);
                        }
                        if let Some(cache_control) = cache_control {
                            block["cache_control"] = cache_control.clone();
                        }
                        block
                    }
                    ContentBlock::Audio { .. } => {
                        // Anthropic 无 audio 内容块 → 显性丢弃
                        drop_unsupported(
                            Protocol::Anthropic,
                            "content[].input_audio",
                            "Anthropic has no audio content block",
                        );
                        json!({ "type": "unknown" })
                    }
                    ContentBlock::Type(t) => {
                        drop_unsupported(
                            Protocol::Anthropic,
                            &format!("content[].{t}"),
                            "content block type has no Anthropic equivalent",
                        );
                        json!({ "type": "unknown" })
                    }
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
            // Anthropic 规范字段为 tool_use_id（非 tool_call_id，缺失会被上游 400 拒收）
            "tool_use_id": result.tool_call_id,
            "content": result.content,
        }]),
        MessageContent::TextWithToolCalls {
            text,
            tool_calls,
            reasoning,
        } => {
            // D3：thinking 块先行（reasoning 有值时）；空 text 不写块
            //（Anthropic 文本块要求非空）；tool_use 随后
            let mut arr: Vec<Value> = Vec::new();
            if let Some(reasoning) = reasoning {
                arr.push(json!({
                    "type": "thinking",
                    "thinking": reasoning,
                }));
            }
            if !text.is_empty() {
                arr.push(json!({
                    "type": "text",
                    "text": text,
                }));
            }
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
        MessageContent::Reasoning { text, signature } => {
            // D3：思考内容出站为原生 thinking 块（signature 存在时原样回写）
            let mut block = json!({
                "type": "thinking",
                "thinking": text,
            });
            if let Some(signature) = signature {
                block["signature"] = json!(signature);
            }
            json!([block])
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

/// Anthropic `output_config.format` → IR `response_format`。
///
/// `{type:"json_schema", schema}` → IR `{type:"json_schema",
/// json_schema:{name:"response", schema}}`（IR 承载 OpenAI 形状）；
/// 未知类型/缺 schema 显性丢弃（debug 可检索）。
fn parse_anthropic_output_format(format: &Value) -> Option<Value> {
    let ty = format.get("type").and_then(|v| v.as_str())?;
    match ty {
        "json_schema" => match format.get("schema") {
            Some(schema) => Some(json!({
                "type": "json_schema",
                "json_schema": {"name": "response", "schema": schema}
            })),
            None => {
                drop_unsupported(
                    Protocol::Anthropic,
                    "output_config.format",
                    "json_schema without schema",
                );
                None
            }
        },
        other => {
            drop_unsupported(
                Protocol::Anthropic,
                "output_config.format",
                &format!("unsupported format type '{other}'"),
            );
            None
        }
    }
}

/// Anthropic `tool_choice` → IR ToolChoice：原生形状 any/tool/auto/none 直读
///（any 即「必须用工具」，映射 IR required）；OpenAI function 对象形状保留
/// 容错；标量兜底 auto。
fn parse_anthropic_tool_choice(tc: &Value) -> ToolChoice {
    match tc {
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
    }
}

/// Anthropic `thinking` → IR ThinkingConfig：adaptive/enabled/disabled/
/// between_tools 四变体与 display（summarized/omitted）全解析；enabled
/// 无预算退化为 Adaptive。
fn parse_anthropic_thinking(body: &Value) -> Option<ThinkingConfig> {
    body.get("thinking")
        .and_then(|v| v.as_object())
        .and_then(|t| {
            let display = match t.get("display").and_then(|v| v.as_str()) {
                Some("summarized") => Some(ThinkingDisplay::Summarized),
                Some("omitted") => Some(ThinkingDisplay::Omitted),
                Some(other) => {
                    // 词表外 display 显性丢弃（防回写未知词被上游 400）
                    drop_unsupported(
                        Protocol::Anthropic,
                        "thinking.display",
                        &format!("unsupported display value '{other}'"),
                    );
                    None
                }
                None => None,
            };
            match t.get("type").and_then(|v| v.as_str()) {
                Some("enabled") => Some(match t.get("budget_tokens").and_then(|v| v.as_u64()) {
                    Some(budget) => ThinkingConfig::Enabled {
                        budget_tokens: budget,
                        display,
                    },
                    None => ThinkingConfig::Adaptive { display },
                }),
                Some("adaptive") => Some(ThinkingConfig::Adaptive { display }),
                Some("between_tools") => Some(ThinkingConfig::BetweenTools),
                Some("disabled") => Some(ThinkingConfig::Disabled),
                _ => None,
            }
        })
}

/// 解析 Anthropic 格式的工具定义：function 工具压入 function 槽；
/// server tool（带 `type` 且无 `input_schema` 的非 function 形态，如
/// `web_search_20250305`）原样保真进 `raw`，不再被压成 function；
/// `cache_control` 两种形态都保留。
fn parse_anthropic_tool(json: &Value) -> Option<Tool> {
    let obj = json.as_object()?;
    let cache_control = obj.get("cache_control").cloned();
    let tool_type = obj.get("type").and_then(|v| v.as_str());

    // server tool：带 type 且无 input_schema → 原对象保真（出站原样回写）
    if obj.get("input_schema").is_none() && tool_type.is_some() {
        return Some(Tool {
            tool_type: tool_type.unwrap_or("function").to_string(),
            function: ToolFunction::default(),
            raw: Some(json.clone()),
            cache_control,
        });
    }

    // Anthropic 使用 name 而不是 function.name
    let name = obj.get("name")?.as_str()?.to_string();
    let description = obj
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();

    // 解析输入模式（M1 / D7.14 闭环：input_schema 原样 Value 保真
    // 透传——嵌套 schema / items / anyOf / $ref / 缺 type 属性全量无损）
    let parameters = obj.get("input_schema").cloned().unwrap_or(Value::Null);

    Some(Tool {
        tool_type: "function".to_string(),
        function: ToolFunction {
            name,
            description,
            parameters,
            // Anthropic input_schema 无 strict 概念，保持 None
            strict: None,
        },
        cache_control,
        ..Default::default()
    })
}

/// 将工具转换为 Anthropic 格式：`raw`（server tool 等非 function 保真
/// 形态）原样回写；function 工具按 input_schema 组装（Null 时补协议最小
/// 形状 `{"type":"object"}`——Anthropic 工具定义必填 input_schema）并回写
/// `cache_control`；OpenAI custom（`custom` 子对象形态）跨协议显式 400，
/// 不静默降级。
fn tool_to_anthropic(tool: &Tool, index: usize) -> Result<Value, GateError> {
    if tool.tool_type != "function" {
        if let Some(raw) = tool.raw.as_ref() {
            let openai_custom_shape = tool.tool_type == "custom" || raw.get("custom").is_some();
            if !openai_custom_shape {
                return Ok(raw.clone());
            }
        }
        return Err(GateError::Validation {
            field: format!("tools[{index}].type"),
        });
    }

    let input_schema = if tool.function.parameters.is_object() {
        tool.function.parameters.clone()
    } else {
        json!({ "type": "object" })
    };

    let mut tool_json = json!({
        "name": tool.function.name,
        "description": tool.function.description,
        "input_schema": input_schema,
    });
    if let Some(cache_control) = tool.cache_control.as_ref() {
        tool_json["cache_control"] = cache_control.clone();
    }
    Ok(tool_json)
}

/// Anthropic 自动缓存断点注入（纯函数，可测）：按锚点顺序在
/// tools/system/last_user 内容块上标记 `cache_control`；候选锚点超出上限
/// 时保留锚序尾部，enabled 为假或上限 0 不注入。返回实际注入数
pub fn inject_cache_control(body: &mut Value, policy: &CacheBreakpointPolicy) -> usize {
    if !policy.enabled || policy.max_breakpoints == 0 {
        return 0;
    }
    let cap = policy.max_breakpoints as usize;

    // 1) 先做锚点规划（不改 body），再截断到尾部 max，最后统一应用——
    //    截断语义 = 「保留锚序尾部 N 个」
    enum Edit {
        /// tools 末块标记
        MarkTools,
        /// system 字符串提升为单块数组（携带 text）
        PromoteSystem { text: String },
        /// system 数组末块标记
        MarkSystemTail,
        /// 最后一条 user 消息末块标记（content 为数组）
        MarkLastUserTail,
    }
    let mut plan: Vec<Edit> = Vec::new();
    for anchor in &policy.anchors {
        match anchor.as_str() {
            "tools" => {
                if body
                    .get("tools")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty())
                {
                    plan.push(Edit::MarkTools);
                }
            }
            "system" => match body.get("system") {
                Some(Value::String(text)) => {
                    plan.push(Edit::PromoteSystem { text: text.clone() });
                }
                Some(Value::Array(blocks)) if !blocks.is_empty() => {
                    plan.push(Edit::MarkSystemTail);
                }
                _ => {}
            },
            "last_user" => {
                let last_user_has_blocks = body
                    .get("messages")
                    .and_then(|v| v.as_array())
                    .is_some_and(|msgs| {
                        msgs.iter()
                            .rev()
                            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                            .is_some_and(|m| {
                                m.get("content")
                                    .is_some_and(|c| c.as_array().is_some_and(|a| !a.is_empty()))
                            })
                    });
                if last_user_has_blocks {
                    plan.push(Edit::MarkLastUserTail);
                }
            }
            other => {
                tracing::debug!(
                    anchor = %other,
                    "{}",
                    t("protocol-anthropic-unknown-cache-anchor")
                );
            }
        }
    }
    if plan.len() > cap {
        plan.drain(..plan.len() - cap);
    }

    // 2) 应用
    let mut injected = 0usize;
    for edit in plan {
        match edit {
            Edit::MarkTools => {
                if let Some(last) = body
                    .get_mut("tools")
                    .and_then(|v| v.as_array_mut())
                    .and_then(|a| a.last_mut())
                    .and_then(|t| t.as_object_mut())
                {
                    last.insert("cache_control".into(), json!({"type": "ephemeral"}));
                    injected += 1;
                }
            }
            Edit::PromoteSystem { text } => {
                body["system"] = json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": {"type": "ephemeral"},
                }]);
                injected += 1;
            }
            Edit::MarkSystemTail => {
                if let Some(last) = body
                    .get_mut("system")
                    .and_then(|v| v.as_array_mut())
                    .and_then(|a| a.last_mut())
                    .and_then(|b| b.as_object_mut())
                {
                    last.insert("cache_control".into(), json!({"type": "ephemeral"}));
                    injected += 1;
                }
            }
            Edit::MarkLastUserTail => {
                let target = body
                    .get_mut("messages")
                    .and_then(|v| v.as_array_mut())
                    .and_then(|msgs| {
                        msgs.iter_mut()
                            .rev()
                            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                    })
                    .and_then(|m| m.get_mut("content"))
                    .and_then(|c| c.as_array_mut())
                    .and_then(|a| a.last_mut())
                    .and_then(|b| b.as_object_mut());
                if let Some(block) = target {
                    block.insert("cache_control".into(), json!({"type": "ephemeral"}));
                    injected += 1;
                }
            }
        }
    }
    injected
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 组装最小合法 Anthropic 响应信封（content 由调用方给出）。
    fn response_with(content: Value) -> Value {
        json!({
            "id": "msg_1",
            "model": "claude-3-5-sonnet-20241022",
            "content": content,
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })
    }

    fn parse_message(content: Value) -> Message {
        AnthropicConverter::new()
            .parse_response(response_with(content))
            .expect("parse_response")
            .choices[0]
            .message
            .clone()
            .expect("message")
    }

    #[test]
    fn response_aggregates_thinking_and_all_text_blocks() {
        // [thinking, text, text]：正文两段拼接（不再只取首块/空串），
        // reasoning 保留
        let message = parse_message(json!([
            {"type": "thinking", "thinking": "想一想", "signature": "sig-1"},
            {"type": "text", "text": "第一段"},
            {"type": "text", "text": "第二段"}
        ]));
        match &message.content {
            MessageContent::TextWithToolCalls {
                text,
                tool_calls,
                reasoning,
            } => {
                assert_eq!(text, "第一段\n第二段");
                assert!(tool_calls.is_empty());
                assert_eq!(reasoning.as_deref(), Some("想一想"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn response_aggregates_thinking_with_tool_use_preserving_reasoning() {
        let message = parse_message(json!([
            {"type": "thinking", "thinking": "先查天气"},
            {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
             "input": {"city": "Beijing"}}
        ]));
        match &message.content {
            MessageContent::TextWithToolCalls {
                text,
                tool_calls,
                reasoning,
            } => {
                assert_eq!(text, "");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "toolu_1");
                assert_eq!(tool_calls[0].function.name, "get_weather");
                assert_eq!(reasoning.as_deref(), Some("先查天气"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn response_aggregates_multiple_tool_use_blocks() {
        // [tool_use, tool_use]（无 text/reasoning）：保旧 ToolCalls 形状，
        // 2 个 ToolCall 全保真
        let message = parse_message(json!([
            {"type": "tool_use", "id": "toolu_1", "name": "a", "input": {"x": 1}},
            {"type": "tool_use", "id": "toolu_2", "name": "b", "input": {"y": 2}}
        ]));
        match &message.content {
            MessageContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, "toolu_1");
                assert_eq!(calls[0].function.arguments, "{\"x\":1}");
                assert_eq!(calls[1].id, "toolu_2");
                assert_eq!(calls[1].function.name, "b");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn response_only_reasoning_maps_to_reasoning_with_signature() {
        let message = parse_message(json!([
            {"type": "thinking", "thinking": "独思", "signature": "sig-9"}
        ]));
        match &message.content {
            MessageContent::Reasoning { text, signature } => {
                assert_eq!(text, "独思");
                assert_eq!(signature.as_deref(), Some("sig-9"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn response_redacted_thinking_is_dropped_without_failure() {
        let message = parse_message(json!([
            {"type": "redacted_thinking", "data": "opaque"},
            {"type": "text", "text": "正文"}
        ]));
        match &message.content {
            MessageContent::Text(text) => assert_eq!(text, "正文"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn response_server_tool_use_aggregates_like_tool_use() {
        let message = parse_message(json!([
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
             "input": {"query": "rust"}}
        ]));
        match &message.content {
            MessageContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "web_search");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 最小可转换 Anthropic 请求（model/max_tokens/messages 三必需槽）。
    fn minimal_request() -> UnifiedRequest {
        AnthropicConverter::new()
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified")
    }

    #[test]
    fn disable_parallel_tool_use_maps_to_parallel_tool_calls() {
        let converter = AnthropicConverter::new();
        // true → Some(false)
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "auto", "disable_parallel_tool_use": true}
            }))
            .expect("to_unified");
        assert_eq!(request.parallel_tool_calls, Some(false));
        // false → None
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "auto", "disable_parallel_tool_use": false}
            }))
            .expect("to_unified");
        assert_eq!(request.parallel_tool_calls, None);
        // 缺省 → None
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "auto"}
            }))
            .expect("to_unified");
        assert_eq!(request.parallel_tool_calls, None);
    }

    #[test]
    fn tool_choice_none_writes_native_none() {
        let converter = AnthropicConverter::new();
        let mut request = minimal_request();
        request.tool_choice = Some(ToolChoice::Strategy("none".into()));
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["tool_choice"], json!({"type": "none"}));
    }

    #[test]
    fn parallel_tool_calls_writes_back_disable_parallel_tool_use() {
        let converter = AnthropicConverter::new();
        // Some(false) 且无 tool_choice → 补 auto + disable_parallel_tool_use=true
        let mut request = minimal_request();
        request.parallel_tool_calls = Some(false);
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["tool_choice"],
            json!({"type": "auto", "disable_parallel_tool_use": true})
        );

        // Specific + Some(true) → 显式 false
        let mut request = minimal_request();
        request.tool_choice = Some(ToolChoice::Specific {
            choice_type: "function".into(),
            function: ToolChoiceFunction {
                name: "get_weather".into(),
            },
        });
        request.parallel_tool_calls = Some(true);
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["tool_choice"],
            json!({"type": "tool", "name": "get_weather", "disable_parallel_tool_use": false})
        );

        // None → 不写开关、无 tool_choice 不产出字段
        let request = minimal_request();
        let body = converter.request_to_external(request).expect("to external");
        assert!(body.get("tool_choice").is_none());

        // required → any（回归）
        let mut request = minimal_request();
        request.tool_choice = Some(ToolChoice::Strategy("required".into()));
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["tool_choice"], json!({"type": "any"}));
    }

    #[test]
    fn output_config_effort_roundtrip_and_degradation() {
        let converter = AnthropicConverter::new();
        // 入站 xhigh → EffortLevel::XHigh
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "xhigh"}
            }))
            .expect("to_unified");
        assert_eq!(request.effort, Some(EffortLevel::XHigh));
        assert!(
            !request.vendor_extensions.contains_key("output_config"),
            "已识别子键不落扩展表"
        );
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["output_config"], json!({"effort": "xhigh"}));

        // 出站 None → 不写 output_config；Minimal → low
        let mut request = minimal_request();
        request.effort = Some(EffortLevel::None);
        let body = converter.request_to_external(request).expect("to external");
        assert!(body.get("output_config").is_none(), "None 不写字段");

        let mut request = minimal_request();
        request.effort = Some(EffortLevel::Minimal);
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["output_config"], json!({"effort": "low"}));

        let mut request = minimal_request();
        request.effort = Some(EffortLevel::Max);
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["output_config"], json!({"effort": "max"}));
    }

    #[test]
    fn output_config_format_json_schema_roundtrip() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {
                    "format": {"type": "json_schema", "schema": {"type": "object"}}
                }
            }))
            .expect("to_unified");
        assert_eq!(
            request.response_format,
            Some(json!({
                "type": "json_schema",
                "json_schema": {"name": "response", "schema": {"type": "object"}}
            }))
        );
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["output_config"],
            json!({"format": {"type": "json_schema", "schema": {"type": "object"}}})
        );

        // json_object 无 Anthropic 等价 → 丢弃 + debug（不产生 output_config）
        let mut request = minimal_request();
        request.response_format = Some(json!({"type": "json_object"}));
        let body = converter.request_to_external(request).expect("to external");
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn output_config_unknown_subkeys_refill_and_unknown_effort_dropped() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "banana", "future_flag": true, "level": 3}
            }))
            .expect("to_unified");
        assert!(request.effort.is_none(), "词表外 effort 丢弃");
        let retained = request
            .vendor_extensions
            .get("output_config")
            .expect("未知子键回填");
        assert_eq!(retained["future_flag"], true);
        assert_eq!(retained["level"], 3);
        assert!(retained.get("effort").is_none(), "已识别键不回填");
        // 出站合并回写未知子键
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["output_config"]["future_flag"], true);
        assert!(body["output_config"].get("effort").is_none());

        // 入站 output_config.format 未知类型 → 丢弃 + debug（不影响其他槽）
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "high", "format": {"type": "weird"}}
            }))
            .expect("to_unified");
        assert!(request.response_format.is_none());
        assert_eq!(request.effort, Some(EffortLevel::High));
    }

    #[test]
    fn thinking_four_variants_inbound_with_display() {
        let converter = AnthropicConverter::new();
        let parse = |thinking: Value| {
            converter
                .to_unified(json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "hi"}],
                    "thinking": thinking
                }))
                .expect("to_unified")
                .thinking_config
        };
        assert_eq!(
            parse(json!({"type": "enabled", "budget_tokens": 1024, "display": "summarized"})),
            Some(ThinkingConfig::Enabled {
                budget_tokens: 1024,
                display: Some(ThinkingDisplay::Summarized),
            })
        );
        assert_eq!(
            parse(json!({"type": "adaptive", "display": "omitted"})),
            Some(ThinkingConfig::Adaptive {
                display: Some(ThinkingDisplay::Omitted),
            })
        );
        assert_eq!(
            parse(json!({"type": "between_tools"})),
            Some(ThinkingConfig::BetweenTools)
        );
        assert_eq!(
            parse(json!({"type": "disabled"})),
            Some(ThinkingConfig::Disabled)
        );
        // enabled 无预算 → Adaptive（旧形状兼容）
        assert_eq!(
            parse(json!({"type": "enabled"})),
            Some(ThinkingConfig::Adaptive { display: None })
        );
        // 词表外 display → 丢弃为 None（不 panic）
        assert_eq!(
            parse(json!({"type": "adaptive", "display": "verbose"})),
            Some(ThinkingConfig::Adaptive { display: None })
        );
    }

    #[test]
    fn thinking_four_variants_outbound() {
        let converter = AnthropicConverter::new();
        // Enabled{budget>0, display} → 原生 enabled 全字段
        let mut request = minimal_request();
        request.thinking_config = Some(ThinkingConfig::Enabled {
            budget_tokens: 2048,
            display: Some(ThinkingDisplay::Omitted),
        });
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 2048, "display": "omitted"})
        );

        // Enabled{budget:0} → adaptive（不带 budget）
        let mut request = minimal_request();
        request.thinking_config = Some(ThinkingConfig::Enabled {
            budget_tokens: 0,
            display: Some(ThinkingDisplay::Summarized),
        });
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );

        // BetweenTools → between_tools
        let mut request = minimal_request();
        request.thinking_config = Some(ThinkingConfig::BetweenTools);
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["thinking"], json!({"type": "between_tools"}));

        // Disabled → 省略字段
        let mut request = minimal_request();
        request.thinking_config = Some(ThinkingConfig::Disabled);
        let body = converter.request_to_external(request).expect("to external");
        assert!(
            body.get("thinking").is_none(),
            "Disabled 不出现 thinking 键"
        );

        // None → 省略
        let body = converter
            .request_to_external(minimal_request())
            .expect("to external");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn document_block_roundtrip_base64_and_url() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "QUJD"}, "title": "doc.pdf", "cache_control": {"type": "ephemeral"}},
                        {"type": "document", "source": {"type": "url", "url": "https://example.com/doc.pdf"}},
                        {"type": "text", "text": "读一下"}
                    ]
                }]
            }))
            .expect("to_unified");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 3);
                match &blocks[0] {
                    ContentBlock::Document {
                        source,
                        media_type,
                        title,
                        cache_control,
                    } => {
                        assert_eq!(media_type, "application/pdf");
                        assert_eq!(title.as_deref(), Some("doc.pdf"));
                        assert_eq!(cache_control, &Some(json!({"type": "ephemeral"})));
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
                match &blocks[1] {
                    ContentBlock::Document {
                        source, media_type, ..
                    } => {
                        assert_eq!(media_type, "application/pdf");
                        assert!(matches!(source, ImageSource::Url { .. }));
                    }
                    other => panic!("unexpected: {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
        // 出站回写：document 全字段（source/title/cache_control）
        let body = converter.request_to_external(request).expect("to external");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "document");
        assert_eq!(
            content[0]["source"],
            json!({"type": "base64", "media_type": "application/pdf", "data": "QUJD"})
        );
        assert_eq!(content[0]["title"], "doc.pdf");
        assert_eq!(content[0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(
            content[1]["source"],
            json!({"type": "url", "url": "https://example.com/doc.pdf"})
        );
        assert_eq!(content[2]["text"], "读一下");
    }

    #[test]
    fn assistant_history_thinking_maps_to_reasoning() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [
                    {"role": "user", "content": "天气?"},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "需要查", "signature": "sig-1"},
                        {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "BJ"}}
                    ]}
                ]
            }))
            .expect("to_unified");
        match &request.messages[1].content {
            MessageContent::TextWithToolCalls {
                text,
                tool_calls,
                reasoning,
            } => {
                assert_eq!(text, "");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(reasoning.as_deref(), Some("需要查"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        // 出站：thinking 块先行、空 text 不写块（TextWithToolCalls.reasoning
        // 无 signature 槽 → 不带 signature）
        let body = converter.request_to_external(request).expect("to external");
        let content = &body["messages"][1]["content"];
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "需要查");
        assert!(content[0].get("signature").is_none());
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "get_weather");
        assert!(content.as_array().unwrap().len() == 2, "空 text 不写文本块");
    }

    #[test]
    fn audio_block_outbound_dropped_without_panic() {
        let converter = AnthropicConverter::new();
        let mut request = minimal_request();
        request.messages[0].content = MessageContent::MultiModal(vec![
            ContentBlock::Text { text: "听".into() },
            ContentBlock::Audio {
                data: "QUJD".into(),
                format: "wav".into(),
            },
        ]);
        let body = converter.request_to_external(request).expect("to external");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["text"], "听");
        assert_eq!(content[1]["type"], "unknown", "Audio 显性丢弃占位");
    }

    #[test]
    fn server_tool_raw_fidelity_roundtrip() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [
                    {"type": "web_search_20250305", "name": "web_search", "max_uses": 3}
                ]
            }))
            .expect("to_unified");
        assert_eq!(request.tools[0].tool_type, "web_search_20250305");
        assert!(
            request.tools[0].raw.is_some(),
            "server tool 原对象保真进 raw（不再压成 function）"
        );
        assert!(request.tools[0].function.name.is_empty());

        // 出站原样回写
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(
            body["tools"][0],
            json!({"type": "web_search_20250305", "name": "web_search", "max_uses": 3})
        );
    }

    #[test]
    fn function_tool_cache_control_roundtrip() {
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [
                    {"name": "get_weather", "description": "查天气",
                     "input_schema": {"type": "object"},
                     "cache_control": {"type": "ephemeral"}}
                ]
            }))
            .expect("to_unified");
        assert_eq!(request.tools[0].tool_type, "function");
        assert_eq!(
            request.tools[0].cache_control,
            Some(json!({"type": "ephemeral"}))
        );
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(
            body["tools"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn cross_protocol_custom_tool_returns_validation_400() {
        let converter = AnthropicConverter::new();
        // OpenAI custom（custom 子对象形态）→ Anthropic 显式 400
        let mut request = minimal_request();
        request.tools = vec![Tool {
            tool_type: "custom".into(),
            raw: Some(json!({"type": "custom", "custom": {"name": "fmt"}})),
            ..Default::default()
        }];
        let err = converter
            .request_to_external(request)
            .expect_err("custom 工具跨协议必须 400");
        match err {
            crate::error::GateError::Validation { field } => {
                assert_eq!(field, "tools[0].type");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 非 function 且 raw 缺失 → 同样 400
        let mut request = minimal_request();
        request.tools = vec![Tool {
            tool_type: "web_search_20250305".into(),
            ..Default::default()
        }];
        let err = converter
            .request_to_external(request)
            .expect_err("无 raw 非 function 必须 400");
        match err {
            crate::error::GateError::Validation { field } => {
                assert_eq!(field, "tools[0].type");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn metadata_user_and_service_tier_roundtrip() {
        let converter = AnthropicConverter::new();
        // metadata.user_id → IR user；metadata 整体保留
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "metadata": {"user_id": "u1", "trace": "t"},
                "service_tier": "auto"
            }))
            .expect("to_unified");
        assert_eq!(request.user.as_deref(), Some("u1"));
        assert_eq!(
            request.metadata,
            Some(json!({"user_id": "u1", "trace": "t"}))
        );
        assert_eq!(request.service_tier.as_deref(), Some("auto"));
        assert!(
            !request.vendor_extensions.contains_key("metadata"),
            "metadata 为协议键，不落扩展表"
        );

        // 出站：metadata 两键保留、service_tier 原名回写
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["metadata"], json!({"user_id": "u1", "trace": "t"}));
        assert_eq!(body["service_tier"], "auto");
    }

    #[test]
    fn user_backfills_metadata_user_id_and_standard_only_passes() {
        let converter = AnthropicConverter::new();
        // IR user 且 metadata 无 user_id → 补 user_id
        let mut request = minimal_request();
        request.user = Some("u9".into());
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["metadata"], json!({"user_id": "u9"}));

        // metadata 已有 user_id 时不被 IR user 覆盖
        let mut request = minimal_request();
        request.user = Some("u9".into());
        request.metadata = Some(json!({"user_id": "u1"}));
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["metadata"], json!({"user_id": "u1"}));

        // 空 metadata 且无 user → 不写字段
        let body = converter
            .request_to_external(minimal_request())
            .expect("to external");
        assert!(body.get("metadata").is_none());

        // standard_only 同名映射；词表外值显性丢弃（不 panic）
        let mut request = minimal_request();
        request.service_tier = Some("standard_only".into());
        let body = converter.request_to_external(request).expect("to external");
        assert_eq!(body["service_tier"], "standard_only");

        let mut request = minimal_request();
        request.service_tier = Some("flex".into());
        let body = converter.request_to_external(request).expect("to external");
        assert!(body.get("service_tier").is_none());

        // 入站词表外值 → None（debug 可检索）
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}],
                "service_tier": "flex"
            }))
            .expect("to_unified");
        assert!(request.service_tier.is_none());
    }

    #[test]
    fn stop_sequence_roundtrip() {
        let converter = AnthropicConverter::new();
        // 命中停止序列：实际值入 IR 并出站回写（R-011）
        let response = converter
            .parse_response(json!({
                "id": "msg_1",
                "model": "claude-3-5-sonnet-20241022",
                "content": [{"type": "text", "text": "部分"}],
                "stop_reason": "stop_sequence",
                "stop_sequence": "END",
                "usage": {"input_tokens": 3, "output_tokens": 2}
            }))
            .expect("parse_response");
        assert_eq!(response.stop_sequence.as_deref(), Some("END"));
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(body["stop_sequence"], "END");
        assert_eq!(body["stop_reason"], "stop_sequence");

        // 未命中 → None → 出站 null
        let response = converter
            .parse_response(json!({
                "id": "msg_2",
                "model": "claude",
                "content": [{"type": "text", "text": "完整"}],
                "stop_reason": "end_turn",
                "stop_sequence": Value::Null,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .expect("parse_response");
        assert!(response.stop_sequence.is_none());
        let body = converter
            .response_to_external(response)
            .expect("to external");
        assert!(body["stop_sequence"].is_null());
    }

    #[test]
    fn stream_thinking_and_signature_deltas_parse_to_reasoning() {
        // thinking_delta → Reasoning{text}
        match parse_sse_frame_to_chunk(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"推理中"}}"#,
        ) {
            Some(StreamChunk::Content { index, delta, .. }) => {
                assert_eq!(index, 1);
                match delta {
                    MessageContent::Reasoning { text, signature } => {
                        assert_eq!(text, "推理中");
                        assert_eq!(signature, None);
                    }
                    other => panic!("unexpected: {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
        // signature_delta → Reasoning{text:"", signature}
        match parse_sse_frame_to_chunk(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sig-x"}}"#,
        ) {
            Some(StreamChunk::Content { delta, .. }) => match delta {
                MessageContent::Reasoning { text, signature } => {
                    assert_eq!(text, "");
                    assert_eq!(signature.as_deref(), Some("sig-x"));
                }
                other => panic!("unexpected: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn stream_encoder_rebuilds_thinking_event_sequence() {
        let mut encoder = AnthropicStreamEncoder::new();
        let mut frames: Vec<Value> = Vec::new();
        for chunk in [
            StreamChunk::Content {
                index: 0,
                delta: MessageContent::Reasoning {
                    text: "想".into(),
                    signature: None,
                },
                finish_reason: None,
                logprobs: None,
            },
            StreamChunk::Content {
                index: 0,
                delta: MessageContent::Reasoning {
                    text: String::new(),
                    signature: Some("sig".into()),
                },
                finish_reason: None,
                logprobs: None,
            },
            // 思考后接文本：思考块先闭合，文本另开块
            StreamChunk::Content {
                index: 0,
                delta: MessageContent::Text("答".into()),
                finish_reason: None,
                logprobs: None,
            },
            StreamChunk::Usage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                finish_reason: Some("stop".into()),
            },
        ] {
            for frame in encoder.encode_chunk(chunk) {
                frames.push(serde_json::from_str(&frame).expect("frame json"));
            }
        }

        let types: Vec<&str> = frames
            .iter()
            .map(|f| f["type"].as_str().expect("type"))
            .collect();
        assert_eq!(
            types,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
            ],
            "事件序：thinking 块开→thinking_delta→signature_delta→闭；文本块开→delta→闭；message_delta"
        );
        // thinking 块细节
        assert_eq!(frames[1]["content_block"]["type"], "thinking");
        assert_eq!(frames[2]["delta"]["type"], "thinking_delta");
        assert_eq!(frames[2]["delta"]["thinking"], "想");
        assert_eq!(frames[3]["delta"]["type"], "signature_delta");
        assert_eq!(frames[3]["delta"]["signature"], "sig");
        // 块序号不混流：thinking=0、text=1
        assert_eq!(frames[1]["index"], 0);
        assert_eq!(frames[3]["index"], 0);
        assert_eq!(frames[4]["index"], 0);
        assert_eq!(frames[5]["index"], 1);
        assert_eq!(frames[5]["content_block"]["type"], "text");
        assert_eq!(frames[6]["delta"]["type"], "text_delta");
    }

    #[test]
    fn unknown_blocks_and_citations_are_dropped_without_failure() {
        // 多模态未知块型 → None（不猜变体，debug 可检索）
        assert!(parse_anthropic_block(&json!({"type": "mystery"})).is_none());
        // text 块带 citations：文本保留、引用显性丢弃
        match parse_anthropic_block(&json!({
            "type": "text",
            "text": "有引用",
            "citations": [{"type": "web_search_result_location"}]
        })) {
            Some(ContentBlock::Text { text }) => assert_eq!(text, "有引用"),
            other => panic!("unexpected: {other:?}"),
        }

        // 消息内容数组中的未知块被跳过且不失败
        let converter = AnthropicConverter::new();
        let request = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 64,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "保留"},
                        {"type": "redacted_thinking", "data": "xxx"}
                    ]
                }]
            }))
            .expect("to_unified");
        match &request.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 1, "未知块被丢弃");
                assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "保留"));
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 响应聚合中 citations 显性丢弃、正文保留
        let response = converter
            .parse_response(json!({
                "id": "msg_1",
                "model": "claude",
                "content": [{
                    "type": "text",
                    "text": "带引用",
                    "citations": [{"type": "char_location"}]
                }],
                "stop_reason": "end_turn"
            }))
            .expect("parse_response");
        match &response.choices[0]
            .message
            .as_ref()
            .expect("message")
            .content
        {
            MessageContent::Text(text) => assert_eq!(text, "带引用"),
            other => panic!("unexpected: {other:?}"),
        }

        // 流：未知增量型（citations_delta）→ None（不产出空文本）
        assert!(parse_sse_frame_to_chunk(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location"}}}"#
        )
        .is_none());
        // 流：未知块 start（redacted_thinking）→ None
        assert!(parse_sse_frame_to_chunk(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"xxx"}}"#
        )
        .is_none());
    }
}
