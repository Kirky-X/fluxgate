// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Fluxgate Rust SDK
//!
//! 通过 Rust 接口与 Fluxgate 网关交互（HTTP 客户端 SDK），
//! 支持 chat / stream / embed / admin。

pub mod admin;
pub mod chat;
pub mod client;
pub mod config;
pub mod embed;
pub mod error;
pub mod stream;

pub use admin::{
    HealthStatus, KeyPoolInfo, ModelCatalogInfo, ModelCatalogUpsert, VendorStatusInfo,
};
pub use chat::{
    ChatContent, ChatContentPart, ChatFinishReason, ChatMessage, ChatRequest, ChatResponse,
    ChatRole, ChatTool, ChatToolCall, ChatToolChoice, ChatToolChoiceFunction, ChatToolDefinition,
    ChatToolFunction, ChatUsage, ImageUrlBody, StandardFinishReason,
};
pub use client::{FluxgateClient, FluxgateClientBuilder};
pub use config::RequestConfig;
pub use embed::{EmbedRequest, EmbedResponse};
pub use error::FluxgateSdkError;
pub use stream::{ChatChunk, ChatDelta, ChatStream, ChatToolCallDelta, ChatToolCallDeltaFunction};

#[cfg(test)]
mod tests;
