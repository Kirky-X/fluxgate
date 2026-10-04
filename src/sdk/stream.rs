//! SSE 流式解码 — 使用 eventsource-stream 解析网关 SSE 事件。
//!
//! T008：解码面对齐网关出站真实事件形状（`stream_chunk_to_external` 产物，
//! T004 后携带工具调用片段）：
//! - 内容帧：`{id:"chunk-{index}",choices:[{index,delta:{content},finish_reason}]}`；
//! - 工具帧：`delta.tool_calls:[{index,type,id?,function:{name?,arguments?}}]`
//!   （id/name 仅首片段携带，arguments 为增量分片——对齐 IR
//!   `StreamChunk::ToolCall` 片段语义）；
//! - usage 尾帧：`{id:"usage",choices:[],usage:{prompt_tokens,...,
//!   prompt_tokens_details:{cached_tokens}}}` → `ChatChunk.usage`。

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::{BoxStream, StreamExt};
use futures::Stream;
use serde::Deserialize;

use crate::sdk::chat::{ChatFinishReason, ChatRole, ChatUsage};
use crate::sdk::error::NexusFlowSdkError;

/// 流式工具调用片段 — 对齐网关出站 `delta.tool_calls[]` 元素
///（index 为多片段交错聚合的槽位；id/name 仅首片段携带）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatToolCallDelta {
    /// 交错槽位（同一轮多工具调用按 index 区分）
    #[serde(default)]
    pub index: Option<u32>,
    /// 恒 `"function"`（网关产出；审查 LOW：字段名与线上键 `type` 显式映射）
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
    inner: BoxStream<'static, Result<ChatChunk, NexusFlowSdkError>>,
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
                    match serde_json::from_str::<StreamFrame>(&event.data) {
                        Ok(parsed) => {
                            // 审查 LOW：网关心跳帧（StreamChunk::Heartbeat →
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
                        Err(e) => Some(Err(NexusFlowSdkError::Stream {
                            message: format!("JSON parse error: {e}"),
                        })),
                    }
                }
                Err(e) => Some(Err(NexusFlowSdkError::Stream {
                    message: format!("SSE decode error: {e}"),
                })),
            }
        });

        ChatStream {
            inner: chunk_stream.boxed(),
        }
    }
}

impl Stream for ChatStream {
    type Item = Result<ChatChunk, NexusFlowSdkError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
