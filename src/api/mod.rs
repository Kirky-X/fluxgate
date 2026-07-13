// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! API 层模块
//!
//! 提供 REST 端点定义、状态共享和路由构建。

pub mod admin;
pub mod anthropic;
pub mod global_state;
pub mod health;
pub mod openai;
pub mod router_builder;
pub mod routing_hints;
pub mod sse;
pub mod state;

pub use router_builder::create_router;
pub use routing_hints::RoutingHints;
pub use state::ApiState;

pub use admin::{get_config, get_models, update_config, update_model};
pub use anthropic::messages;
pub use global_state::{get_global_state, init_global_state};
pub use openai::{chat_completions, embeddings};
