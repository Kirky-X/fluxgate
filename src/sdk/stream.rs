//! SSE 流式解码 — 使用 eventsource-stream 解析上游 SSE 事件。

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::{BoxStream, StreamExt};
use futures::Stream;
use serde::Deserialize;

use crate::sdk::chat::ChatRole;
use crate::sdk::error::NexusFlowSdkError;

/// 流式 chunk 的 delta 增量。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatDelta {
    pub role: Option<ChatRole>,
    pub content: Option<String>,
}

/// 单个流式 chunk。
#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub id: String,
    pub delta: Option<ChatDelta>,
    pub finish_reason: Option<String>,
}

/// Chat 流式响应。包装 SSE 事件流，解码为 ChatChunk。
pub struct ChatStream {
    inner: BoxStream<'static, Result<ChatChunk, NexusFlowSdkError>>,
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
                    match serde_json::from_str::<StreamChunk>(&event.data) {
                        Ok(parsed) => {
                            let choice = parsed.choices.into_iter().next();
                            let (delta, finish_reason) = match choice {
                                Some(c) => (c.delta, c.finish_reason),
                                None => (None, None),
                            };
                            Some(Ok(ChatChunk {
                                id: parsed.id,
                                delta,
                                finish_reason,
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

/// OpenAI 流式响应 JSON 解析辅助结构。
#[derive(Deserialize)]
struct StreamChunk {
    id: String,
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: Option<ChatDelta>,
    finish_reason: Option<String>,
}
