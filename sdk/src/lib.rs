//! NexusFlow Rust SDK
//!
//! 通过 Rust 接口与 NexusFlow 网关交互，支持 chat / stream / embed / admin / pipeline。

pub mod admin;
pub mod chat;
#[cfg(feature = "client")]
pub mod client;
pub mod config;
pub mod embed;
pub mod error;
pub mod pipeline;
pub mod stream;

pub use chat::{ChatMessage, ChatRequest, ChatResponse, ChatRole};
#[cfg(feature = "client")]
pub use client::{NexusFlowClient, NexusFlowClientBuilder};
pub use config::RequestConfig;
pub use embed::{EmbedRequest, EmbedResponse};
pub use error::NexusFlowSdkError;
pub use stream::ChatStream;
