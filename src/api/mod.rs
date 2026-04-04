use axum::{routing::{post, get}, Router};
use tower_http::trace::TraceLayer;
use std::sync::Arc;
use crate::{ConfigManager, VendorManager, Database};
use crate::protocol::ProtocolConverter;
use crate::router::Router as RouterTrait;
use reqwest::Client;

#[derive(Clone)]
pub struct ApiState {
    #[allow(dead_code)]
    pub config: Arc<ConfigManager>,
    pub vendor_manager: Arc<VendorManager>,
    pub router: Arc<RouterTrait>,
    #[allow(dead_code)]
    pub client: Client,
    pub openai_converter: Arc<dyn ProtocolConverter>,
    pub anthropic_converter: Arc<dyn ProtocolConverter>,
    pub database: Arc<Database>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("config", &"ConfigManager")
            .field("vendor_manager", &"VendorManager")
            .field("router", &"Router")
            .field("client", &"Client")
            .field("openai_converter", &"ProtocolConverter")
            .field("anthropic_converter", &"ProtocolConverter")
            .field("database", &"Database")
            .finish()
    }
}

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

pub mod openai;
pub mod anthropic;
pub mod health;
pub mod admin;
pub mod admin_sdforge;
pub mod openai_sdforge;
pub mod anthropic_sdforge;
pub mod global_state;
pub mod routing_hints;

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
