// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! SSE（Server-Sent Events）编码器
//!
//! 提供符合 [SSE 规范](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)
//! 的编码函数，用于流式响应的 chunk 包装。

/// 编码普通 SSE 事件。
///
/// - `data` 为负载数据（必填）
/// - `event` 为可选事件类型（如 `chunk`、`error`）
///
/// 输出格式：
/// - 有 event：`event: <e>` + LF + `data: <data>` + LF + LF
/// - 无 event：`data: <data>` + LF + LF
pub fn encode_sse_event(data: &str, event: Option<&str>) -> String {
    match event {
        Some(e) => format!("event: {e}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// 编码流结束标记 `[DONE]`。
pub fn encode_sse_done() -> &'static str {
    "data: [DONE]\n\n"
}

/// 编码错误事件。
///
/// 输出格式：`event: error` + LF + `data: <msg>` + LF + LF
pub fn encode_sse_error(msg: &str) -> String {
    format!("event: error\ndata: {msg}\n\n")
}
