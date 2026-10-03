//! NexusFlow Rust SDK
//!
//! 通过 Rust 接口与 NexusFlow 网关交互（HTTP 客户端 SDK），
//! 支持 chat / stream / embed / admin。

pub mod admin;
pub mod chat;
pub mod client;
pub mod config;
pub mod embed;
pub mod error;
pub mod stream;

pub use chat::{ChatMessage, ChatRequest, ChatResponse, ChatRole};
pub use client::{NexusFlowClient, NexusFlowClientBuilder};
pub use config::RequestConfig;
pub use embed::{EmbedRequest, EmbedResponse};
pub use error::NexusFlowSdkError;
pub use stream::ChatStream;

#[cfg(test)]
mod tests;
