// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::GateError;

pub mod anthropic_converter;
pub mod embedding_converter;
pub mod openai_converter;
pub mod sse_frame;
pub mod unified;

pub use anthropic_converter::{AnthropicConverter, AnthropicStreamEncoder};
pub use embedding_converter::embedding_request_to_openai;
pub use openai_converter::OpenAIConverter;
pub use sse_frame::{
    encode_sse_done, encode_sse_error, encode_sse_event, SseFrame, SseFrameParser,
};
pub(crate) use unified::{absorb_vendor_extensions, merge_vendor_extensions};
pub use unified::{
    Choice, ContentBlock, EffortLevel, EmbeddingData, EmbeddingUsage, FinishReason, ImageSource,
    Message, MessageContent, Role, SamplingParams, StreamChunk, ThinkingConfig, TokenUsage, Tool,
    ToolCall, ToolCallFunction, ToolChoice, ToolChoiceFunction, ToolFunction, ToolResult,
    UnifiedEmbeddingResponse, UnifiedRequest, UnifiedResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

#[async_trait::async_trait]
pub trait ProtocolConverter: Send + Sync {
    fn input_protocol(&self) -> Protocol;
    fn output_protocol(&self) -> Protocol;
    fn to_unified(&self, body: serde_json::Value) -> Result<UnifiedRequest, GateError>;
    /// 将 UnifiedRequest 转换为外部 API 请求格式 (如 Anthropic → OpenAI)
    fn request_to_external(&self, request: UnifiedRequest) -> Result<serde_json::Value, GateError>;
    fn response_to_external(
        &self,
        response: UnifiedResponse,
    ) -> Result<serde_json::Value, GateError>;
    fn parse_response(&self, response: serde_json::Value) -> Result<UnifiedResponse, GateError>;
    fn stream_chunk_to_external(&self, chunk: StreamChunk) -> Result<serde_json::Value, GateError>;

    /// 上游 SSE 帧 `data` 负载 → IR StreamChunk（recId=3 阶段二反向解析）。
    ///
    /// 与 [`ProtocolConverter::stream_chunk_to_external`] 互为逆操作：
    /// 前者把上游协议的流式事件解析进 IR，后者把 IR 编码为目标协议。
    /// 无法映射到 IR 的帧（OpenAI `[DONE]` 终止标记、Anthropic
    /// message_start/content_block_start 等生命周期事件）返回 `None`，
    /// 由调用方跳过。
    fn parse_stream_frame(&self, _data: &str) -> Option<StreamChunk> {
        None
    }

    /// 检查是否可以透传（同格式直接转发）
    fn can_passthrough(&self, output_protocol: Protocol) -> bool {
        self.output_protocol() == output_protocol
    }

    /// 透传请求体（零拷贝优化）
    /// 当入站协议与出站协议相同时，直接返回原始请求体，避免序列化/反序列化开销
    fn passthrough_request(&self, body: serde_json::Value) -> Result<serde_json::Value, GateError> {
        Ok(body)
    }

    /// 透传响应体（零拷贝优化）
    fn passthrough_response(
        &self,
        response: serde_json::Value,
    ) -> Result<serde_json::Value, GateError> {
        Ok(response)
    }
}

#[cfg(test)]
mod default_impl_tests {
    use super::*;

    /// 未覆写 `parse_stream_frame` 的最小实现：Anthropic/OpenAI 两个具体
    /// 转换器都覆写了该方法，trait 默认体只能经此类触达。
    struct NoopConverter;

    #[async_trait::async_trait]
    impl ProtocolConverter for NoopConverter {
        fn input_protocol(&self) -> Protocol {
            Protocol::OpenAI
        }

        fn output_protocol(&self) -> Protocol {
            Protocol::OpenAI
        }

        fn to_unified(&self, _body: serde_json::Value) -> Result<UnifiedRequest, GateError> {
            Err(GateError::Validation {
                field: "unsupported".into(),
            })
        }

        fn request_to_external(
            &self,
            _request: UnifiedRequest,
        ) -> Result<serde_json::Value, GateError> {
            Err(GateError::Validation {
                field: "unsupported".into(),
            })
        }

        fn response_to_external(
            &self,
            _response: UnifiedResponse,
        ) -> Result<serde_json::Value, GateError> {
            Err(GateError::Validation {
                field: "unsupported".into(),
            })
        }

        fn parse_response(
            &self,
            _response: serde_json::Value,
        ) -> Result<UnifiedResponse, GateError> {
            Err(GateError::Validation {
                field: "unsupported".into(),
            })
        }

        fn stream_chunk_to_external(
            &self,
            _chunk: StreamChunk,
        ) -> Result<serde_json::Value, GateError> {
            Err(GateError::Validation {
                field: "unsupported".into(),
            })
        }
    }

    #[test]
    fn default_parse_stream_frame_returns_none() {
        // 默认契约：任何输入都不映射 IR，由调用方跳过
        assert!(NoopConverter.parse_stream_frame("anything").is_none());
        assert!(NoopConverter.parse_stream_frame("").is_none());
        assert!(NoopConverter.parse_stream_frame("[DONE]").is_none());
    }

    /// 最小实现只依赖 trait 默认方法即可成立：未实现的必需方法必须显式
    /// 报 Validation（而非 panic 或静默成功），透传判定与 output_protocol 一致。
    #[test]
    fn minimal_converter_unsupported_ops_return_validation() {
        let c = NoopConverter;
        assert!(!c.can_passthrough(Protocol::Anthropic));
        assert!(c.can_passthrough(Protocol::OpenAI));

        let req = UnifiedRequest {
            model: "m".into(),
            messages: vec![],
            ..Default::default()
        };
        let resp = UnifiedResponse {
            id: "r".into(),
            object: String::new(),
            created: 0,
            model: "m".into(),
            choices: vec![],
            usage: Default::default(),
        };

        let is_validation = |e: GateError| matches!(e, GateError::Validation { .. });
        assert!(is_validation(
            c.to_unified(serde_json::json!({})).unwrap_err()
        ));
        assert!(is_validation(c.request_to_external(req).unwrap_err()));
        assert!(is_validation(
            c.parse_response(serde_json::json!({})).unwrap_err()
        ));
        assert!(is_validation(c.response_to_external(resp).unwrap_err()));
        assert!(is_validation(
            c.stream_chunk_to_external(StreamChunk::Heartbeat)
                .unwrap_err()
        ));
    }
}
