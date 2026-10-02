// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::NexusFlowError;

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
pub use unified::{
    Choice, ContentBlock, EmbeddingData, EmbeddingUsage, ImageSource, Message, MessageContent,
    Role, StreamChunk, TokenUsage, Tool, ToolCall, ToolCallFunction, ToolChoice,
    ToolChoiceFunction, ToolFunction, ToolParameterProperty, ToolParameters, ToolResult,
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
    fn to_unified(&self, body: serde_json::Value) -> Result<UnifiedRequest, NexusFlowError>;
    /// 将 UnifiedRequest 转换为外部 API 请求格式 (如 Anthropic → OpenAI)
    fn request_to_external(
        &self,
        request: UnifiedRequest,
    ) -> Result<serde_json::Value, NexusFlowError>;
    fn response_to_external(
        &self,
        response: UnifiedResponse,
    ) -> Result<serde_json::Value, NexusFlowError>;
    fn parse_response(
        &self,
        response: serde_json::Value,
    ) -> Result<UnifiedResponse, NexusFlowError>;
    fn stream_chunk_to_external(
        &self,
        chunk: StreamChunk,
    ) -> Result<serde_json::Value, NexusFlowError>;

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
    fn passthrough_request(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, NexusFlowError> {
        Ok(body)
    }

    /// 透传响应体（零拷贝优化）
    fn passthrough_response(
        &self,
        response: serde_json::Value,
    ) -> Result<serde_json::Value, NexusFlowError> {
        Ok(response)
    }
}
