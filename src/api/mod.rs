// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! API 层模块
//!
//! 提供 REST 端点定义、状态共享和路由构建。

pub mod openai;
pub mod anthropic;
pub mod health;
pub mod admin;
pub mod admin_sdforge;
pub mod openai_sdforge;
pub mod anthropic_sdforge;
pub mod global_state;
pub mod routing_hints;
pub mod state;
pub mod router_builder;

pub use state::ApiState;
pub use router_builder::create_router;
pub use routing_hints::RoutingHints;

pub use openai::{chat_completions, embeddings};
pub use anthropic::messages;
pub use admin::{
    get_config, update_config,
    get_models, update_model,
};
pub use admin_sdforge::{
    logs, search_logs, token_stats,
    vendors, vendor_status, reset_vendor,
    get_vendors,
};
pub use openai_sdforge::{chat_completions as chat_completions_sd, embeddings as embeddings_sd};
pub use anthropic_sdforge::{messages as messages_sd};
pub use global_state::{init_global_state, get_global_state};
