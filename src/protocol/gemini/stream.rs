// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Gemini SSE：上游 chunk → IR `StreamChunk` 反解，与 IR → Gemini SSE 编码器。
//!
//! Gemini 无 `event:` 名与 `[DONE]`：每帧 `data` 是一个完整
//! `GenerateContentResponse` chunk；`functionCall` 以完整对象到达（非增量），
//! 因此编码侧按 index 缓冲 IR 工具片段，在终止帧一次性 flush。

use super::types::parse_response;
use crate::protocol::unified::{MessageContent, StreamChunk, ToolCall, ToolCallFunction};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// 上游 SSE `data` → IR chunk。无法映射（心跳/非 JSON/空候选）→ None。
pub fn parse_stream_frame(data: &str) -> Option<StreamChunk> {
    let value: Value = serde_json::from_str(data).ok()?;
    let parsed = parse_response(&value)?;
    let finish_raw = parsed.finish_reason().map(str::to_string);
    let finish_wire = finish_raw.as_deref().map(finish_to_ir);

    if let Some(usage) = parsed.usage_metadata.as_ref() {
        return Some(StreamChunk::Usage {
            prompt_tokens: usage.prompt_token_count.min(u32::MAX as u64) as u32,
            completion_tokens: usage
                .candidates_token_count
                .saturating_add(usage.thoughts_token_count)
                .min(u32::MAX as u64) as u32,
            total_tokens: usage.total_token_count.min(u32::MAX as u64) as u32,
            cache_read_tokens: usage.cached_content_token_count.min(u32::MAX as u64) as u32,
            cache_creation_tokens: 0,
            finish_reason: finish_wire.map(str::to_string),
        });
    }

    let candidate = parsed.candidates.first()?;
    let mut text = String::new();
    let mut calls = Vec::new();
    if let Some(content) = candidate.content.as_ref() {
        for part in &content.parts {
            if let Some(chunk) = part.text.as_deref() {
                if !part.thought {
                    text.push_str(chunk);
                }
            } else if let Some(call) = part.function_call.as_ref() {
                calls.push(ToolCall {
                    id: format!("call_{}", uuid::Uuid::new_v4()),
                    call_type: "function".into(),
                    function: ToolCallFunction {
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
    }

    if !calls.is_empty() {
        if text.is_empty() && calls.len() == 1 {
            let call = calls.remove(0);
            return Some(StreamChunk::ToolCall {
                index: 0,
                id: call.id,
                name: Some(call.function.name),
                arguments: Some(call.function.arguments),
            });
        }
        if text.is_empty() {
            return Some(StreamChunk::Content {
                index: candidate.index.unwrap_or(0),
                delta: MessageContent::ToolCalls(calls),
                finish_reason: finish_wire.map(str::to_string),
            });
        }
        return Some(StreamChunk::Content {
            index: candidate.index.unwrap_or(0),
            delta: MessageContent::TextWithToolCalls {
                text,
                tool_calls: calls,
            },
            finish_reason: finish_wire.map(str::to_string),
        });
    }

    if text.is_empty() && finish_wire.is_none() {
        return None;
    }
    Some(StreamChunk::Content {
        index: candidate.index.unwrap_or(0),
        delta: MessageContent::Text(text),
        finish_reason: finish_wire.map(str::to_string),
    })
}

/// IR → Gemini SSE 帧编码器（有状态：工具片段缓冲、终止帧去重）。
#[derive(Debug)]
pub struct GeminiStreamEncoder {
    tool_calls: BTreeMap<u32, PendingCall>,
    finish: Option<String>,
    finish_sent: bool,
    usage: Option<UsageFrame>,
    usage_sent: bool,
}

#[derive(Debug, Default)]
struct PendingCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Copy)]
struct UsageFrame {
    prompt: u32,
    completion: u32,
    total: u32,
    cache_read: u32,
}

impl Default for GeminiStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiStreamEncoder {
    pub fn new() -> Self {
        Self {
            tool_calls: BTreeMap::new(),
            finish: None,
            finish_sent: false,
            usage: None,
            usage_sent: false,
        }
    }

    /// 消费一个 IR chunk，返回需要写出的零到多帧。
    pub fn encode(&mut self, chunk: StreamChunk) -> Vec<Value> {
        match chunk {
            StreamChunk::Content {
                delta,
                finish_reason,
                ..
            } => {
                let mut frames = Vec::new();
                if let Some(reason) = finish_reason {
                    self.finish = Some(reason);
                }
                match delta {
                    MessageContent::Text(text) => {
                        if !text.is_empty() {
                            frames.push(text_frame(&text, None));
                        }
                    }
                    MessageContent::MultiModal(blocks) => {
                        let mut parts = Vec::new();
                        for block in blocks {
                            match block {
                                crate::protocol::ContentBlock::Text { text } => {
                                    parts.push(json!({"text": text}));
                                }
                                crate::protocol::ContentBlock::Image {
                                    source:
                                        crate::protocol::ImageSource::Base64 { media_type, data },
                                    ..
                                } => parts.push(json!({"inlineData": {
                                    "mimeType": media_type, "data": data
                                }})),
                                crate::protocol::ContentBlock::Image {
                                    source: crate::protocol::ImageSource::Url { url },
                                    ..
                                } => parts.push(json!({"fileData": {"fileUri": url}})),
                                other => parts.push(json!({"text": format!("{other:?}")})),
                            }
                        }
                        if !parts.is_empty() {
                            frames.push(parts_frame(parts, None));
                        }
                    }
                    MessageContent::ToolCalls(calls) => {
                        for (index, call) in calls.into_iter().enumerate() {
                            self.push_call(index as u32, &call);
                        }
                    }
                    MessageContent::TextWithToolCalls { text, tool_calls } => {
                        if !text.is_empty() {
                            frames.push(text_frame(&text, None));
                        }
                        for (index, call) in tool_calls.into_iter().enumerate() {
                            self.push_call(index as u32, &call);
                        }
                    }
                    MessageContent::ToolResult(result) => {
                        frames.push(parts_frame(
                            vec![json!({"functionResponse": {
                                "name": result.function_name,
                                "response": serde_json::from_str::<Value>(&result.content)
                                    .ok()
                                    .filter(Value::is_object)
                                    .unwrap_or_else(|| json!({"result": result.content})),
                            }})],
                            None,
                        ));
                    }
                }
                if self.finish.is_some() && !self.finish_sent {
                    frames.extend(self.flush_finish());
                }
                frames.extend(self.flush_usage());
                frames
            }
            StreamChunk::ToolCall {
                index,
                id: _,
                name,
                arguments,
            } => {
                if let Some(name) = name {
                    self.tool_calls.entry(index).or_default().name = name;
                }
                if let Some(arguments) = arguments {
                    self.tool_calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(&arguments);
                }
                Vec::new()
            }
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
                finish_reason,
                ..
            } => {
                if let Some(reason) = finish_reason {
                    self.finish = Some(reason);
                }
                self.usage = Some(UsageFrame {
                    prompt: prompt_tokens,
                    completion: completion_tokens,
                    total: total_tokens,
                    cache_read: cache_read_tokens,
                });
                let mut frames = Vec::new();
                if self.finish.is_some() && !self.finish_sent {
                    frames.extend(self.flush_finish());
                }
                frames.extend(self.flush_usage());
                frames
            }
            StreamChunk::Heartbeat => Vec::new(),
        }
    }

    /// 流结束兜底：未发终止帧/usage 时补发（无 finish 按 STOP）。
    pub fn finish(&mut self) -> Vec<Value> {
        let mut frames = Vec::new();
        if !self.finish_sent {
            frames.extend(self.flush_finish());
        }
        frames.extend(self.flush_usage());
        frames
    }

    fn push_call(&mut self, index: u32, call: &ToolCall) {
        let entry = self.tool_calls.entry(index).or_default();
        entry.name = call.function.name.clone();
        entry.arguments.push_str(&call.function.arguments);
    }

    fn flush_finish(&mut self) -> Vec<Value> {
        if self.finish_sent {
            return Vec::new();
        }
        let finish = self
            .finish
            .as_deref()
            .map(finish_to_gemini)
            .unwrap_or("STOP");
        let parts = std::mem::take(&mut self.tool_calls)
            .into_values()
            .map(|call| {
                let args = serde_json::from_str::<Value>(&call.arguments)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({"raw": call.arguments}));
                json!({"functionCall": {"name": call.name, "args": args}})
            })
            .collect::<Vec<_>>();
        self.finish_sent = true;
        vec![parts_frame(parts, Some(finish))]
    }

    fn flush_usage(&mut self) -> Vec<Value> {
        if self.usage_sent {
            return Vec::new();
        }
        let Some(usage) = self.usage else {
            return Vec::new();
        };
        self.usage_sent = true;
        let mut metadata = json!({
            "promptTokenCount": usage.prompt,
            "candidatesTokenCount": usage.completion,
            "totalTokenCount": usage.total,
        });
        if usage.cache_read > 0 {
            metadata["cachedContentTokenCount"] = json!(usage.cache_read);
        }
        vec![json!({"usageMetadata": metadata})]
    }
}

fn text_frame(text: &str, finish: Option<&str>) -> Value {
    parts_frame(vec![json!({"text": text})], finish)
}

fn parts_frame(parts: Vec<Value>, finish: Option<&str>) -> Value {
    let mut candidate = json!({
        "content": {"role": "model", "parts": parts},
        "index": 0,
    });
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    json!({"candidates": [candidate]})
}

/// IR wire 词 → Gemini finishReason。
fn finish_to_gemini(raw: &str) -> &'static str {
    match raw {
        "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        "tool_calls" | "stop" | "function_call" => "STOP",
        _ => "OTHER",
    }
}

/// Gemini finishReason → IR wire 词。
fn finish_to_ir(raw: &str) -> &'static str {
    match raw {
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "LANGUAGE" => {
            "content_filter"
        }
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => "tool_calls",
        _ => "stop",
    }
}

/// `Part` 反序列化辅助：仅测试用（确保 wire 形状与类型定义一致）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::gemini::types::Part;
    use serde_json::json;

    fn part_from(value: Value) -> Part {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parses_text_delta_and_finish() {
        let chunk = parse_stream_frame(
            &json!({
                "candidates": [{"content": {"parts": [{"text": "he"}]}}]
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            chunk,
            StreamChunk::Content { ref delta, .. }
                if matches!(delta, MessageContent::Text(text) if text == "he")
        ));

        let finish = parse_stream_frame(
            &json!({
                "candidates": [{"content": {"parts": []}, "finishReason": "MAX_TOKENS"}]
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            finish,
            StreamChunk::Content { finish_reason: Some(ref r), .. } if r == "length"
        ));
    }

    #[test]
    fn parses_usage_frame_with_finish() {
        let chunk = parse_stream_frame(
            &json!({
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5,
                    "thoughtsTokenCount": 2,
                    "totalTokenCount": 17,
                    "cachedContentTokenCount": 3
                },
                "candidates": [{"finishReason": "STOP"}]
            })
            .to_string(),
        )
        .unwrap();
        match chunk {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
                finish_reason,
                ..
            } => {
                assert_eq!(prompt_tokens, 10);
                assert_eq!(completion_tokens, 7);
                assert_eq!(total_tokens, 17);
                assert_eq!(cache_read_tokens, 3);
                assert_eq!(finish_reason.as_deref(), Some("stop"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parses_function_call_and_skips_thought_text() {
        let chunk = parse_stream_frame(
            &json!({
                "candidates": [{"content": {"parts": [
                    {"text": "thinking", "thought": true},
                    {"functionCall": {"name": "f", "args": {"x": 1}}}
                ]}}]
            })
            .to_string(),
        )
        .unwrap();
        match chunk {
            StreamChunk::ToolCall {
                name, arguments, ..
            } => {
                assert_eq!(name.as_deref(), Some("f"));
                assert!(arguments.unwrap().contains("\"x\":1"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn encoder_streams_text_and_flushes_tools_on_finish() {
        let mut encoder = GeminiStreamEncoder::new();
        let frames = encoder.encode(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("he".into()),
            finish_reason: None,
        });
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0]["candidates"][0]["content"]["parts"][0]["text"],
            "he"
        );

        encoder.encode(StreamChunk::ToolCall {
            index: 0,
            id: "c1".into(),
            name: Some("f".into()),
            arguments: Some("{\"x\":".into()),
        });
        encoder.encode(StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("1}".into()),
        });
        let frames = encoder.encode(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text(String::new()),
            finish_reason: Some("tool_calls".into()),
        });
        assert_eq!(frames.len(), 1);
        let parts = frames[0]["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts[0]["functionCall"]["name"], "f");
        assert_eq!(parts[0]["functionCall"]["args"]["x"], 1);
        assert_eq!(frames[0]["candidates"][0]["finishReason"], "STOP");
    }

    #[test]
    fn encoder_emits_usage_once_and_finish_fallback() {
        let mut encoder = GeminiStreamEncoder::new();
        let frames = encoder.encode(StreamChunk::Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens: 7,
            cache_read_tokens: 1,
            cache_creation_tokens: 0,
            finish_reason: Some("stop".into()),
        });
        assert_eq!(frames.len(), 2, "终止帧 + usage 帧");
        assert_eq!(frames[0]["candidates"][0]["finishReason"], "STOP");
        assert_eq!(frames[1]["usageMetadata"]["promptTokenCount"], 3);
        assert_eq!(frames[1]["usageMetadata"]["cachedContentTokenCount"], 1);
        assert!(encoder.finish().is_empty(), "终止帧与 usage 不重复发送");
    }

    #[test]
    fn heartbeat_and_empty_text_produce_no_frames() {
        let mut encoder = GeminiStreamEncoder::new();
        assert!(encoder.encode(StreamChunk::Heartbeat).is_empty());
        assert!(encoder
            .encode(StreamChunk::Content {
                index: 0,
                delta: MessageContent::Text(String::new()),
                finish_reason: None,
            })
            .is_empty());
        assert_eq!(encoder.finish().len(), 1, "无 finish 时按 STOP 收尾");
    }

    #[test]
    fn wire_part_shape_matches_type() {
        let part = part_from(json!({"text": "x", "thought": true}));
        assert_eq!(part.text.as_deref(), Some("x"));
        assert!(part.thought);
    }
}
