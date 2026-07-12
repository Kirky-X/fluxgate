// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::NexusFlowError;

pub mod anthropic_converter;
pub mod embedding_converter;
pub mod openai_converter;
pub mod unified;

pub use anthropic_converter::AnthropicConverter;
#[allow(dead_code)]
pub use embedding_converter::{AnthropicEmbeddingConverter, OpenAIEmbeddingConverter};
pub use openai_converter::OpenAIConverter;
pub use unified::{
    MessageContent, Role, StreamChunk, Tool, ToolCall, ToolCallFunction, ToolChoice,
    ToolChoiceFunction, ToolFunction, ToolParameters, ToolParameterProperty, ToolResult,
    UnifiedRequest, UnifiedResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

#[async_trait::async_trait]
pub trait ProtocolConverter: Send + Sync {
    #[allow(dead_code)]
    fn input_protocol(&self) -> Protocol;
    #[allow(dead_code)]
    fn output_protocol(&self) -> Protocol;
    fn to_unified(&self, body: serde_json::Value) -> Result<UnifiedRequest, NexusFlowError>;
    /// 将 UnifiedRequest 转换为外部 API 请求格式 (如 Anthropic → OpenAI)
    fn request_to_external(&self, request: UnifiedRequest)
        -> Result<serde_json::Value, NexusFlowError>;
    fn from_unified(&self, response: UnifiedResponse) -> Result<serde_json::Value, NexusFlowError>;
    fn parse_response(&self, response: serde_json::Value) -> Result<UnifiedResponse, NexusFlowError>;
    #[allow(dead_code)]
    fn stream_chunk_to_external(&self, chunk: StreamChunk)
        -> Result<serde_json::Value, NexusFlowError>;

    /// 检查是否可以透传（同格式直接转发）
    fn can_passthrough(&self, output_protocol: Protocol) -> bool {
        self.output_protocol() == output_protocol
    }

    /// 透传请求体（零拷贝优化）
    /// 当入站协议与出站协议相同时，直接返回原始请求体，避免序列化/反序列化开销
    fn passthrough_request(&self, body: serde_json::Value) -> Result<serde_json::Value, NexusFlowError> {
        Ok(body)
    }

    /// 透传响应体（零拷贝优化）
    fn passthrough_response(&self, response: serde_json::Value) -> Result<serde_json::Value, NexusFlowError> {
        Ok(response)
    }
}

