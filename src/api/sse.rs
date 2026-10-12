// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! SSE 编码器与增量分帧器：实源头位于 [`crate::protocol::sse_frame`]，
//! 本模块 re-export 保持既有引用路径

pub use crate::protocol::sse_frame::{
    encode_gateway_stream_error, encode_sse_done, encode_sse_error, encode_sse_event,
    reframe_passthrough_stream, reframe_passthrough_stream_with_usage, ReframeObservation,
    SseFrame, SseFrameParser,
};

/// 流式路径内部错误 → 客户端 `event: error` 帧的统一出口：出站 message
/// 固定为无参安全 i18n 文本（内部错误详情只进 tracing，防拓扑外泄）
pub fn stream_error_frame(e: &impl std::fmt::Display) -> String {
    let safe_text = crate::i18n::t("protocol-stream-error-frame");
    tracing::warn!(error = %e, "{safe_text}");
    encode_gateway_stream_error(&safe_text)
}

#[cfg(test)]
mod tests {
    use super::stream_error_frame;

    #[test]
    fn stream_error_frame_is_json_envelope_without_internal_detail() {
        // reqwest Display 形态：携带上游 URL 的内部错误
        let internal =
            "error sending request for url (http://internal-vendor:8001/v1/chat/completions)";
        let frame = stream_error_frame(&internal);
        assert!(
            frame.starts_with("event: error\ndata: "),
            "帧格式不变: {frame:?}"
        );
        assert!(frame.ends_with("\n\n"), "双换行终止: {frame:?}");
        let payload = &frame["event: error\ndata: ".len()..frame.len() - 2];
        let value: serde_json::Value =
            serde_json::from_str(payload).expect("data 必须是 {\"error\":{...}} JSON 信封");
        assert_eq!(
            value["error"]["type"], "upstream_stream_interrupted",
            "错误码为稳定字面量: {payload}"
        );
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "message 为 i18n 文案: {payload}"
        );
        assert!(
            !frame.contains("internal-vendor"),
            "不得泄露内部拓扑: {frame:?}"
        );
        assert!(!frame.contains(internal), "不得出站内部错误原文: {frame:?}");
    }
}
