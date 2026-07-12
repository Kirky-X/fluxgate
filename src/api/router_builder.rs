// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::sync::Arc;
use axum::{routing::{post, get}, Router};
use tower_http::trace::TraceLayer;

use super::state::ApiState;
use super::{openai, anthropic, admin};

pub fn create_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/embeddings", post(openai::embeddings))
        .route("/v1/messages", post(anthropic::messages))
        .route("/admin/logs", get(admin::logs))
        .route("/admin/logs/search", get(admin::search_logs))
        .route("/admin/stats/tokens", get(admin::token_stats))
        .route("/admin/vendors", get(admin::vendors))
        .route("/admin/vendors/:id/status", get(admin::vendor_status))
        .route("/admin/vendors/:id/reset", post(admin::reset_vendor))
        .route("/admin/config", get(admin::get_config).post(admin::update_config))
        .route("/admin/models", get(admin::get_models))
        .route("/admin/models/:id", post(admin::update_model))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
