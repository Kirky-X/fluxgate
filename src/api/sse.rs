// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
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

/// 流式路径内部错误 → 客户端 `event: error` 帧的统一出口。
///
/// 内部错误详情（reqwest 等的 Display 可携带上游 vendor URL）只进
/// `tracing` 日志；出站固定为无参安全 i18n 文本，与 REST 错误响应的
/// 固定码纪律对齐——防止网关后端拓扑经错误原文外泄。
pub fn stream_error_frame(e: &impl std::fmt::Display) -> String {
    let safe_text = crate::i18n::t("vendor-upstream-stream-interrupted");
    tracing::warn!(error = %e, "{safe_text}");
    encode_sse_error(&safe_text)
}

#[cfg(test)]
mod tests {
    use super::stream_error_frame;

    #[test]
    fn stream_error_frame_hides_internal_detail() {
        // reqwest Display 形态：携带上游 URL 的内部错误
        let internal =
            "error sending request for url (http://internal-vendor:8001/v1/chat/completions)";
        let frame = stream_error_frame(&internal);
        assert!(
            frame.starts_with("event: error\ndata: "),
            "帧格式不变: {frame:?}"
        );
        assert!(frame.ends_with("\n\n"), "双换行终止: {frame:?}");
        assert!(
            !frame.contains("internal-vendor"),
            "不得泄露内部拓扑: {frame:?}"
        );
        assert!(!frame.contains(internal), "不得出站内部错误原文: {frame:?}");
    }
}
