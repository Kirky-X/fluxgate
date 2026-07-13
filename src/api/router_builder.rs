// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use axum::{
    extract::{Request, State},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use limiteron::{Decision, Governor, RequestContext};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use super::state::ApiState;
use super::{admin, anthropic, openai};

/// 限流中间件 — 基于 limiteron::Governor::check
///
/// 健康检查路径放行；其余请求根据 Governor 决策返回 429/403 或放行。
async fn rate_limit_middleware(
    State(governor): State<Arc<Governor>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    // 健康检查放行
    if matches!(path.as_str(), "/health" | "/healthz" | "/ready") {
        return next.run(request).await;
    }

    let context = RequestContext::new()
        .with_path(&path)
        .with_method(request.method().as_str());

    match governor.check(&context).await {
        Ok(Decision::Allowed(_)) | Err(_) => next.run(request).await,
        Ok(Decision::Rejected(_)) => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded",
        )
            .into_response(),
        Ok(Decision::Banned(_)) => {
            (axum::http::StatusCode::FORBIDDEN, "Access denied").into_response()
        }
    }
}

/// 创建 axum 路由，集成 limiteron 限流中间件
pub async fn create_router(state: Arc<ApiState>) -> Router {
    let governor = Arc::new(Governor::new().await);

    Router::new()
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/embeddings", post(openai::embeddings))
        .route("/v1/messages", post(anthropic::messages))
        .route("/admin/logs", get(admin::logs))
        .route("/admin/logs/search", get(admin::search_logs))
        .route("/admin/stats/tokens", get(admin::token_stats))
        .route("/admin/metrics", get(admin::metrics))
        .route("/admin/vendors", get(admin::vendors))
        .route("/admin/vendors/:id/status", get(admin::vendor_status))
        .route("/admin/vendors/:id/reset", post(admin::reset_vendor))
        .route(
            "/admin/config",
            get(admin::get_config).post(admin::update_config),
        )
        .route("/admin/models", get(admin::get_models))
        .route("/admin/models/:id", post(admin::update_model))
        .with_state(state)
        .layer(from_fn_with_state(governor, rate_limit_middleware))
        .layer(TraceLayer::new_for_http())
}
