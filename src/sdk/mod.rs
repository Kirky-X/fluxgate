// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Fluxgate Rust SDK
//!
//! 通过 Rust 接口与 Fluxgate 网关交互（HTTP 客户端 SDK），
//! 支持 chat / stream / embed / rerank / responses / admin。

pub mod admin;
pub mod chat;
pub mod client;
pub mod config;
pub mod embed;
pub mod error;
pub mod rerank;
pub mod responses;
pub mod retry;
pub mod stream;
pub(crate) mod transport;

pub use admin::{
    HealthStatus, KeyPoolInfo, ModelCatalogInfo, ModelCatalogUpsert, VendorStatusInfo,
};
pub use chat::{
    AllowedToolsBody, AllowedToolsMode, ChatChoice, ChatContent, ChatContentPart, ChatFinishReason,
    ChatMessage, ChatRequest, ChatResponse, ChatRole, ChatTool, ChatToolCall, ChatToolChoice,
    ChatToolChoiceFunction, ChatToolDefinition, ChatToolFunction, ChatUsage, ImageUrlBody,
    StandardFinishReason,
};
pub use client::{FluxgateClient, FluxgateClientBuilder};
pub use config::RequestConfig;
pub use embed::{
    EmbedData, EmbedEncodingFormat, EmbedInput, EmbedRequest, EmbedResponse, EmbedUsage,
    EmbeddingValue,
};
pub use error::{ApiError, ApiErrorKind, FluxgateSdkError};
pub use rerank::{RerankDocument, RerankMeta, RerankRequest, RerankResponse, RerankResult};
pub use responses::{
    ResponsesContent, ResponsesError, ResponsesInputTokensDetails, ResponsesInputTokensRequest,
    ResponsesInputTokensResponse, ResponsesOutputItem, ResponsesRequest, ResponsesResponse,
    ResponsesStream, ResponsesStreamEvent, ResponsesUsage,
};
pub use stream::{ChatChunk, ChatDelta, ChatStream, ChatToolCallDelta, ChatToolCallDeltaFunction};

#[cfg(test)]
mod tests;
