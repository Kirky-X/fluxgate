// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SSE 流式解码 — 使用 eventsource-stream 解析网关 SSE 事件。
//!
//! 解码面对齐网关出站真实事件形状（`stream_chunk_to_external` 产物，
//! 后携带工具调用片段）
//! - 内容帧：`{id:"chunk-{index}",choices:[{index,delta:{content},finish_reason}]}`；
//! - 工具帧：`delta.tool_calls:[{index,type,id?,function:{name?,arguments?}}]`
//!   （id/name 仅首片段携带，arguments 为增量分片——对齐 IR
//!   `StreamChunk::ToolCall` 片段语义）；
//! - usage 尾帧：`{id:"usage",choices:[],usage:{prompt_tokens,...,
//!   prompt_tokens_details:{cached_tokens}}}` → `ChatChunk.usage`；
//! - **错误帧**：`event: error` 或顶层 `{"error":…}` 负载 → `Err(Stream)`
//!   （流内事件无 HTTP status，消息取 `error.message`；不得静默结束）。

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::{BoxStream, StreamExt};
use futures::Stream;
use serde::Deserialize;

use crate::sdk::chat::{
    ChatChoice, ChatFinishReason, ChatResponse, ChatRole, ChatToolCall, ChatToolFunction, ChatUsage,
};
use crate::sdk::error::FluxgateSdkError;

/// 流式工具调用片段 — 对齐网关出站 `delta.tool_calls[]` 元素
///（index 为多片段交错聚合的槽位；id/name 仅首片段携带）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatToolCallDelta {
    /// 交错槽位（同一轮多工具调用按 index 区分）
    #[serde(default)]
    pub index: Option<u32>,
    /// 恒 `"function"`（字段名为 Rust 侧命名，线上键经 `rename = "type"` 映射）
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub tool_call_type: Option<String>,
    /// 首片段携带（续片段缺省）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<ChatToolCallDeltaFunction>,
}

/// 工具调用片段函数体（name 首片段携带；arguments 增量分片）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatToolCallDeltaFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// 流式 chunk 的 delta 增量。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ChatRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 思考内容增量（非标字段；Anthropic thinking 经网关流转）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// 采样 logprobs（网关原样透传的 `delta.logprobs`，不做二次结构化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCallDelta>>,
}

/// 单个流式 chunk — delta 与 usage 互斥（usage 尾帧无 delta）。
#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub id: String,
    pub delta: Option<ChatDelta>,
    pub finish_reason: Option<ChatFinishReason>,
    /// usage 尾帧（`stream_options.include_usage` 形态，choices 为空）
    pub usage: Option<ChatUsage>,
}

/// Chat 流式响应。包装 SSE 事件流，解码为 ChatChunk。
pub struct ChatStream {
    inner: BoxStream<'static, Result<ChatChunk, FluxgateSdkError>>,
}

/// 网关出站流式帧 JSON 解析辅助结构（choices 可空——usage 尾帧）。
#[derive(Deserialize)]
pub(crate) struct StreamFrame {
    #[serde(default)]
    id: String,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
    /// 顶层错误负载（错误帧）：非 null 即 `Err(Stream)`。
    #[serde(default)]
    error: Option<serde_json::Value>,
}

/// 从错误帧负载提取可读消息：`{"error":{message}}` / `{"error":"…"}` /
/// `{"message":"…"}` → 文本；否则原始 data（截断 512 字符，防超长 HTML）。
pub(crate) fn stream_error_message(data: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
        let error = value.get("error").unwrap_or(&value);
        if let Some(message) = error.get("message").and_then(|v| v.as_str()) {
            return message
                .chars()
                .take(crate::sdk::error::MAX_ERROR_MESSAGE_CHARS)
                .collect();
        }
        if let Some(message) = error.as_str() {
            return message
                .chars()
                .take(crate::sdk::error::MAX_ERROR_MESSAGE_CHARS)
                .collect();
        }
        if let Some(message) = value.get("message").and_then(|v| v.as_str()) {
            return message
                .chars()
                .take(crate::sdk::error::MAX_ERROR_MESSAGE_CHARS)
                .collect();
        }
    }
    let raw = data.trim();
    if raw.is_empty() {
        "stream error".to_string()
    } else {
        raw.chars()
            .take(crate::sdk::error::MAX_ERROR_MESSAGE_CHARS)
            .collect()
    }
}

#[derive(Deserialize)]
pub(crate) struct StreamChoice {
    #[serde(default)]
    delta: Option<ChatDelta>,
    #[serde(default)]
    finish_reason: Option<ChatFinishReason>,
}

impl ChatStream {
    /// 从 HTTP 字节流构建 ChatStream。
    ///
    /// 使用 eventsource-stream 解析 SSE 事件，过滤 `data: [DONE]` 终止标记，
    /// 其余事件反序列化为 ChatChunk。
    pub fn from_bytes_stream<S>(bytes: S) -> Self
    where
        S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    {
        use eventsource_stream::Eventsource;

        let event_stream = bytes.eventsource();

        let chunk_stream = event_stream.filter_map(|res| async move {
            match res {
                Ok(event) => {
                    if event.data == "[DONE]" {
                        return None;
                    }
                    // `event: error` 帧：不按流式内容解析，直接上抛（对齐
                    // 官方 SDK `error` 事件语义）
                    if event.event == "error" {
                        return Some(Err(FluxgateSdkError::Stream {
                            message: stream_error_message(&event.data),
                        }));
                    }
                    match serde_json::from_str::<StreamFrame>(&event.data) {
                        Ok(parsed) => {
                            // 顶层 `{"error":…}` 负载（部分上游不带
                            // event 名）：不得静默结束，上抛 Stream 错误
                            if parsed.error.as_ref().is_some_and(|value| !value.is_null()) {
                                return Some(Err(FluxgateSdkError::Stream {
                                    message: stream_error_message(&event.data),
                                }));
                            }
                            // 网关心跳帧（StreamChunk::Heartbeat →
                            // `{":":""}` 注释帧，无 id/choices/usage）不产出
                            // 空 ChatChunk
                            if parsed.id.is_empty()
                                && parsed.choices.is_empty()
                                && parsed.usage.is_none()
                            {
                                return None;
                            }
                            let choice = parsed.choices.into_iter().next();
                            let (delta, finish_reason) = match choice {
                                Some(c) => (c.delta, c.finish_reason),
                                None => (None, None),
                            };
                            Some(Ok(ChatChunk {
                                id: parsed.id,
                                delta,
                                finish_reason,
                                usage: parsed.usage,
                            }))
                        }
                        Err(e) => Some(Err(FluxgateSdkError::Stream {
                            message: format!("JSON parse error: {e}"),
                        })),
                    }
                }
                Err(e) => Some(Err(FluxgateSdkError::Stream {
                    message: format!("SSE decode error: {e}"),
                })),
            }
        });

        ChatStream {
            inner: chunk_stream.boxed(),
        }
    }

    /// 聚合流式碎片为完整响应（对齐官方 SDK `finalChatCompletion()`）：
    /// `content`/`reasoning_content` 按到达序拼接；`tool_calls` 按 `index`
    /// 槽位累积（id/name 取首个非空，arguments 拼接）；`finish_reason`/
    /// `usage` 取最后一个携带值；`id` 取首个非空。`model` 流式帧不携带
    /// 有效值（网关产出 `"unknown"`），产物中留空。
    pub async fn finalize(mut self) -> Result<ChatResponse, FluxgateSdkError> {
        let mut id = String::new();
        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ChatToolCall> = Vec::new();
        let mut finish_reason: Option<ChatFinishReason> = None;
        let mut usage: Option<ChatUsage> = None;

        while let Some(chunk) = self.next().await {
            let chunk = chunk?;
            if id.is_empty() && !chunk.id.is_empty() {
                id = chunk.id;
            }
            if let Some(value) = chunk.usage {
                usage = Some(value);
            }
            if let Some(value) = chunk.finish_reason {
                finish_reason = Some(value);
            }
            if let Some(delta) = chunk.delta {
                if let Some(part) = delta.content {
                    content.push_str(&part);
                }
                if let Some(part) = delta.reasoning_content {
                    reasoning_content.push_str(&part);
                }
                if let Some(fragments) = delta.tool_calls {
                    for fragment in fragments {
                        merge_tool_call_fragment(&mut tool_calls, fragment);
                    }
                }
            }
        }

        let finish_reason = finish_reason.unwrap_or_else(|| ChatFinishReason::Other(String::new()));
        let choice = ChatChoice {
            index: 0,
            content: content.clone(),
            reasoning_content: reasoning_content.clone(),
            tool_calls: tool_calls.clone(),
            finish_reason: finish_reason.clone(),
        };
        Ok(ChatResponse {
            id,
            model: String::new(),
            choices: vec![choice],
            content,
            reasoning_content,
            tool_calls,
            finish_reason,
            usage: usage.unwrap_or(ChatUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cache_read_tokens: 0,
            }),
        })
    }
}

/// wire 端 `index` 不可信（上游可为任意 u32），密集 Vec 槽位必须设上限——
/// 单帧超大 index 否则会触发巨量分配；超限片段丢弃并 debug 记录。
const MAX_TOOL_CALL_SLOTS: u32 = 128;

/// 将流式工具片段按 `index` 槽位并入累积表：id/name 取首个非空，
/// arguments 拼接（对齐官方 SDK 的 tool_call 分片合并语义）。
fn merge_tool_call_fragment(calls: &mut Vec<ChatToolCall>, fragment: ChatToolCallDelta) {
    let slot = fragment.index.unwrap_or(0);
    if slot >= MAX_TOOL_CALL_SLOTS {
        tracing::debug!(
            index = slot,
            "drop tool_call fragment: index exceeds slot cap"
        );
        return;
    }
    let slot = slot as usize;
    if calls.len() <= slot {
        calls.resize_with(slot + 1, || ChatToolCall {
            id: String::new(),
            function: ChatToolFunction {
                name: String::new(),
                arguments: String::new(),
            },
        });
    }
    let call = &mut calls[slot];
    if call.id.is_empty() {
        if let Some(id) = fragment.id.filter(|id| !id.is_empty()) {
            call.id = id;
        }
    }
    if let Some(function) = fragment.function {
        if call.function.name.is_empty() {
            if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                call.function.name = name;
            }
        }
        if let Some(arguments) = function.arguments {
            call.function.arguments.push_str(&arguments);
        }
    }
}

impl Stream for ChatStream {
    type Item = Result<ChatChunk, FluxgateSdkError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
