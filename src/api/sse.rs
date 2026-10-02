// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! SSE（Server-Sent Events）编码器与增量分帧器
//!
//! 提供符合 [SSE 规范](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)
//! 的编码函数，用于流式响应的 chunk 包装；以及上游 SSE 字节流的增量分帧器
//! （recId=3 阶段一）。
//!
//! 分帧器为手写：已核实 sdforge::streaming 仅提供出站编码
//! （stream_to_sse / create_stream_channel，sdforge
//! src/streaming/streaming_impl.rs:54-98），无上游 SSE 字节流解析能力。
//!
//! 实源头位于 [`crate::protocol::sse_frame`]（`vendor-management` 等
//! feature 不含 `web-server`，vendor 层分帧需要该能力）；本模块
//! re-export 保持既有引用路径。

pub use crate::protocol::sse_frame::{
    encode_sse_done, encode_sse_error, encode_sse_event, reframe_passthrough_stream, SseFrame,
    SseFrameParser,
};
