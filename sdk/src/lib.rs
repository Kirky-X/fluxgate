//! NexusFlow Rust SDK
//!
//! 通过 Rust 接口与 NexusFlow 网关交互，支持 chat / stream / embed / admin / pipeline。
//! 骨架模块，各类型在后续 Phase 中逐步实现。

pub mod admin;
pub mod chat;
pub mod client;
pub mod config;
pub mod embed;
pub mod error;
pub mod pipeline;
pub mod stream;

pub use chat::{ChatMessage, ChatRequest, ChatResponse, ChatRole};
pub use client::NexusFlowClient;
pub use config::RequestConfig;
pub use embed::{EmbedRequest, EmbedResponse};
pub use error::NexusFlowSdkError;
pub use stream::ChatStream;
