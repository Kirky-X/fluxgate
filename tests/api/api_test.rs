#![cfg(feature = "web-server")]

// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

#[path = "../common/mod.rs"]
mod common;

mod endpoints {
    use crate::common;
    use axum::{
        body::Body,
        http::Request,
        routing::{get, post},
        Router,
    };
    use fluxgate::{
        api::{chat_completions, messages},
        config::{Config, ConfigManager, DatabaseConfig, ModelConfig, ServerConfig},
        database::Database,
        protocol::{AnthropicConverter, OpenAIConverter},
        vendor::VendorManager,
        ApiState,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
    /// database ./test.db pool 5、双厂商 Interval{1h} 限流 + rps 10、
    /// 各自真实 api_key_env 与模型；anthropic 型厂商另覆盖 type_ 与模型。
    fn create_test_config() -> Config {
        let mut openai_main = common::base_vendor("openai_main", "");
        openai_main.name = "OpenAI Main".into();
        openai_main.api_key_env = "OPENAI_API_KEY".into();
        openai_main.rate_limit = fluxgate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        openai_main.concurrency.requests_per_second = 10;

        let mut anthropic_main = common::base_vendor("anthropic_main", "");
        anthropic_main.name = "Anthropic Main".into();
        anthropic_main.type_ = "anthropic".into();
        anthropic_main.api_key_env = "ANTHROPIC_API_KEY".into();
        anthropic_main.models = vec![ModelConfig {
            model_id: "claude-3-5-sonnet-20241022".to_string(),
            abstract_id: Some("claude-3.5-sonnet".to_string()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        anthropic_main.rate_limit = fluxgate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        anthropic_main.concurrency.requests_per_second = 10;

        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
                url: String::new(),
            },
            ..common::test_config(vec![openai_main, anthropic_main])
        }
    }

    async fn create_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config.clone()));

        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });

        let router = Arc::new(fluxgate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
        })
    }

    fn create_app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages))
            .route("/health", get(|| async { "OK" }))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = create_api_state().await;
        let app = create_app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_openai_chat_completions_missing_model() {
        let state = create_api_state().await;
        let app = create_app(state);

        let request_body = json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 400 for missing model
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn test_openai_chat_completions_invalid_json() {
        let state = create_api_state().await;
        let app = create_app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{invalid json}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 400 for invalid JSON
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn test_anthropic_messages_missing_model() {
        let state = create_api_state().await;
        let app = create_app(state);

        let request_body = json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 400 for missing model
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn test_openai_converter_to_unified() {
        let state = create_api_state().await;
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "What is Rust?"}
            ],
            "temperature": 0.7,
            "max_tokens": 100
        });

        let unified = state.openai_converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "gpt-4");
        assert_eq!(unified.messages.len(), 2);
        assert_eq!(unified.temperature, Some(0.7));
        assert_eq!(unified.max_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_anthropic_converter_to_unified() {
        let state = create_api_state().await;
        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "Hello, Claude!"}
            ],
            "max_tokens": 100
        });

        let unified = state.anthropic_converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "claude-3-5-sonnet-20241022");
        assert_eq!(unified.messages.len(), 1);
    }

    #[tokio::test]
    async fn test_unified_to_openai_response() {
        let state = create_api_state().await;

        let unified_response = fluxgate::protocol::unified::UnifiedResponse {
            id: "test-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "gpt-4".to_string(),
            choices: vec![fluxgate::protocol::unified::Choice {
                index: 0,
                message: Some(fluxgate::protocol::unified::Message {
                    role: fluxgate::protocol::unified::Role::Assistant,
                    content: fluxgate::protocol::unified::MessageContent::Text(
                        "Hello!".to_string(),
                    ),
                }),
                finish_reason: Some("stop".to_string()),
                finish: None,
                logprobs: None,
            }],
            usage: fluxgate::protocol::unified::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
            stop_sequence: None,
        };

        let external = state
            .openai_converter
            .response_to_external(unified_response);
        assert!(external.is_ok());
        let external = external.unwrap();

        assert!(external.is_object());
        assert_eq!(external["model"], "gpt-4");
    }

    #[tokio::test]
    async fn test_model_routing_openai() {
        let state = create_api_state().await;

        let decision = state.router.route("gpt-4").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        assert_eq!(decision.vendor.id, "openai_main");
        assert_eq!(decision.model.model_id, "gpt-4");
    }

    #[tokio::test]
    async fn test_model_routing_anthropic() {
        let state = create_api_state().await;

        let decision = state.router.route("claude-3.5-sonnet").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        assert_eq!(decision.vendor.id, "anthropic_main");
        assert_eq!(decision.model.model_id, "claude-3-5-sonnet-20241022");
    }

    #[tokio::test]
    async fn test_model_routing_not_found() {
        let state = create_api_state().await;

        let decision = state.router.route("nonexistent-model").await;
        assert!(decision.is_err());
    }

    #[tokio::test]
    async fn test_disabled_vendor_excluded() {
        let mut config = create_test_config();
        config.vendors[1].enabled = false; // Disable anthropic

        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Arc::new(fluxgate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        // claude-3.5-sonnet should fail because vendor is disabled
        let decision = router.route("claude-3.5-sonnet").await;
        assert!(decision.is_err());
    }

    #[tokio::test]
    async fn test_disabled_model_excluded() {
        let mut config = create_test_config();
        config.vendors[0].models[0].enabled = false; // Disable gpt-4

        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Arc::new(fluxgate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        // gpt-4 should fail because model is disabled
        let decision = router.route("gpt-4").await;
        assert!(decision.is_err());
    }
}

mod integration {
    use crate::common;
    use fluxgate::api::ApiState;
    use fluxgate::config::{Config, ConfigManager, DatabaseConfig, ServerConfig};
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
    use fluxgate::router::Router;
    use fluxgate::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
    /// database ./test.db pool 5、厂商 Interval{1h} 限流 + rps 10 +
    /// 真实 api_key_env OPENAI_API_KEY 与名称。
    fn create_test_config() -> Config {
        let mut v = common::base_vendor("openai_main", "");
        v.name = "OpenAI Main".into();
        v.api_key_env = "OPENAI_API_KEY".into();
        v.rate_limit = fluxgate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        v.concurrency.requests_per_second = 10;
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
                url: String::new(),
            },
            ..common::test_config(vec![v])
        }
    }

    async fn create_mock_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));

        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
        })
    }

    #[tokio::test]
    async fn test_openai_chat_completions_request_parsing() {
        let state = create_mock_api_state().await;

        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "What is Rust?"}
            ],
            "temperature": 0.7,
            "max_tokens": 100
        });

        let unified = state.openai_converter.to_unified(request.clone());
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "gpt-4");
        assert_eq!(unified.messages.len(), 2);
        assert_eq!(unified.temperature, Some(0.7));
        assert_eq!(unified.max_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_openai_response_formatting() {
        let converter = OpenAIConverter::new();

        let response = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1677858242,
            "model": "gpt-4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Rust is a systems programming language."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 10,
                "total_tokens": 25
            }
        });

        let result = converter.parse_response(response);
        assert!(result.is_ok());
        let parsed = result.unwrap();

        assert_eq!(parsed.id, "chatcmpl-abc123");
        assert_eq!(parsed.choices.len(), 1);
        assert_eq!(parsed.usage.prompt_tokens, 15);
        assert_eq!(parsed.usage.completion_tokens, 10);
    }

    #[tokio::test]
    async fn test_anthropic_messages_request_parsing() {
        let state = create_mock_api_state().await;

        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "Hello, Claude!"}
            ],
            "max_tokens": 100
        });

        let unified = state.anthropic_converter.to_unified(request.clone());
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "claude-3-5-sonnet-20241022");
        assert_eq!(unified.messages.len(), 1);
    }

    #[tokio::test]
    async fn test_anthropic_response_formatting() {
        let converter = AnthropicConverter::new();

        let response = json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                {
                    "type": "text",
                    "text": "Hello! I am Claude."
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 8
            }
        });

        let result = converter.parse_response(response);
        assert!(result.is_ok());
        let parsed = result.unwrap();

        assert_eq!(parsed.id, "msg_abc123");
        assert_eq!(parsed.choices.len(), 1);
    }

    #[tokio::test]
    async fn test_unified_response_to_openai_format() {
        let converter = OpenAIConverter::new();

        let unified_response = fluxgate::protocol::unified::UnifiedResponse {
            id: "test-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "gpt-4".to_string(),
            choices: vec![fluxgate::protocol::unified::Choice {
                index: 0,
                message: Some(fluxgate::protocol::unified::Message {
                    role: fluxgate::protocol::unified::Role::Assistant,
                    content: fluxgate::protocol::unified::MessageContent::Text(
                        "Hello!".to_string(),
                    ),
                }),
                finish_reason: Some("stop".to_string()),
                finish: None,
                logprobs: None,
            }],
            usage: fluxgate::protocol::unified::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
            stop_sequence: None,
        };

        let external = converter.response_to_external(unified_response);
        assert!(external.is_ok());
        let external = external.unwrap();

        assert!(external.is_object());
        assert_eq!(external["model"], "gpt-4");
    }

    #[tokio::test]
    async fn test_error_response_parsing() {
        let converter = OpenAIConverter::new();

        let error_response = json!({
            "error": {
                "message": "Invalid API key",
                "type": "authentication_error",
                "code": 401
            }
        });

        let result = converter.parse_response(error_response);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_messages_handling() {
        let converter = OpenAIConverter::new();

        let request = json!({
            "model": "gpt-4",
            "messages": []
        });

        let result = converter.to_unified(request);
        assert!(result.is_ok());
        let unified = result.unwrap();
        assert_eq!(unified.messages.len(), 0);
    }

    #[tokio::test]
    async fn test_stream_parameter_handling() {
        let converter = OpenAIConverter::new();

        let streaming_request = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Tell me a story"}],
            "stream": true
        });

        let result = converter.to_unified(streaming_request);
        assert!(result.is_ok());
        let unified = result.unwrap();
        assert!(unified.stream);
    }

    #[tokio::test]
    async fn test_partial_options_handling() {
        let converter = OpenAIConverter::new();

        let minimal_request = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = converter.to_unified(minimal_request);
        assert!(result.is_ok());
        let unified = result.unwrap();

        assert_eq!(unified.temperature, None);
        assert_eq!(unified.max_tokens, None);
        assert!(!unified.stream);
    }
}

mod auth {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use fluxgate::middleware::auth::api_auth_middleware;
    use fluxgate::middleware::ApiAuthState;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 构建带 auth 中间件的测试路由
    fn build_test_router(auth_state: Arc<ApiAuthState>) -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .route("/health", get(|| async { "OK" }))
            .route("/admin/panel", get(|| async { "admin" }))
            .route_layer(from_fn(move |req, next| {
                let state = auth_state.clone();
                async move { api_auth_middleware(req, next, state).await }
            }))
    }

    #[test]
    fn test_api_auth_state_disabled() {
        let state = ApiAuthState::new(false);
        assert!(state.validate_key("any-key"));
        assert!(state.validate_key(""));
    }

    #[test]
    fn test_api_auth_state_enabled_no_keys() {
        let state = ApiAuthState::new(true);
        assert!(!state.validate_key("some-key"));
    }

    #[test]
    fn test_api_auth_state_add_and_validate() {
        let mut state = ApiAuthState::new(true);
        state.add_api_key("secret-key".to_string());
        assert!(state.validate_key("secret-key"));
        assert!(!state.validate_key("wrong-key"));
    }

    #[tokio::test]
    async fn test_api_auth_middleware_disabled() {
        let auth_state = Arc::new(ApiAuthState::new(false));
        let app = build_test_router(auth_state);

        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_middleware_health_path() {
        let auth_state = Arc::new(ApiAuthState::new(true));
        let app = build_test_router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_middleware_admin_path() {
        let auth_state = Arc::new(ApiAuthState::new(true));
        let app = build_test_router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/panel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_middleware_missing_key() {
        let auth_state = Arc::new(ApiAuthState::new(true));
        let app = build_test_router(auth_state);

        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["message"], "Missing API key");
    }

    #[tokio::test]
    async fn test_api_auth_middleware_invalid_key() {
        let mut auth_state = ApiAuthState::new(true);
        auth_state.add_api_key("valid-key".to_string());
        let app = build_test_router(Arc::new(auth_state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("authorization", "Bearer wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["message"], "Invalid API key");
    }

    #[tokio::test]
    async fn test_api_auth_middleware_valid_key_bearer() {
        let mut auth_state = ApiAuthState::new(true);
        auth_state.add_api_key("secret-token".to_string());
        let app = build_test_router(Arc::new(auth_state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("authorization", "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_middleware_valid_key_x_api_key() {
        let mut auth_state = ApiAuthState::new(true);
        auth_state.add_api_key("api-key-123".to_string());
        let app = build_test_router(Arc::new(auth_state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("x-api-key", "api-key-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}

mod admin {
    use crate::common;
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::Json;
    use fluxgate::api::admin;
    use fluxgate::api::ApiState;
    use fluxgate::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ServerConfig, VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::error::{FluxgateError, GateError};
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
    use fluxgate::router::Router;
    use fluxgate::vendor::VendorManager;
    use std::sync::Arc;

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
    /// database ./test.db pool 5、厂商 Interval{1h} 限流 + rps 10 +
    /// 真实 api_key_env OPENAI_API_KEY 与名称。
    fn create_test_config() -> Config {
        let mut v = common::base_vendor("openai_main", "");
        v.name = "OpenAI Main".into();
        v.api_key_env = "OPENAI_API_KEY".into();
        v.rate_limit = fluxgate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        v.concurrency.requests_per_second = 10;
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
                url: String::new(),
            },
            ..common::test_config(vec![v])
        }
    }

    async fn create_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        // 初始化厂商实例，使 get_all_status() 能返回数据
        vendor_manager.initialize().await.unwrap();

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });

        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));
        let openai_converter: Arc<dyn ProtocolConverter> = Arc::new(OpenAIConverter::new());
        let anthropic_converter: Arc<dyn ProtocolConverter> = Arc::new(AnthropicConverter::new());

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            openai_converter,
            anthropic_converter,
            database,
        })
    }

    #[tokio::test]
    async fn test_admin_vendors_returns_list() {
        let state = create_api_state().await;
        let result = admin::vendors(State(state)).await;
        assert!(result.is_ok());
        let value = result.unwrap().0;
        assert!(value.is_array(), "vendors should return a JSON array");
        let arr = value.as_array().unwrap();
        assert_eq!(arr.len(), 1, "should have one vendor");
        assert_eq!(arr[0]["id"], "openai_main");
        assert!(arr[0]["status"].is_string(), "status must be a string");
        assert!(
            arr[0]["queue_length"].is_number(),
            "queue_length must be a number"
        );
    }

    #[tokio::test]
    async fn test_admin_vendor_status_known() {
        let state = create_api_state().await;
        let result = admin::vendor_status(State(state), Path("openai_main".to_string())).await;
        assert!(result.is_ok());
        let value = result.unwrap().0;
        assert_eq!(value["id"], "openai_main");
        assert!(value["status"].is_string());
        assert!(value["queue_length"].is_number());
    }

    #[tokio::test]
    async fn test_admin_vendor_status_unknown() {
        let state = create_api_state().await;
        let result = admin::vendor_status(State(state), Path("nonexistent".to_string())).await;
        assert!(result.is_ok());
        let value = result.unwrap().0;
        assert_eq!(value["id"], "nonexistent");
        // get_status 对未知 vendor 返回 Unavailable
        assert_eq!(value["status"], "Unavailable");
    }

    #[tokio::test]
    async fn test_admin_reset_vendor() {
        let state = create_api_state().await;
        let result = admin::reset_vendor(State(state), Path("openai_main".to_string())).await;
        assert!(
            result.is_ok(),
            "reset_vendor should succeed for any vendor id"
        );
    }

    #[tokio::test]
    async fn test_admin_get_config() {
        let state = create_api_state().await;
        let result = admin::get_config(State(state)).await;
        assert!(result.is_ok());
        let config = result.unwrap().0;
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.vendors.len(), 1);
        assert_eq!(config.vendors[0].id, "openai_main");
    }

    #[tokio::test]
    async fn test_admin_get_vendors() {
        let state = create_api_state().await;
        let result = admin::get_vendors(State(state)).await;
        assert!(result.is_ok());
        let vendors = result.unwrap().0;
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0].id, "openai_main");
        assert!(vendors[0].enabled);
    }

    /// GET /admin/models 响应面为目录条目列表（create_test_config
    /// 未登记条目 → 空 vec）；全局模型策略改经 /admin/config 或
    /// POST /admin/models/global 读写，不并入本列表。
    #[tokio::test]
    async fn test_admin_get_models() {
        let state = create_api_state().await;
        let result = admin::get_models(State(state.clone())).await;
        assert!(result.is_ok());
        let models = result.unwrap().0;
        assert!(
            models.is_empty(),
            "未登记目录为空列表（不再是 global_model 包装）"
        );
        // 全局策略段仍经 config 面可读（独立关注点）
        assert_eq!(
            state.config.get_config().global_model.unwrap().strategy,
            "weighted"
        );
    }

    #[tokio::test]
    async fn test_admin_update_vendor_not_found() {
        let state = create_api_state().await;
        let vendor = VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            chat_path: None,
            api_style: None,
            id: "nonexistent".to_string(),
            name: "Test".to_string(),
            type_: "openai".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: "".to_string(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: fluxgate::config::RateLimitConfig::Interval {
                hours: 1,
                sliding: false,
            },
            concurrency: fluxgate::config::ConcurrencyConfig {
                requests_per_second: 10,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
            supports_batch: false,
            anthropic_version: None,
            forward_beta_headers: false,
            send_nonstandard_params: false,
        };
        let result =
            admin::update_vendor(State(state), Path("nonexistent".to_string()), Json(vendor)).await;
        match result {
            Err(FluxgateError::Gate(GateError::Config { message })) => {
                assert!(
                    message.contains("not found"),
                    "message should mention 'not found': {}",
                    message
                );
            }
            Err(_) => panic!("Expected Config error for nonexistent vendor"),
            Ok(_) => panic!("update_vendor should fail for nonexistent vendor"),
        }
    }

    #[tokio::test]
    async fn test_admin_update_model_invalid_id() {
        let state = create_api_state().await;
        let model = GlobalModelConfig {
            enabled: true,
            strategy: "weighted".to_string(),
        };
        let result =
            admin::update_model(State(state), Path("non-global".to_string()), Json(model)).await;
        match result {
            Err(FluxgateError::Gate(GateError::Config { message })) => {
                assert!(
                    message.contains("global"),
                    "message should mention 'global': {}",
                    message
                );
            }
            Err(_) => panic!("Expected Config error for non-global model id"),
            Ok(_) => panic!("update_model should fail for non-global id"),
        }
    }

    #[tokio::test]
    async fn test_admin_metrics_empty() {
        let state = create_api_state().await;
        let result = admin::metrics(State(state)).await;
        assert!(result.is_ok());
        let value = result.unwrap().0;
        assert_eq!(value["total_requests"], 0);
        assert_eq!(value["successful_requests"], 0);
        assert_eq!(value["failed_requests"], 0);
        assert!((value["error_rate"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!((value["avg_duration_ms"].as_f64().unwrap() - 0.0).abs() < 1e-9);
        assert!(value["vendors"].is_array());
        assert!(value["vendors"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_admin_metrics_after_recorded_request() {
        let state = create_api_state().await;
        // 注入一次失败请求并验证快照反映
        state.vendor_manager.metrics().record_request(
            "openai_main",
            false,
            std::time::Duration::from_millis(120),
        );

        let result = admin::metrics(State(state.clone())).await;
        assert!(result.is_ok());
        let value = result.unwrap().0;
        assert_eq!(value["total_requests"], 1);
        assert_eq!(value["failed_requests"], 1);
        assert_eq!(value["successful_requests"], 0);
        assert!((value["error_rate"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // avg_duration_ms ≈ 120.0
        assert!((value["avg_duration_ms"].as_f64().unwrap() - 120.0).abs() < 1e-3);

        let vendors = value["vendors"].as_array().unwrap();
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0]["vendor"], "openai_main");
        assert_eq!(vendors[0]["failures"], 1);
    }

    /// update_config 响应显式声明建站期段（server/cors）变更需重启，
    /// 消除"返回 success 但 cors 静默不生效"的运维误判
    #[tokio::test]
    async fn test_admin_update_config_reports_restart_required_for_cors() {
        // update_config 返回 impl IntoResponse（Ok 分支为 Json<Value>），
        // 经 into_response 读 body 解析 JSON
        async fn body_json<R: IntoResponse>(resp: Result<R, FluxgateError>) -> serde_json::Value {
            let bytes =
                axum::body::to_bytes(resp.unwrap().into_response().into_body(), 1024 * 1024)
                    .await
                    .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        let state = create_api_state().await;
        // update_config 需要路径记忆才落盘
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        // 不动 server/cors：restart_required 为空
        let mut next = state.config.get_config();
        next.vendors[0].name = "renamed-only".to_string();
        let value = body_json(admin::update_config(State(state.clone()), Json(next)).await).await;
        assert_eq!(value["success"], true);
        assert_eq!(
            value["restart_required"].as_array().unwrap().len(),
            0,
            "未动建站期段时不应提示重启"
        );

        // 改 cors：restart_required 含 "cors"
        let mut next = state.config.get_config();
        let mut cors = next.cors.clone().unwrap_or_default();
        cors.allowed_origins = vec!["https://changed.example.com".to_string()];
        next.cors = Some(cors);
        let value = body_json(admin::update_config(State(state.clone()), Json(next)).await).await;
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "cors"),
            "cors 变更必须显式提示需重启: {:?}",
            required
        );
        // message 里也有可读提示
        assert!(
            value["message"].as_str().unwrap().contains("restart"),
            "message 应含 restart 提示: {:?}",
            value["message"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 建站期/装配期消费的键经 POST 改后不热生效，必须在 restart_required 里
    /// 显性提示，消除"返回 success 但静默失效"的运维误判（F17/F20）：
    /// docs 开关（swagger 路由在 create_router 期挂载）、
    /// routing.difficulty.enabled（定档路由 bootstrap 一次装配）、
    /// rate_limit（Router 构造期消费，config.example.toml ❌ 标注）。
    #[tokio::test]
    async fn test_admin_update_config_reports_restart_required_for_docs_and_difficulty() {
        async fn body_json<R: IntoResponse>(resp: Result<R, FluxgateError>) -> serde_json::Value {
            let bytes =
                axum::body::to_bytes(resp.unwrap().into_response().into_body(), 1024 * 1024)
                    .await
                    .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        let state = create_api_state().await;
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        // 翻转 docs.enabled（默认 false → true）
        let mut next = state.config.get_config();
        next.docs.enabled = !next.docs.enabled;
        let value = body_json(admin::update_config(State(state.clone()), Json(next)).await).await;
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "docs.enabled"),
            "docs 开关变更必须显式提示需重启: {required:?}"
        );

        // 新增一条 rate_limit 规则：Router 构造期消费，改后须重启
        let mut next = state.config.get_config();
        next.rate_limit.rules.push(fluxgate::config::RateLimitRule {
            id: "restart-probe".to_string(),
            path: None,
            per_tenant: false,
            window_secs: 60,
            max_requests: 1,
        });
        let value = body_json(admin::update_config(State(state.clone()), Json(next)).await).await;
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "rate_limit"),
            "rate_limit 变更必须显式提示需重启: {required:?}"
        );

        // 翻转 routing.difficulty.enabled（默认 false → true）；纯规则形态
        //（w_clf=0，不要求 difficulty-classifier feature）+ 档位指向本次请求
        // 里新声明的抽象模型名（校验 V5/V6 要求档位可解析且不与 model_id 撞名；
        // 本夹具唯一的模型名 gpt-4 即 model_id，直接用会撞名）
        let mut next = state.config.get_config();
        next.vendors[0].models[0].abstract_id = Some("difficulty-low".to_string());
        next.routing.difficulty.enabled = true;
        next.routing.difficulty.w_clf = 0.0;
        next.routing.difficulty.tiers.low = "difficulty-low".to_string();
        next.routing.difficulty.tiers.high = "difficulty-low".to_string();
        let value = body_json(admin::update_config(State(state.clone()), Json(next)).await).await;
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "routing.difficulty.enabled"),
            "难度定档开关变更必须显式提示需重启: {required:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 真机回归：config.toml 被进程外修改后，管理面写回必须显式失败且磁盘
    /// 编辑逐字节保留——此前的全量写回会用内存态覆盖磁盘，静默抹掉按约定
    /// 「改文件后重启」新增、但热重载以 topology changed 拒绝的 vendor 段。
    #[tokio::test]
    async fn test_admin_update_config_refuses_after_external_edit() {
        let state = create_api_state().await;
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        // 进程外编辑（模拟操作者追加待重启生效的 [[vendors]] 段）
        let edited = format!(
            "{}\n[[vendors]]\nid = \"pending-vendor\"\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &edited).unwrap();

        let mut next = state.config.get_config();
        next.vendors[0].name = "renamed-after-external-edit".to_string();
        match admin::update_config(State(state.clone()), Json(next)).await {
            Err(FluxgateError::Gate(GateError::Config { message })) => {
                assert!(
                    message.contains("config.toml"),
                    "错误必须点名被外部修改的文件: {message}"
                );
            }
            Err(other) => panic!("期望 Config 错误，实际: {other:?}"),
            Ok(_) => panic!("外部编辑未消解前写回必须拒绝"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            edited,
            "拒绝写回时磁盘保持原样（外部编辑不丢）"
        );
        assert_eq!(
            state.config.get_vendor("openai_main").unwrap().name,
            "OpenAI Main",
            "内存态不变"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// update_vendor 经原子 RMW：改单字段不回退其他字段，且未找到时
    /// 内存不变
    #[tokio::test]
    async fn test_admin_update_vendor_atomic_rmw() {
        let state = create_api_state().await;
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        // RMW 改 name：api_key_env 等其他字段保留
        let mut vendor = state.config.get_vendor("openai_main").unwrap();
        vendor.name = "rmw-renamed".to_string();
        let result = admin::update_vendor(
            State(state.clone()),
            Path("openai_main".into()),
            Json(vendor),
        )
        .await;
        assert!(result.is_ok());
        let fresh = state.config.get_vendor("openai_main").unwrap();
        assert_eq!(fresh.name, "rmw-renamed");
        assert_eq!(fresh.api_key_env, "OPENAI_API_KEY", "RMW 不回退其他字段");

        // 未找到 → Err 且内存不变
        let vendor = state.config.get_vendor("openai_main").unwrap();
        let result =
            admin::update_vendor(State(state.clone()), Path("ghost".into()), Json(vendor)).await;
        assert!(result.is_err());
        assert!(state.config.get_vendor("openai_main").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 流式路径错误传播 —— 上游非 2xx 不再以 200 正文伪装成 SSE 流。
///
/// - send 后先校验状态码：401 → VendorUnavailable（error.rs 固定映射 503
///   JSON），429 → RateLimited（固定映射 429）；
/// - 透传流式与转换流式两路径同口径（消除同网关两套行为）；
/// - 错误分支补 GlobalMetrics 记账（此前流式路径零指标）。
mod streaming_error_propagation {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::{chat_completions, messages};
    use fluxgate::config::{Config, ConfigManager, ModelConfig, ServerConfig, VendorConfig};
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// type_ 决定出站协议与 URL 形状：anthropic → {base}/v1/messages，
    /// openai → {base}/v1/chat/completions。
    /// 差异覆盖（其余字段继承 common 基座）：type_ 参数化、模型
    /// target-model→gpt-4、rps 100。
    fn make_vendor(id: &str, type_: &str, api_base: String) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.type_ = type_.into();
        v.models = vec![ModelConfig {
            model_id: "target-model".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v.concurrency.requests_per_second = 100;
        v
    }

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080。
    fn make_config(vendors: Vec<VendorConfig>) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vendors)
        }
    }

    async fn make_api_state(config: Config) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });

        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    async fn post_streaming(
        uri: &str,
        state: Arc<ApiState>,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn anthropic_streaming_body() -> serde_json::Value {
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 10,
            "stream": true
        })
    }

    /// ①③ 透传流式（anthropic 入站 + anthropic vendor）：上游 401 →
    /// 客户端收 503 JSON（VendorUnavailable 固定映射），Content-Type 非
    /// text/event-stream；且 metrics.failed_requests 递增（④）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn passthrough_stream_upstream_401_yields_503_json_not_sse() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let state = make_api_state(make_config(vec![make_vendor(
            "passthrough_anthropic",
            "anthropic",
            server.url(),
        )]))
        .await;
        state.vendor_manager.initialize().await.expect("init");

        let failed_before = state.vendor_manager.metrics().snapshot().failed_requests;

        let response =
            post_streaming("/v1/messages", state.clone(), anthropic_streaming_body()).await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "上游 401 应映射为 503（VendorUnavailable），而非 200 伪流"
        );
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            !content_type.contains("text/event-stream"),
            "错误响应不得是 SSE 流，实际 content-type: {content_type}"
        );

        // 响应体是 error JSON 结构
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON body");
        assert!(body["error"].is_object(), "应含 error 对象: {body}");
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        // ④ metrics 记账
        let failed_after = state.vendor_manager.metrics().snapshot().failed_requests;
        assert!(
            failed_after > failed_before,
            "流式错误分支应递增 failed_requests（{failed_before} → {failed_after}）"
        );

        mock.assert_async().await;
    }

    /// ③ 转换流式（anthropic 入站 + openai vendor）：上游 401 → 503，
    /// 与透传流式同口径。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn conversion_stream_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        // 出站 openai vendor：入站 anthropic 端点走转换路径
        // （execute_stream_request）
        let state = make_api_state(make_config(vec![make_vendor(
            "conversion_openai",
            "openai",
            server.url(),
        )]))
        .await;
        state.vendor_manager.initialize().await.expect("init");

        let response = post_streaming("/v1/messages", state, anthropic_streaming_body()).await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "转换流式与透传流式应同口径：401 → 503"
        );

        mock.assert_async().await;
    }

    /// ② 上游 429 → 客户端收 429（RateLimited 固定映射，可退避重试）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn conversion_stream_upstream_429_yields_429() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(429)
            .with_body(r#"{"error":{"message":"rate limited"}}"#)
            .expect(1)
            .create_async()
            .await;

        let state = make_api_state(make_config(vec![make_vendor(
            "rate_limited_openai",
            "openai",
            server.url(),
        )]))
        .await;
        state.vendor_manager.initialize().await.expect("init");

        let failed_before = state.vendor_manager.metrics().snapshot().failed_requests;

        let response =
            post_streaming("/v1/messages", state.clone(), anthropic_streaming_body()).await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "上游 429 应映射为 429（RateLimited），客户端可退避重试"
        );

        // ④ metrics 记账（429 同样计失败）
        let failed_after = state.vendor_manager.metrics().snapshot().failed_requests;
        assert!(failed_after > failed_before);

        mock.assert_async().await;
    }

    /// 评审 blocker 回归：Anthropic 转换流式成功路径 —— 每个事件必须是带
    /// `data:` 前缀的规范 SSE 行（此前 api 层只拼 `\n\n`，产出裸 JSON 行，
    /// 标准客户端按 SSE 规范忽略未知字段行 → 零事件零文本）。
    /// 同时断言完整生命周期序列与文本/usage 内容。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn conversion_stream_success_emits_sse_data_prefixed_lifecycle() {
        let mut server = mockito::Server::new_async().await;

        // OpenAI 上游 SSE：role+文本 delta、文本 delta、finish_reason、
        // usage 尾帧、[DONE]
        let upstream_sse = concat!(
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":" there"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "\n\n",
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
            "\n\n",
            "data: [DONE]\n\n",
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(upstream_sse)
            .expect(1)
            .create_async()
            .await;

        // 出站 openai vendor：入站 anthropic 端点走转换流式路径
        let state = make_api_state(make_config(vec![make_vendor(
            "conversion_stream_success",
            "openai",
            server.url(),
        )]))
        .await;
        state.vendor_manager.initialize().await.expect("init");

        let response = post_streaming("/v1/messages", state, anthropic_streaming_body()).await;

        assert_eq!(response.status(), 200);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.contains("text/event-stream"),
            "成功流式应为 SSE，实际 content-type: {content_type}"
        );

        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).expect("utf-8 body");

        // 线格式：非空行必须全部带 "data: " 前缀（评审 blocker 的核心断言）
        let non_empty_lines: Vec<&str> = body
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(!non_empty_lines.is_empty(), "流式响应不应为空：{body:?}");
        for line in &non_empty_lines {
            assert!(
                line.starts_with("data: "),
                "每个 SSE 事件行必须带 data: 前缀，实际行: {line:?}"
            );
        }

        // 事件序列：message_start → content_block_start → delta×2 →
        // content_block_stop → message_delta → message_stop
        let events: Vec<serde_json::Value> = non_empty_lines
            .iter()
            .filter_map(|line| {
                let payload = line.strip_prefix("data: ")?;
                serde_json::from_str(payload).ok()
            })
            .collect();
        let types: Vec<&str> = events
            .iter()
            .filter_map(|v| v.get("type").and_then(|t| t.as_str()))
            .collect();

        assert_eq!(
            types.first().copied(),
            Some("message_start"),
            "首个事件必须是 message_start: {types:?}"
        );
        assert_eq!(
            types.last().copied(),
            Some("message_stop"),
            "流必须以 message_stop 收尾: {types:?}"
        );
        assert!(types.contains(&"content_block_start"), "{types:?}");
        assert_eq!(
            types
                .iter()
                .filter(|t| **t == "content_block_delta")
                .count(),
            2,
            "两个文本 delta 各产一个 content_block_delta: {types:?}"
        );
        assert!(types.contains(&"content_block_stop"), "{types:?}");
        assert!(types.contains(&"message_delta"), "{types:?}");

        // 文本内容拼接与 usage 透传
        let text: String = events
            .iter()
            .filter_map(|v| v["delta"]["text"].as_str())
            .collect();
        assert_eq!(text, "Hi there");
        let message_delta = events
            .iter()
            .find(|v| v["type"] == "message_delta")
            .expect("message_delta present");
        assert_eq!(message_delta["usage"]["output_tokens"], 2);

        // Anthropic 出站以 message_stop 结束，不发 OpenAI 的 [DONE]
        assert!(
            !body.contains("[DONE]"),
            "Anthropic 流不得带 [DONE]: {body:?}"
        );

        mock.assert_async().await;
    }
}

// ============ 日志管道接线（异步批量 + token 语义 + 失败落库） ============

#[cfg(test)]
mod logging_pipeline {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::{chat_completions, ApiState};
    use fluxgate::config::{CacheSection, Config, ConfigManager, ServerConfig};
    use fluxgate::database::{
        AsyncLogger, AsyncLoggerHandle, LogFilter, Repository, TokenPreAggregator,
    };
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::storage::UsageSink;
    use fluxgate::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
    /// [cache] 显式开启 enabled=true ttl=300（日志管道回归的前提接线）、
    /// 厂商名称 OpenAI Main + rps 100。
    fn make_config(api_base: String) -> Config {
        common::ensure_test_api_key();
        let mut v = common::base_vendor("openai_main", api_base);
        v.name = "OpenAI Main".into();
        v.concurrency.requests_per_second = 100;
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            ..common::test_config(vec![v])
        }
    }

    /// 接线完整的 ApiState：AsyncLogger + TokenPreAggregator + 已初始化 schema。
    async fn make_wired_state(
        api_base: String,
    ) -> (
        Arc<ApiState>,
        Arc<AsyncLoggerHandle>,
        Arc<TokenPreAggregator>,
        Arc<Repository>,
    ) {
        make_wired_state_with_config(make_config(api_base)).await
    }

    /// 同 [`make_wired_state`]，配置由调用方提供（模型维度等差异夹具）。
    async fn make_wired_state_with_config(
        config: Config,
    ) -> (
        Arc<ApiState>,
        Arc<AsyncLoggerHandle>,
        Arc<TokenPreAggregator>,
        Arc<Repository>,
    ) {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");

        let logger = Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
            repository.clone(),
            1024,
            Duration::from_millis(200),
        )));
        let pa = Arc::new(TokenPreAggregator::new(
            repository.clone(),
            Duration::from_secs(60),
        ));

        let database = Arc::new(fluxgate::database::Database {
            pool,
            repository: repository.clone(),
        });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        let state = Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: Some(logger.clone()),
            pre_aggregator: Some(pa.clone()),
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        });
        (state, logger, pa, repository)
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    async fn post_chat(state: Arc<ApiState>, body: serde_json::Value) -> axum::response::Response {
        app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn chat_body() -> serde_json::Value {
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 100
        })
    }

    /// ④ mockito 成功后（shutdown flush）→ request_logs 行的 prompt_tokens
    /// == 响应 usage.prompt_tokens（15，而非旧实现的请求 max_tokens 100）、
    /// status_code==200、completion/total 同步落库。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn success_request_logs_real_usage_and_status() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":10,"total_tokens":25}}"#,
            )
            .create_async()
            .await;

        let (state, logger, _pa, repository) =
            make_wired_state(format!("{}/v1", server.url())).await;

        let response = post_chat(state, chat_body()).await;
        assert_eq!(response.status(), 200);

        // push 即返回：shutdown 触发最终 flush
        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1, "成功请求必须落一条日志");
        let log = &logs[0];
        assert_eq!(log.status_code, Some(200), "status_code 取实际结果");
        assert_eq!(
            log.prompt_tokens,
            Some(15),
            "prompt_tokens 必须取响应 usage（15），而非请求 max_tokens（100）"
        );
        assert_eq!(log.completion_tokens, Some(10));
        assert_eq!(log.total_tokens, Some(25));
        assert!(log.error_message.is_none());
        assert!(log.duration_ms.is_some());
    }

    /// ⑧ 流式请求的用量记账：上游 usage 尾帧必须补写 `request_logs` 三列并进
    /// 预聚合分账表；此前流式整链漏计——日志只记 `{"stream": true}` 占位、
    /// 分账表零写入（流式流量在 token/成本口径上整体消失）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_request_logs_usage_from_tail_frame() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                );
                // usage 尾帧（OpenAI stream_options.include_usage 形态）
                let _ = w.write_all(
                    b"data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
                );
                let _ = w.write_all(b"data: [DONE]\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let (state, logger, _pa, repository) =
            make_wired_state(format!("{}/v1", server.url())).await;

        let mut stream_body = chat_body();
        stream_body["stream"] = json!(true);
        let response = post_chat(state, stream_body).await;
        assert_eq!(response.status(), 200);
        // 收尾记账随流终止触发：客户端必须把流读完
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("consume stream");
        assert!(
            String::from_utf8_lossy(&bytes).contains("[DONE]"),
            "透传流必须原样转发到 [DONE]"
        );

        // 收尾在流终止处的异步任务里落库
        tokio::time::sleep(Duration::from_millis(400)).await;
        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1, "流式请求必须落一条日志");
        let log = &logs[0];
        assert_eq!(log.prompt_tokens, Some(11), "usage 尾帧必须补写三列");
        assert_eq!(log.completion_tokens, Some(7));
        assert_eq!(log.total_tokens, Some(18));
        let response = log.response.as_deref().unwrap_or("");
        assert!(
            response.contains("\"prompt_tokens\":11"),
            "response 列必须补写 usage：{response}"
        );
    }

    /// 透传流的分账 model 维度取上游 `model_id`（抽象名只进 request_logs）。
    /// 旧实现取请求体抽象名，会让同一流量在 token_usage 里被拆成两个维度。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_usage_keyed_accounting_uses_upstream_model_id() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                );
                let _ = w.write_all(
                    b"data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
                );
                let _ = w.write_all(b"data: [DONE]\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut config = make_config(format!("{}/v1", server.url()));
        config.vendors[0].models[0].model_id = "gpt-4-0613".into();
        let (state, logger, pa, repository) = make_wired_state_with_config(config).await;

        let mut stream_body = chat_body();
        stream_body["stream"] = json!(true);
        let response = post_chat(state, stream_body).await;
        assert_eq!(response.status(), 200);
        let _ = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("consume stream");

        tokio::time::sleep(Duration::from_millis(400)).await;
        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;

        pa.flush().await.expect("flush");
        let stats = pa
            .get_token_stats(Some("openai_main"))
            .await
            .expect("token stats");
        assert_eq!(stats.len(), 1, "流式完毕必须写 keyed 分账");
        assert_eq!(stats[0].model, "gpt-4-0613", "分账维度必须是上游 model_id");
        assert_eq!(stats[0].prompt_tokens, 11);
        assert_eq!(stats[0].completion_tokens, 7);

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].abstract_model.as_deref(), Some("gpt-4"));
        assert_eq!(logs[0].actual_model, "gpt-4-0613");
    }

    /// ⑤ 上游 500 → 客户端 503（VendorUnavailable 固定映射），且失败路径
    /// 落库：error_message 非空、status_code==500（上游真实状态码）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upstream_500_is_logged_with_real_status_and_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let (state, logger, _pa, repository) =
            make_wired_state(format!("{}/v1", server.url())).await;

        let response = post_chat(state, chat_body()).await;
        assert_eq!(response.status(), 503, "上游 500 → 客户端 503");

        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1, "失败请求必须在 ? 早退前补记落库");
        let log = &logs[0];
        assert!(
            !log.error_message.as_deref().unwrap_or_default().is_empty(),
            "error_message 必须非空: {:?}",
            log.error_message
        );
        assert_eq!(
            log.status_code,
            Some(500),
            "落库 status_code 取上游真实状态码（500），非硬编码 200"
        );
        assert!(
            log.prompt_tokens.is_none(),
            "失败无响应 usage，tokens 应为空"
        );
    }

    /// ⑦Anthropic 格式响应的 usage 键是 input_tokens/
    /// output_tokens（透传路径原始上游 JSON；转换路径
    /// response_to_external 同格式）——落库必须双格式取值：prompt_tokens
    /// 回退 input_tokens、total 以 prompt+completion 合成，且预聚合器记账
    /// （此前三个 token 字段恒 NULL、token_stats 对 anthropic 流量恒空）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_usage_keys_are_mapped_end_to_end() {
        use axum::routing::post as post_route;
        use fluxgate::api::messages;

        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"msg_01","type":"message","role":"assistant","model":"claude-x","content":[{"type":"text","text":"Hi from anthropic"}],"stop_reason":"end_turn","usage":{"input_tokens":7,"output_tokens":3}}"#,
            )
            .create_async()
            .await;

        // anthropic 入站 + anthropic vendor → 透传路径，响应为 Anthropic 格式
        let mut config = make_config(format!("{}/v1", server.url()));
        config.vendors[0].type_ = "anthropic".into();
        config.vendors[0].models[0].model_id = "claude-x".into();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");

        let logger = Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
            repository.clone(),
            1024,
            Duration::from_millis(200),
        )));
        let pa = Arc::new(TokenPreAggregator::new(
            repository.clone(),
            Duration::from_secs(60),
        ));

        let state = Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(fluxgate::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: Some(logger.clone()),
            pre_aggregator: Some(pa.clone()),
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        });

        let response = Router::new()
            .route("/v1/messages", post_route(messages))
            .with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "hello"}],
                            "max_tokens": 10
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1, "成功请求必须落一条日志");
        let log = &logs[0];
        assert_eq!(
            log.prompt_tokens,
            Some(7),
            "Anthropic input_tokens 必须映射到 prompt_tokens"
        );
        assert_eq!(
            log.completion_tokens,
            Some(3),
            "Anthropic output_tokens 必须映射到 completion_tokens"
        );
        assert_eq!(
            log.total_tokens,
            Some(10),
            "Anthropic 无 total_tokens 键，须以 input+output 合成"
        );

        // 预聚合器必须因双格式映射而记账（此前 token 全 None 短路）
        pa.flush().await.expect("aggregator flush");
        let stats = repository.get_token_stats(None).await.expect("token stats");
        assert!(!stats.is_empty(), "anthropic 流量的 token_stats 不得为空");
        assert_eq!(stats[0].total_tokens, 10);
        assert_eq!(stats[0].prompt_tokens, 7);
        assert_eq!(stats[0].completion_tokens, 3);
    }

    /// ⑥ token_stats 在 PreAggregator flush 后非空（预聚合接线证明）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pre_aggregator_flush_populates_token_stats() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
            )
            .create_async()
            .await;

        let (state, logger, pa, repository) =
            make_wired_state(format!("{}/v1", server.url())).await;

        let response = post_chat(state, chat_body()).await;
        assert_eq!(response.status(), 200);

        logger.shutdown().await.expect("shutdown");
        pa.flush().await.expect("aggregator flush");

        let stats = repository.get_token_stats(None).await.expect("token stats");
        assert!(!stats.is_empty(), "flush 后 token_stats 必须非空");
        assert_eq!(stats[0].total_tokens, 10);
        assert_eq!(stats[0].request_count, 1);
    }
}

// ============ embeddings 回归供应商治理 ============

#[cfg(test)]
mod embeddings_governance {
    use crate::common;
    use axum::{
        body::Body,
        extract::{Path, State},
        http::Request,
        response::IntoResponse,
        routing::post,
        Router,
    };
    use fluxgate::api::{admin, embeddings, ApiState};
    use fluxgate::config::{
        CacheSection, Config, ConfigManager, ModelConfig, RateLimitConfig, ServerConfig,
        VendorConfig,
    };
    use fluxgate::database::{AsyncLogger, AsyncLoggerHandle, Repository, TokenPreAggregator};
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 差异覆盖（其余字段继承 common 基座）：embeddings 模型
    /// text-embedding-3-small、Token 配额 max_tokens 参数、rps 100。
    fn make_vendor(id: &str, api_base: String, max_tokens: u64) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.models = vec![ModelConfig {
            model_id: "text-embedding-3-small".into(),
            abstract_id: Some("text-embedding-3-small".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v.rate_limit = RateLimitConfig::Token {
            max_tokens,
            window_hours: None,
            sliding: false,
        };
        v.concurrency.requests_per_second = 100;
        v
    }

    async fn make_state(vendors: Vec<VendorConfig>) -> Arc<ApiState> {
        // 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
        // [cache] enabled=true ttl=300（embeddings 缓存语义接线）。
        let config = Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            ..common::test_config(vendors)
        };
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema");

        Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(
                NexusRouter::new(config_manager, vendor_manager)
                    .with_embedding_pin(Arc::new(fluxgate::router::EmbeddingPin::new())),
            ),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(fluxgate::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: Some(Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
                repository.clone(),
                64,
                std::time::Duration::from_secs(60),
            )))),
            pre_aggregator: Some(Arc::new(TokenPreAggregator::new(
                repository,
                std::time::Duration::from_secs(3600),
            ))),
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/embeddings", post(embeddings))
            .with_state(state)
    }

    async fn post_embeddings(
        state: Arc<ApiState>,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn embedding_body() -> serde_json::Value {
        json!({"model": "text-embedding-3-small", "input": "hello"})
    }

    /// ① embeddings 经 mockito 成功 → 响应透传，且
    /// metrics.snapshot().vendors 含该 vendor requests==1（治理接线证明：
    /// 旧实现每请求 reqwest::Client::new() 不计 metrics）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_success_passthrough_and_counts_metrics() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}],"model":"text-embedding-3-small","usage":{"prompt_tokens":3,"total_tokens":3}}"#,
            )
            .create_async()
            .await;

        let state = make_state(vec![make_vendor(
            "emb-governed",
            format!("{}/v1", server.url()),
            1_000_000,
        )])
        .await;

        let response = post_embeddings(state.clone(), embedding_body()).await;
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["object"], "list", "上游响应必须原样透传");
        assert_eq!(body["data"][0]["embedding"], json!([0.1, 0.2]));

        // 治理接线证明：metrics 记账（旧实现是盲区）
        let vendors = state.vendor_manager.metrics().snapshot().vendors;
        let v = vendors
            .iter()
            .find(|v| v.vendor == "emb-governed")
            .expect("embeddings 必须出现在 metrics（治理路径）");
        assert_eq!(v.requests, 1);
        assert_eq!(v.successes, 1);

        mock.assert_async().await;
    }

    /// ② 限流生效：Token 配额=1 且首个请求消费完 → 后续并发请求不再触达
    /// 上游（配额外零出站），且状态非 200 —— 路由层把 Throttled vendor 排除
    ///（Validation 400：无可用候选）或 manager 层 RateLimited 429，两者都是
    /// 治理生效的表现；旧实现绕过治理永远 200。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn embeddings_rate_limiting_kicks_in_after_quota() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
            )
            // 恰好 1 次：配额耗尽后不允许再有任何出站（治理硬证据）
            .expect(1)
            .create_async()
            .await;

        // max_tokens=1：首个请求 consume(1) 后窗口耗尽
        let state = make_state(vec![make_vendor(
            "emb-quota",
            format!("{}/v1", server.url()),
            1,
        )])
        .await;

        let first = post_embeddings(state.clone(), embedding_body()).await;
        assert_eq!(first.status(), 200, "首个请求在配额内必须成功");

        // 并发 5：全部被治理路径拦截（路由排除 → 400，或 manager 层 → 429）
        let mut handles = Vec::new();
        for _ in 0..5 {
            let st = state.clone();
            handles.push(tokio::spawn(async move {
                post_embeddings(st, embedding_body()).await.status()
            }));
        }
        let mut rejected = 0;
        for h in handles {
            let status = h.await.expect("join");
            assert_ne!(status, 200, "配额耗尽后的请求不得成功");
            assert!(
                status == 400 || status == 429,
                "治理拦截形态：400（路由排除）或 429（限流），实际 {status}"
            );
            rejected += 1;
        }
        assert_eq!(rejected, 5, "限流生效：5 个后续请求全部被拒");
        mock.assert_async().await;
    }

    /// ③ 上游 401 → 客户端 503（与 错误传播一致
    /// VendorUnavailable 固定映射，错误体结构同款）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(vec![make_vendor(
            "emb-401",
            format!("{}/v1", server.url()),
            1_000_000,
        )])
        .await;

        let response = post_embeddings(state.clone(), embedding_body()).await;
        assert_eq!(
            response.status(),
            503,
            "上游 401 必须映射 503（VendorUnavailable）"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        // 失败同样计入 metrics
        let vendors = state.vendor_manager.metrics().snapshot().vendors;
        let v = vendors.iter().find(|v| v.vendor == "emb-401").unwrap();
        assert_eq!(v.failures, 1);

        mock.assert_async().await;
    }

    /// mock 依请求 input 返回指定维度的嵌入响应（动态体：维度守门用例
    /// 需要同一 vendor/model 键上先见维度与漂移维度）
    fn dimension_aware_mock(
        server: &mut mockito::ServerGuard,
        expected_calls: usize,
        dims_for_input: impl Fn(&str) -> u32 + Send + Sync + 'static,
    ) -> mockito::Mock {
        server
            .mock("POST", "/v1/embeddings")
            .with_status(200)
            .with_header("content-type", "application/json")
            .expect(expected_calls)
            .with_body_from_request(move |request| {
                let body: serde_json::Value =
                    serde_json::from_slice(request.body().expect("request body"))
                        .expect("json body");
                // 网关转换后 input 恒为数组（embedding_request_to_openai）
                let input = body["input"]
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| body["input"].as_str().map(str::to_string))
                    .unwrap_or_default();
                let n = dims_for_input(&input);
                let vector: Vec<f64> = (0..n).map(|i| 0.1 + i as f64).collect();
                serde_json::to_vec(&json!({
                    "object": "list",
                    "data": [{"object": "embedding", "index": 0, "embedding": vector}],
                    "model": "text-embedding-3-small",
                    "usage": {"prompt_tokens": 1, "total_tokens": 1},
                }))
                .expect("serialize response")
            })
            .create()
    }

    /// ④ 维度守门：首见放行并记录；同键后续响应维度漂移 → 502，错误体含
    /// vendor/model/新旧维度值，漂移向量不落给客户端。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_dimension_drift_rejected_with_502() {
        let mut server = mockito::Server::new_async().await;
        let mock = dimension_aware_mock(&mut server, 3, |input| match input {
            "drift" => 3,
            _ => 2,
        });

        let state = make_state(vec![make_vendor(
            "emb-dim",
            format!("{}/v1", server.url()),
            1_000_000,
        )])
        .await;

        // 首见放行（2 维）
        let first = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "first"}),
        )
        .await;
        assert_eq!(first.status(), 200, "首见响应必须放行并记录");

        // 同键一致放行（2 维）
        let same = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "same"}),
        )
        .await;
        assert_eq!(same.status(), 200, "维度一致的后续响应必须放行");

        // 漂移（3 维）→ 502 + 错误体四要素
        let drift = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "drift"}),
        )
        .await;
        assert_eq!(drift.status(), 502, "维度漂移必须拒绝为 502");
        let bytes = axum::body::to_bytes(drift.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("emb-dim"),
            "错误体必须点名 vendor: {message}"
        );
        assert!(
            message.contains("text-embedding-3-small"),
            "错误体必须点名 model: {message}"
        );
        assert!(
            message.contains(" 2 ") && message.contains(" 3 "),
            "错误体必须含新旧维度值: {message}"
        );
        assert_eq!(
            state
                .vendor_manager
                .metrics()
                .snapshot()
                .embedding_dimension_mismatch_total,
            1,
            "维度漂移必须计数一次"
        );

        mock.assert_async().await;
    }

    /// ⑤ 维度守门：带 dimensions 参数的请求不写入也不比对记录——
    /// 参数请求的维度既不能污染无参请求的首见记录，也不受其约束。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_dimensions_param_request_bypasses_recording() {
        let mut server = mockito::Server::new_async().await;
        let mock = dimension_aware_mock(&mut server, 4, |input| match input {
            "param" => 1,
            "param2" => 5,
            "drift" => 3,
            _ => 2,
        });

        let state = make_state(vec![make_vendor(
            "emb-param",
            format!("{}/v1", server.url()),
            1_000_000,
        )])
        .await;

        // a. 带参数请求（1 维）→ 200 且不记录
        let a = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "param", "dimensions": 1}),
        )
        .await;
        assert_eq!(a.status(), 200, "带参数请求必须放行");

        // b. 无参请求（2 维）→ 200 首见放行（若 a 写入了 1 维记录，此处必 502）
        let b = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "plain"}),
        )
        .await;
        assert_eq!(b.status(), 200, "带参数请求不得写入记录");

        // c. 带参数请求（5 维）→ 200 不比对（若比对会与记录的 2 维冲突）
        let c = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "param2", "dimensions": 5}),
        )
        .await;
        assert_eq!(c.status(), 200, "带参数请求不得参与比对");

        // d. 无参漂移（3 维）→ 502，证明 b 的首见记录仍生效
        let d = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "drift"}),
        )
        .await;
        assert_eq!(d.status(), 502, "无参请求的漂移仍必须被拦截");

        mock.assert_async().await;
    }

    /// ⑥ T008 admin pin 管理：GET 列表反映嵌入请求建立的 pin；POST 单条重置
    /// 幂等（存在→cleared=true 且计数，不存在→cleared=false 不计数）；重置后
    /// 下一请求按权重重新粘着（等权双厂商有界轮询内必现换厂商）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_embedding_pins_list_reset_and_reattach() {
        let mut server = mockito::Server::new_async().await;
        let embedding_upstream = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1]}],"model":"text-embedding-3-small","usage":{"prompt_tokens":1,"total_tokens":1}}"#;
        for _ in 0..2 {
            server
                .mock("POST", "/v1/embeddings")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(embedding_upstream)
                .create();
        }

        let state = make_state(vec![
            make_vendor("pin-a", format!("{}/v1", server.url()), 1_000_000),
            make_vendor("pin-b", format!("{}/v1", server.url()), 1_000_000),
        ])
        .await;

        // 初始列表为空
        let empty = admin::embedding_pins(State(state.clone())).await.unwrap();
        assert!(empty.0.as_array().unwrap().is_empty(), "初始 pin 列表为空");

        // 首请求建立 pin（等权二选一，单例组键）
        let first = post_embeddings(state.clone(), embedding_body()).await;
        assert_eq!(first.status(), 200);
        let entries = admin::embedding_pins(State(state.clone())).await.unwrap().0;
        let entries = entries.as_array().unwrap();
        assert_eq!(entries.len(), 1, "首请求建立一条 pin");
        assert_eq!(entries[0]["abstract_id"], "text-embedding-3-small");
        let first_group = entries[0]["group_key"].as_str().unwrap().to_string();
        assert!(
            first_group == "pin-a/::text-embedding-3-small"
                || first_group == "pin-b/::text-embedding-3-small",
            "单例组键形态: {first_group}"
        );

        // 重置存在条目 → cleared=true；列表清空
        let reset = admin::reset_embedding_pin(
            State(state.clone()),
            Path("text-embedding-3-small".to_string()),
        )
        .await
        .unwrap();
        let reset_json = reset_body(reset).await;
        assert_eq!(reset_json["cleared"], json!(true), "存在的 pin 重置成功");
        assert!(
            admin::embedding_pins(State(state.clone()))
                .await
                .unwrap()
                .0
                .as_array()
                .unwrap()
                .is_empty(),
            "重置后列表清空"
        );

        // 重复重置（已不存在）→ 幂等成功 cleared=false，不计数
        let absent = admin::reset_embedding_pin(
            State(state.clone()),
            Path("text-embedding-3-small".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(reset_body(absent).await["cleared"], json!(false));
        let resets_before_loop = state
            .vendor_manager
            .metrics()
            .snapshot()
            .embedding_pin_reset_total;
        assert_eq!(
            resets_before_loop, 1,
            "仅实际移除计入（首例 +1，幂等空重置不增）"
        );

        // 重置后按权重重新粘着：等权双厂商 20 轮内必现换厂商
        //（全同概率 ~1e-6；若重置后仍被旧 pin 约束则恒不换，必失败）。
        // 每轮先请求重建 pin 再重置，保证每次 reset 都落在存在的 pin 上
        let mut switched = false;
        let mut reset_count = 0u64;
        for _ in 0..20 {
            let response = post_embeddings(state.clone(), embedding_body()).await;
            assert_eq!(response.status(), 200);
            let group = admin::embedding_pins(State(state.clone())).await.unwrap().0;
            let group = group.as_array().unwrap()[0]["group_key"]
                .as_str()
                .unwrap()
                .to_string();
            if group != first_group {
                switched = true;
                break;
            }
            let r = admin::reset_embedding_pin(
                State(state.clone()),
                Path("text-embedding-3-small".to_string()),
            )
            .await
            .unwrap();
            assert_eq!(reset_body(r).await["cleared"], json!(true));
            reset_count += 1;
        }
        assert!(switched, "重置后必须按权重重新粘着（20 轮内换厂商）");
        assert_eq!(
            state
                .vendor_manager
                .metrics()
                .snapshot()
                .embedding_pin_reset_total,
            resets_before_loop + reset_count,
            "每次对存在 pin 的重置都计数（+1/次）"
        );
    }

    /// ⑦ F1 级联：维度漂移 502 后按错误文案指引调 reset 端点必须能恢复
    ///（级联清除维度基准）——单厂商形态下 502 → reset → 同漂移维度放行为
    /// 新基准
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_reset_recovers_dimension_drift_single_vendor() {
        let mut server = mockito::Server::new_async().await;
        let mock = dimension_aware_mock(&mut server, 3, |input| match input {
            "drift" => 3,
            _ => 2,
        });

        let state = make_state(vec![make_vendor(
            "emb-drift",
            format!("{}/v1", server.url()),
            1_000_000,
        )])
        .await;

        // 首见 2 维放行（建立基准）
        let first = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "first"}),
        )
        .await;
        assert_eq!(first.status(), 200);

        // 漂移 3 维 → 502
        let drift = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "drift"}),
        )
        .await;
        assert_eq!(drift.status(), 502);

        // 按指引 reset（pin 存在 → cleared=true；维度基准级联清除）
        let reset = admin::reset_embedding_pin(
            State(state.clone()),
            Path("text-embedding-3-small".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(reset_body(reset).await["cleared"], json!(true));

        // 同漂移维度（上游模型已变更的新空间）放行并成为新基准
        let recovered = post_embeddings(
            state.clone(),
            json!({"model": "text-embedding-3-small", "input": "drift"}),
        )
        .await;
        assert_eq!(
            recovered.status(),
            200,
            "reset 必须级联清除维度基准，否则此处永久 502（指引失效）"
        );

        mock.assert_async().await;
    }

    /// reset 端点返回 impl IntoResponse（Json<Value>），统一取体
    async fn reset_body(response: impl IntoResponse) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_response().into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

// ============ 精确响应缓存（oxcache，租户隔离键 + 确定性门控） ============

#[cfg(test)]
mod response_cache_tests {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::{chat_completions, response_cache::ResponseCache};
    use fluxgate::config::{
        CacheSection, Config, ConfigManager, ModelConfig, ServerConfig, VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 差异覆盖（其余字段继承 common 基座）：固定 id cached-openai、
    /// 名称 Cached OpenAI、模型 target-model→gpt-4、rps 100。
    fn make_vendor(api_base: String) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor("cached-openai", api_base);
        v.name = "Cached OpenAI".into();
        v.models = vec![ModelConfig {
            model_id: "target-model".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v.concurrency.requests_per_second = 100;
        v
    }

    /// [cache] 显式开启（enabled=true, ttl=300）——缓存行为测试的前提。
    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080。
    fn make_cache_enabled_config(api_base: String) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            ..common::test_config(vec![make_vendor(api_base)])
        }
    }

    async fn make_state(config: Config) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });

        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            affinity: None,
            response_cache: Some(Arc::new(ResponseCache::new(
                Arc::new(oxcache::backend::default_memory_backend()),
                config_manager.clone(),
                Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
            ))),
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    async fn post_chat(
        state: Arc<ApiState>,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        app(state)
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn body_json(response: &mut axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(std::mem::take(response).into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("JSON body")
    }

    fn header_of(response: &axum::response::Response, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    const UPSTREAM_BODY: &str = r#"{"id":"chatcmpl-1","object":"chat.completion","model":"target-model","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

    /// ① 相同 body 两次：上游仅 1 次（mockito expect(1) 锁定），第二次
    /// X-Cache: HIT 且响应 body 与首次相同。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn identical_body_served_from_cache_on_second_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(UPSTREAM_BODY)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0
        });
        // /admin/metrics 的 cache 块数据源：oxcache 全局记录器（本进程唯一
        // 的 oxcache Cache 即此响应缓存）。计数为进程级单调量，并行用例只会
        // 抬高基线，故按增量断言。
        let stats_before = oxcache::get_enhanced_stats();

        let mut first = post_chat(state.clone(), body.clone(), &[]).await;
        assert_eq!(first.status(), 200);
        assert_eq!(
            header_of(&first, "x-cache").as_deref(),
            Some("MISS"),
            "首次必为 MISS"
        );
        let first_body = body_json(&mut first).await;

        let mut second = post_chat(state, body, &[]).await;
        assert_eq!(second.status(), 200);
        assert_eq!(
            header_of(&second, "x-cache").as_deref(),
            Some("HIT"),
            "第二次必须命中缓存"
        );
        let second_body = body_json(&mut second).await;
        assert_eq!(first_body, second_body, "缓存命中响应必须与首次一致");

        mock.assert_async().await; // 上游恰好 1 次

        let stats_after = oxcache::get_enhanced_stats();
        assert!(
            stats_after.l1_misses > stats_before.l1_misses,
            "首次 MISS 必须计入全局 l1_misses（before={}，after={}）",
            stats_before.l1_misses,
            stats_after.l1_misses,
        );
        assert!(
            stats_after.l1_hits > stats_before.l1_hits,
            "第二次 HIT 必须计入全局 l1_hits（before={}，after={}）",
            stats_before.l1_hits,
            stats_after.l1_hits,
        );
    }

    /// ② temperature=0.7（采样语义）→ 两次都回源，不缓存。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sampled_requests_are_never_cached() {
        let mut server = mockito::Server::new_async().await;
        // 「两次都回源」由下方 X-Cache 头缺失 + 双 200 断言承载（采样请求
        // 不写缓存，200 只能来自上游）；mockito 计数只锁隐藏重试上界
        // （≤ 2 请求 × (1 + RetryableShort 2 次瞬态重试)）。精确 expect(2)
        // 在全量并行下会被 mockito 丢计数误伤（实测：两次 200 而计数 1/2）。
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(UPSTREAM_BODY)
            .expect_at_most(6)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0.7
        });

        let mut first = post_chat(state.clone(), body.clone(), &[]).await;
        assert_eq!(
            header_of(&first, "x-cache"),
            None,
            "采样请求不可缓存（Disabled 不参与，无 X-Cache 头）"
        );
        let _ = body_json(&mut first).await;

        let mut second = post_chat(state, body, &[]).await;
        assert_ne!(
            header_of(&second, "x-cache").as_deref(),
            Some("HIT"),
            "采样请求不得命中缓存"
        );
        let _ = body_json(&mut second).await;

        mock.assert_async().await; // 上游 2 次（两次都回源）
    }

    /// ③ 带 tools → 不缓存（工具调用请求有副作用语义）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tool_requests_are_never_cached() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(UPSTREAM_BODY)
            .expect(2)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}]
        });

        let mut first = post_chat(state.clone(), body.clone(), &[]).await;
        assert_ne!(
            header_of(&first, "x-cache").as_deref(),
            Some("MISS"),
            "不可缓存请求不参与（无头或 Disabled）"
        );
        let _ = body_json(&mut first).await;

        let mut second = post_chat(state, body, &[]).await;
        assert_ne!(header_of(&second, "x-cache").as_deref(), Some("HIT"));
        let _ = body_json(&mut second).await;

        mock.assert_async().await; // 上游 2 次
    }

    /// ④ stream=true → 不缓存（流式响应无法整体缓存）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_requests_are_never_cached() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body("data: [DONE]\n\n")
            .expect(2)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        });

        let first = post_chat(state.clone(), body.clone(), &[]).await;
        assert_eq!(
            header_of(&first, "x-cache"),
            None,
            "流式响应不得带 X-Cache 头"
        );

        let second = post_chat(state, body, &[]).await;
        assert_eq!(header_of(&second, "x-cache"), None);

        mock.assert_async().await; // 上游 2 次（两次都回源）
    }

    /// ⑤ X-No-Cache: 1 → 强制回源（Cloudflare 语义；跳过读与写）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn no_cache_header_forces_upstream() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(UPSTREAM_BODY)
            .expect(2)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0
        });

        // 第一次带 X-No-Cache：执行但不写缓存
        let mut first = post_chat(state.clone(), body.clone(), &[("X-No-Cache", "1")]).await;
        assert_eq!(
            header_of(&first, "x-cache"),
            None,
            "X-No-Cache 请求不参与缓存（无 X-Cache 头）"
        );
        let _ = body_json(&mut first).await;

        // 第二次无头：缓存仍空（第一次未写）→ MISS 回源
        let mut second = post_chat(state, body, &[]).await;
        assert_eq!(header_of(&second, "x-cache").as_deref(), Some("MISS"));
        let _ = body_json(&mut second).await;

        mock.assert_async().await; // 上游 2 次
    }

    /// ⑥ 不同 API key 同 body → 各自回源（租户隔离：key 哈希入键）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn different_api_keys_do_not_share_cache_entries() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(UPSTREAM_BODY)
            .expect(2)
            .create_async()
            .await;

        let state = make_state(make_cache_enabled_config(server.url())).await;
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "temperature": 0
        });

        let mut first = post_chat(
            state.clone(),
            body.clone(),
            &[("Authorization", "Bearer tenant-a-key")],
        )
        .await;
        assert_eq!(header_of(&first, "x-cache").as_deref(), Some("MISS"));
        let _ = body_json(&mut first).await;

        // 不同 key：不得命中 tenant-a 的缓存条目
        let mut second = post_chat(state, body, &[("Authorization", "Bearer tenant-b-key")]).await;
        assert_ne!(
            header_of(&second, "x-cache").as_deref(),
            Some("HIT"),
            "不同 API key 必须各自回源（租户隔离）"
        );
        let _ = body_json(&mut second).await;

        mock.assert_async().await; // 上游 2 次
    }
}

// ============ handler→奖励臂键全链 ============

#[cfg(test)]
mod linucb_reward_chain {
    //! handler→奖励臂键全链回归——
    //! 修复前奖励写原始 request.model（("GPT-4"/"auto", vendor) 与被咨询
    //! 臂 ("gpt-4"/"__global__", vendor) 永不相交），且 grep 全 tests/
    //! 无 record_linucb_reward 调用、handler→奖励链路零覆盖。

    use crate::common;
    use fluxgate::config::{Config, ConfigManager, RoutingSection, ServerConfig};
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::bandit::BanditState;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080、
    /// 路由选择 linucb、厂商名称 OpenAI Main、timeout 三项缩短到 2s。
    fn linucb_config() -> Config {
        common::ensure_test_api_key();
        // timeout 2s + 拒绝端口（127.0.0.1:1）：路由成功、执行快速失败
        // → 奖励 r=0 落账
        let mut v = common::base_vendor("openai_main", "http://127.0.0.1:1");
        v.name = "OpenAI Main".into();
        v.timeout_ms = 2_000;
        v.stream_ttft_ms = 2_000;
        v.stream_idle_ms = 2_000;
        // auth: None（继承基座）→ 放行（测试聚焦奖励链路而非认证）
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            routing: RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            ..common::test_config(vec![v])
        }
    }

    /// linucb 档 + bandit 接线的 ApiState。
    async fn create_linucb_state() -> (Arc<ApiState>, Arc<BanditState>) {
        let config = linucb_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });
        let bandit = Arc::new(BanditState::new(1.0));
        let router = Arc::new(
            fluxgate::router::Router::new(config_manager.clone(), vendor_manager.clone())
                .with_bandit(bandit.clone()),
        );
        let state = Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            logger: None,
            pre_aggregator: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            affinity: None,
            response_cache: None,
            bandit: Some(bandit.clone()),
            quota: None,
            difficulty: None,
            responses: None,
        });
        (state, bandit)
    }

    fn app(state: Arc<ApiState>) -> axum::Router {
        use axum::routing::post;
        axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(fluxgate::api::completions::chat_completions),
            )
            .with_state(state)
    }

    async fn post_chat(state: Arc<ApiState>, body: serde_json::Value) -> axum::response::Response {
        app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// 奖励臂键回归（handler→奖励全链）：1) 大写 model 奖励落归一键
    /// "gpt-4"；2) auto 路径奖励落 "__global__" 臂；3) 失败奖励（r=0）
    /// 压低被咨询臂 UCB
    #[tokio::test]
    async fn reward_lands_on_selection_arm_key() {
        let (state, bandit) = create_linucb_state().await;

        // 1) 大写 model 全链：路由成功（归一 gpt-4）→ 执行失败 → 奖励
        let resp = post_chat(
            state.clone(),
            serde_json::json!({"model": "GPT-4", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        assert_ne!(resp.status(), 401);
        assert!(
            bandit.has_arm("gpt-4", "openai_main"),
            "奖励必须落选择侧归一键 (gpt-4, openai_main)"
        );
        assert!(
            !bandit.has_arm("GPT-4", "openai_main"),
            "不得产生未归一的大写臂"
        );

        // 3) 失败奖励压低 UCB（先验 α=1 → ucb=1.0；r=0 后 <1.0）
        let x = [1.0, 0.0, 0.0, 0.0, 0.0];
        let after = bandit.ucb("gpt-4", "openai_main", &x, 1.0);
        assert!(after < 1.0, "失败奖励必须压低被咨询臂 UCB: {after}");

        // 2) auto 全链：global 路由（__global__ 臂）
        let resp = post_chat(
            state,
            serde_json::json!({"model": "auto", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        assert_ne!(resp.status(), 401);
        let consulted = bandit.has_arm("__global__", "openai_main")
            || bandit.has_arm("__global__", "anthropic_main");
        assert!(
            consulted,
            "auto 路径奖励必须落 __global__ 臂（与选择键对齐）"
        );
        assert!(
            !bandit.has_arm("auto", "openai_main") && !bandit.has_arm("auto", "anthropic_main"),
            "不得产生未归一的 auto 臂"
        );
    }
}

// ============ [global_model].enabled 开关：HTTP 面语义 ============

#[cfg(test)]
mod global_model_switch {
    //! E2E 场景「auto 且全局模型关闭」的 HTTP 层锁定：`[global_model].enabled=false`
    //! 时 `model="auto"` 必须以 400 `validation_error` 拒绝（错误信息显性说明
    //! 开关关闭），不得回落全局池；开启时 auto 照常选路（本用例 vendor 拒绝
    //! 端口，路由成功后执行失败 → 非 4xx 客户端错误）。

    use crate::common;
    use fluxgate::config::{Config, ConfigManager, GlobalModelConfig, ServerConfig};
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 单 vendor（拒绝端口 → 执行快速失败）+ 指定全局开关的 ApiState
    async fn create_state(global_enabled: bool) -> Arc<ApiState> {
        common::ensure_test_api_key();
        let mut v = common::base_vendor("openai_main", "http://127.0.0.1:1");
        v.name = "OpenAI Main".into();
        v.timeout_ms = 2_000;
        v.stream_ttft_ms = 2_000;
        v.stream_idle_ms = 2_000;
        let config = Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            global_model: Some(GlobalModelConfig {
                enabled: global_enabled,
                strategy: "weighted".into(),
            }),
            ..common::test_config(vec![v])
        };
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });
        Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(Router::new(config_manager, vendor_manager)),
            logger: None,
            pre_aggregator: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    async fn post_chat(state: Arc<ApiState>) -> axum::response::Response {
        use axum::routing::post;
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(fluxgate::api::completions::chat_completions),
            )
            .with_state(state);
        app.oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "model": "auto",
                        "messages": [{"role": "user", "content": "hi"}]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn auto_rejected_with_validation_error_when_global_disabled() {
        let resp = post_chat(create_state(false).await).await;
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "全局开关关闭时 auto 必须 400"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "validation_error");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("global_model"),
            "错误信息需显性说明全局开关关闭: {message}"
        );
    }

    #[tokio::test]
    async fn auto_routes_when_global_enabled() {
        let resp = post_chat(create_state(true).await).await;
        assert_ne!(
            resp.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "开关开启时 auto 必须进入全局池选路（执行失败另计，不得是 400）"
        );
    }
}

// ============ handler 层行覆盖补齐：错误路径 / 流式 / 缓存 / 亲和 / admin ============
//
// 针对 cargo-llvm-cov 实测的 src/api/* 未覆盖区间设计：每个测试让具体
// 分支真正走进去（cover_all_paths 换路、流式透传、缓存 HIT/MISS、
// 亲和 bind/rebind/clear、logger 停机降级、放大守门器、全候选限流、
// admin 查询端点等），断言状态码 / JSON 结构 / 头 / mock 调用次数等
// 可观测行为，而非"调用不报错"。
#[cfg(test)]
mod api_handler_coverage {
    use crate::common;
    use axum::extract::{Query, State};
    use axum::response::IntoResponse;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::admin::{self, LogQuery};
    use fluxgate::api::response_cache::{cache_api_key_hash, ResponseCache};
    use fluxgate::api::{
        chat_completions, embeddings, get_global_state, init_global_state, messages, ApiState,
    };
    use fluxgate::config::{
        CacheSection, Config, ConfigManager, GlobalModelConfig, ModelConfig, RateLimitConfig,
        ServerConfig, VendorConfig,
    };
    use fluxgate::database::{
        AsyncLogger, AsyncLoggerHandle, LogFilter, Repository, TokenPreAggregator,
    };
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::bandit::BanditState;
    use fluxgate::router::{AffinityTable, Router as NexusRouter};
    use fluxgate::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    /// ApiState 可选接线项（None = 不接线，覆盖降级分支）
    #[derive(Default)]
    struct Wire {
        logger: bool,
        pre_aggregator: bool,
        affinity: bool,
        response_cache: bool,
        bandit: bool,
    }

    /// 测试数据构造器：字段逐一对应 VendorConfig 的必填维度，
    /// 收敛为参数对象会让 39 个调用点更啰嗦，故豁免参数数上限。
    /// 差异覆盖（其余字段继承 common 基座）：名称 Vendor {id}、type_/
    /// priority/model_id/weight/max_tokens/cap 参数化、Token 配额
    /// max_tokens=0 时 window_hours=Some(1)。
    #[allow(clippy::too_many_arguments)]
    fn vendor(
        id: &str,
        type_: &str,
        api_base: String,
        priority: u8,
        model_id: &str,
        weight: f64,
        max_tokens: u64,
        cap: Option<usize>,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.name = format!("Vendor {id}");
        v.type_ = type_.into();
        v.priority = priority;
        v.models = vec![ModelConfig {
            model_id: model_id.into(),
            abstract_id: Some(model_id.into()),
            weight,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v.rate_limit = RateLimitConfig::Token {
            max_tokens,
            window_hours: if max_tokens == 0 { Some(1) } else { None },
            sliding: false,
        };
        v.concurrency.max_concurrent_requests = cap;
        v
    }

    /// 差异覆盖（其余字段继承 common 基座）：server 0.0.0.0:8080。
    fn base_config(vendors: Vec<VendorConfig>) -> Config {
        common::ensure_test_api_key();
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vendors)
        }
    }

    async fn build_state(config: Config, wire: Wire) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");

        let logger = wire.logger.then(|| {
            let sink: Arc<dyn fluxgate::storage::RequestLogSink> =
                Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
                    repository.clone(),
                    1024,
                    Duration::from_millis(200),
                )));
            sink
        });
        let pre_aggregator = wire.pre_aggregator.then(|| {
            let sink: Arc<dyn fluxgate::storage::UsageSink> = Arc::new(TokenPreAggregator::new(
                repository.clone(),
                Duration::from_secs(60),
            ));
            sink
        });
        let affinity = wire.affinity.then(|| {
            Arc::new(AffinityTable::with_memory_backend(Duration::from_secs(
                3600,
            )))
        });
        let response_cache = wire.response_cache.then(|| {
            Arc::new(ResponseCache::new(
                Arc::new(oxcache::backend::default_memory_backend()),
                config_manager.clone(),
                Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
            ))
        });
        let bandit = wire.bandit.then(|| Arc::new(BanditState::new(1.0)));

        let router = {
            let r = NexusRouter::new(config_manager.clone(), vendor_manager.clone());
            if let Some(b) = bandit.clone() {
                r.with_bandit(b)
            } else {
                r
            }
        };

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router: Arc::new(router),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(fluxgate::database::Database { pool, repository }),
            logger,
            pre_aggregator,
            affinity,
            response_cache,
            bandit,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages))
            .route("/v1/embeddings", post(embeddings))
            .with_state(state)
    }

    async fn post_json(
        state: Arc<ApiState>,
        uri: &str,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        app(state)
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("JSON body")
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    fn header_of(response: &axum::response::Response, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    fn chat_body(model: &str, stream: bool) -> serde_json::Value {
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 16
        });
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    fn anthropic_body(model: &str, stream: bool) -> serde_json::Value {
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 16
        });
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    const OPENAI_CHAT_JSON: &str = r#"{"id":"c1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;

    fn anthropic_message_json() -> String {
        r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","content":[{"type":"text","text":"hi there"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#
            .to_string()
    }

    /// 标准 Anthropic SSE 事件流（透传上游原样返回；转换上游经 IR 反解析）
    fn anthropic_sse() -> String {
        format!(
            "event: message_start\ndata: {}\n\ndata: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":3,"output_tokens":1}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        )
    }

    /// OpenAI 上游 SSE（转换路径入站，IR → Anthropic/OpenAI 出站重建）
    fn openai_sse() -> String {
        concat!(
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":" there"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "\n\n",
            "data: [DONE]\n\n",
        )
        .to_string()
    }

    /// OpenAI 上游 SSE（usage 尾帧：仅在 `stream_options.include_usage`
    /// 时由上游回传；转换出站自动索取）。
    fn openai_sse_with_usage() -> String {
        let base = openai_sse();
        let base = base
            .strip_suffix("data: [DONE]\n\n")
            .unwrap_or(base.as_str());
        format!(
            "{base}data: {{\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{{\"prompt_tokens\":7,\"completion_tokens\":4,\"total_tokens\":11,\"prompt_tokens_details\":{{\"cached_tokens\":2}}}}}}\n\ndata: [DONE]\n\n"
        )
    }

    // ---- anthropic.rs：透传流式（cover_all_paths 开/关 × 成功/失败）----

    /// 透传流式成功（cover_all_paths 关）：上游 SSE 帧原样转发给客户端，
    /// 响应为 text/event-stream 且保留上游事件内容。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_stream_success_forwards_upstream_frames() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(anthropic_sse())
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "anthropic_main",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let response =
            post_json(state, "/v1/messages", anthropic_body("claude-x", true), &[]).await;

        assert_eq!(response.status(), 200);
        assert_eq!(
            header_of(&response, "content-type").as_deref(),
            Some("text/event-stream"),
            "透传流式必须以 SSE 返回"
        );
        let text = body_text(response).await;
        assert!(
            text.contains("message_start"),
            "丢失 message_start: {text:?}"
        );
        assert!(
            text.contains("content_block_delta"),
            "丢失文本 delta: {text:?}"
        );
        assert!(text.contains("message_stop"), "丢失 message_stop: {text:?}");

        mock.assert_async().await;
    }

    /// 透传流式成功（cover_all_paths 开）：换路循环包裹下首派发即成功，
    /// 行为与关闭时逐位一致（SSE 帧原样转发）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_stream_cover_all_paths_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(anthropic_sse())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "anthropic_main",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response =
            post_json(state, "/v1/messages", anthropic_body("claude-x", true), &[]).await;

        assert_eq!(response.status(), 200);
        let text = body_text(response).await;
        assert!(text.contains("message_start") && text.contains("message_stop"));

        mock.assert_async().await;
    }

    /// 透传流式失败（cover_all_paths 开 + 放大守门器开但无失败观测）：
    /// 守门器在无观测时放行基础尝试预算（fail-open），单 vendor 换路无候选
    /// → 原错误冒泡为 503 JSON。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_stream_cover_all_paths_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "anthropic_main",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        config.retry.amplification_gate = true;
        let state = build_state(config, Wire::default()).await;

        let response =
            post_json(state, "/v1/messages", anthropic_body("claude-x", true), &[]).await;

        assert_eq!(
            response.status(),
            503,
            "上游 401 应映射 503（首字节前失败）"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    // ---- anthropic.rs：透传非流式缓存 HIT/MISS + cover_all_paths ----

    /// 透传非流式 + 响应缓存：首次 MISS（回写缓存并带 X-Cache: MISS），
    /// 第二次 HIT（上游仅 1 次，响应体与首次一致）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_nonstream_cache_miss_then_hit() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "anthropic_main",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.cache = CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        };
        let state = build_state(
            config,
            Wire {
                response_cache: true,
                ..Default::default()
            },
        )
        .await;

        let body = anthropic_body("claude-x", false);
        let first = post_json(state.clone(), "/v1/messages", body.clone(), &[]).await;
        assert_eq!(first.status(), 200);
        assert_eq!(
            header_of(&first, "x-cache").as_deref(),
            Some("MISS"),
            "首次必须 MISS 并回写缓存"
        );
        let first_body = body_json(first).await;
        assert_eq!(first_body["type"], "message");

        let second = post_json(state, "/v1/messages", body, &[]).await;
        assert_eq!(second.status(), 200);
        assert_eq!(
            header_of(&second, "x-cache").as_deref(),
            Some("HIT"),
            "第二次必须命中缓存"
        );
        let second_body = body_json(second).await;
        assert_eq!(first_body, second_body, "HIT 响应必须与首次一致");

        mock.assert_async().await;
    }

    /// cover_all_paths 开：主 vendor 失败（404，Fatal 不内部重试）→ 换路
    /// 至同协议健康 vendor 成功，客户端拿到 200。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_nonstream_cover_all_paths_reroutes_to_healthy_vendor() {
        let mut server = mockito::Server::new_async().await;
        let fail_mock = server
            .mock("POST", "/v1/messages")
            .with_status(404)
            .with_body("not found")
            .expect(1)
            .create_async()
            .await;
        let ok_mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![
            vendor(
                "v-anth-a",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                // 悬殊权重锁定初始候选为失败 vendor（/v1/messages 无
                // X-Vendor 语义），换路断言才确定
                1_000_000.0,
                1_000_000,
                None,
            ),
            vendor(
                "v-anth-b",
                "anthropic",
                server.url(),
                2,
                "claude-x",
                0.0001,
                1_000_000,
                None,
            ),
        ]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/messages",
            anthropic_body("claude-x", false),
            &[],
        )
        .await;

        assert_eq!(response.status(), 200, "换路后必须由健康 vendor 服务");
        let body = body_json(response).await;
        assert_eq!(body["type"], "message");

        fail_mock.assert_async().await;
        ok_mock.assert_async().await;
    }

    /// cover_all_paths 开 + 透传（same_protocol_only）：重路由命中异协议
    /// 候选时计入排除集不发起 HTTP；总尝试预算（max_total_attempts=2）在
    /// 排除循环内耗尽 → 原错误冒泡 503，openai 候选零触达。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_passthrough_cover_all_paths_skips_cross_protocol_candidate() {
        let mut server = mockito::Server::new_async().await;
        let fail_mock = server
            .mock("POST", "/v1/messages")
            .with_status(404)
            .with_body("not found")
            .expect(1)
            .create_async()
            .await;
        let mut other = mockito::Server::new_async().await;
        let untouched = other
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(
                r#"{"id":"sentinel-openai-b","object":"chat.completion","created":1,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"sentinel-b"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
            )
            .expect(0)
            .create_async()
            .await;

        let mut config = base_config(vec![
            vendor(
                "v-anth-a",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                // /v1/messages 不支持 X-Vendor 提示（无 hints 路由语义）：
                // 以悬殊 model 权重锁定初始候选（基础 queue/window 项对所
                // 有候选相同，0.4×1e6 ≫ 0.6，选中偏差 ~1e-6）
                1_000_000.0,
                1_000_000,
                None,
            ),
            vendor(
                "v-openai-b",
                "openai",
                other.url(),
                2,
                "claude-x",
                0.0001,
                1_000_000,
                None,
            ),
        ]);
        config.resilience.cover_all_paths = true;
        config.retry.max_total_attempts = 2;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/messages",
            anthropic_body("claude-x", false),
            &[],
        )
        .await;

        let status = response.status();
        let body = body_json(response).await;
        assert_eq!(
            status, 503,
            "预算在排除循环内耗尽应冒泡原错误, body: {body}"
        );
        assert_eq!(body["error"]["type"], "vendor_unavailable", "body: {body}");

        fail_mock.assert_async().await;
        untouched.assert_async().await;
    }

    // ---- anthropic.rs：转换路径（流式 cover_all_paths、非流式缓存/错误）----

    /// 转换流式成功（cover_all_paths 开）：OpenAI 上游 SSE 经 IR 重建为
    /// 完整 Anthropic 生命周期事件（message_start → … → message_stop）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_conversion_stream_cover_all_paths_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(openai_sse_with_usage())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(
            config,
            Wire {
                logger: true,
                pre_aggregator: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/messages",
            anthropic_body("gpt-4", true),
            &[],
        )
        .await;

        assert_eq!(response.status(), 200);
        assert_eq!(
            header_of(&response, "content-type").as_deref(),
            Some("text/event-stream")
        );
        let text = body_text(response).await;
        let non_empty: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(!non_empty.is_empty());
        for line in &non_empty {
            assert!(
                line.starts_with("data: "),
                "每个 SSE 事件行必须带 data: 前缀: {line:?}"
            );
        }
        assert!(text.contains("message_start"), "{text:?}");
        assert!(text.contains("message_stop"), "{text:?}");
        let aggregated: String = text
            .lines()
            .filter_map(|l| {
                let payload = l.strip_prefix("data: ")?;
                serde_json::from_str::<serde_json::Value>(payload)
                    .ok()?
                    .get("delta")?
                    .get("text")?
                    .as_str()
                    .map(String::from)
            })
            .collect();
        assert_eq!(aggregated, "Hi there", "文本 delta 必须完整重建: {text:?}");

        // 转换流用量记账：日志三列 + keyed 分账（含 cache 两档）。
        tokio::time::sleep(Duration::from_millis(400)).await;
        state
            .logger
            .as_ref()
            .expect("logger wired")
            .shutdown()
            .await
            .expect("shutdown");
        let logs = state
            .database
            .query_logs(&LogFilter::default())
            .await
            .expect("query logs");
        assert_eq!(logs.len(), 1, "流式请求必须落一条日志");
        assert_eq!(logs[0].prompt_tokens, Some(7));
        assert_eq!(logs[0].completion_tokens, Some(4));
        assert_eq!(logs[0].total_tokens, Some(11));
        let usage = state.pre_aggregator.as_ref().expect("pa wired");
        usage.flush().await.expect("flush");
        let stats = usage
            .get_token_stats(Some("v-openai"))
            .await
            .expect("token stats");
        assert_eq!(stats.len(), 1, "转换流必须写 keyed 分账");
        assert_eq!(stats[0].model, "gpt-4");
        assert_eq!(stats[0].prompt_tokens, 7);
        assert_eq!(stats[0].completion_tokens, 4);
        assert_eq!(stats[0].cache_read_tokens, 2, "cached tokens 必须入分账");
        assert_eq!(stats[0].request_count, 1, "不得重复计数");

        mock.assert_async().await;
    }

    /// 转换流式失败（cover_all_paths 开）：上游 401 在响应头前失败 →
    /// 503 JSON（非 200 伪流）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_conversion_stream_cover_all_paths_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(state, "/v1/messages", anthropic_body("gpt-4", true), &[]).await;

        assert_eq!(response.status(), 503);
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    /// 转换非流式成功（缓存未接线）：OpenAI 上游响应经 UnifiedResponse
    /// 转回 Anthropic 出站格式，且不带 X-Cache 头。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_conversion_nonstream_success_without_cache() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let response = post_json(state, "/v1/messages", anthropic_body("gpt-4", false), &[]).await;

        assert_eq!(response.status(), 200);
        assert_eq!(header_of(&response, "x-cache"), None, "缓存未接线不带头");
        let body = body_json(response).await;
        assert_eq!(body["type"], "message", "body: {body}");
        assert_eq!(body["role"], "assistant", "body: {body}");
        assert_eq!(
            body["content"]["content"][0]["text"], "hi there",
            "body: {body}"
        );
        assert_eq!(body["usage"]["input_tokens"], 3, "body: {body}");
        assert_eq!(body["usage"]["output_tokens"], 2, "body: {body}");

        mock.assert_async().await;
    }

    /// 转换非流式 + 响应缓存：首次 MISS（以最终 vendor/model 回写），
    /// 第二次 HIT（上游仅 1 次）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_conversion_nonstream_cache_miss_then_hit() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.cache = CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        };
        let state = build_state(
            config,
            Wire {
                response_cache: true,
                ..Default::default()
            },
        )
        .await;

        let body = anthropic_body("gpt-4", false);
        let first = post_json(state.clone(), "/v1/messages", body.clone(), &[]).await;
        assert_eq!(first.status(), 200);
        assert_eq!(header_of(&first, "x-cache").as_deref(), Some("MISS"));
        let first_body = body_json(first).await;
        assert_eq!(first_body["type"], "message");

        let second = post_json(state, "/v1/messages", body, &[]).await;
        assert_eq!(second.status(), 200);
        assert_eq!(
            header_of(&second, "x-cache").as_deref(),
            Some("HIT"),
            "转换路径回写后第二次必须 HIT"
        );
        let second_body = body_json(second).await;
        assert_eq!(first_body, second_body);

        mock.assert_async().await;
    }

    /// 转换非流式失败：上游 401 → 换路无候选 → 503 JSON（口径）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_conversion_nonstream_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let response = post_json(state, "/v1/messages", anthropic_body("gpt-4", false), &[]).await;

        assert_eq!(response.status(), 503);
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    /// linucb 档 + /v1/messages：请求特征参与选择、奖励落归一臂键
    ///（body.model 小写），成功请求后臂 (claude-x, vendor) 必然存在。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn anthropic_messages_linucb_features_recorded_on_reward_arm() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "anthropic_main",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.routing.selection = "linucb".into();
        let state = build_state(
            config,
            Wire {
                bandit: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/messages",
            anthropic_body("claude-x", false),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        assert_eq!(body["type"], "message");

        assert!(
            state
                .bandit
                .as_ref()
                .unwrap()
                .has_arm("claude-x", "anthropic_main"),
            "linucb 档成功请求必须落归一臂键 (claude-x, anthropic_main)"
        );

        mock.assert_async().await;
    }

    /// logger 已 shutdown：log push 被拒仅记 warn，响应路径不受影响。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_passthrough_with_shutdown_logger_still_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "anthropic_main",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                ..Default::default()
            },
        )
        .await;
        state
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");

        let response = post_json(
            state,
            "/v1/messages",
            anthropic_body("claude-x", false),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200, "日志 push 失败不得影响响应");
        let body = body_json(response).await;
        assert_eq!(body["type"], "message");

        mock.assert_async().await;
    }

    // ---- completions.rs：转换流式（anthropic vendor）、cover_all_paths、缓存 ----

    /// 转换流式成功（cover_all_paths 关）：Anthropic 上游 SSE 经 IR 重建为
    /// OpenAI chunk 事件并以 [DONE] 收尾，文本完整。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn openai_conversion_stream_anthropic_vendor_success_emits_chunks_and_done() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(anthropic_sse())
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-anthropic",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                pre_aggregator: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("claude-x", true),
            &[],
        )
        .await;

        assert_eq!(response.status(), 200);
        assert_eq!(
            header_of(&response, "content-type").as_deref(),
            Some("text/event-stream")
        );
        let text = body_text(response).await;
        assert!(
            text.contains("chat.completion.chunk"),
            "出站必须是 OpenAI chunk 事件: {text:?}"
        );
        assert!(
            text.contains("[DONE]"),
            "OpenAI 流必须以 [DONE] 收尾: {text:?}"
        );
        let aggregated: String = text
            .lines()
            .filter_map(|l| {
                let payload = l.strip_prefix("data: ")?;
                let v: serde_json::Value = serde_json::from_str(payload).ok()?;
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(String::from)
            })
            .collect();
        assert_eq!(
            aggregated, "hi",
            "文本 delta 必须重建自 Anthropic 流: {text:?}"
        );

        // 转换流用量记账：日志三列 + keyed 分账（分账 model = 上游 model_id）。
        tokio::time::sleep(Duration::from_millis(400)).await;
        state
            .logger
            .as_ref()
            .expect("logger wired")
            .shutdown()
            .await
            .expect("shutdown");
        let logs = state
            .database
            .query_logs(&LogFilter::default())
            .await
            .expect("query logs");
        assert_eq!(logs.len(), 1, "流式请求必须落一条日志");
        assert_eq!(logs[0].prompt_tokens, Some(3));
        assert_eq!(logs[0].completion_tokens, Some(2));
        assert_eq!(logs[0].total_tokens, Some(5));
        assert!(
            logs[0]
                .response
                .as_deref()
                .unwrap_or("")
                .contains("\"prompt_tokens\":3"),
            "response 列必须补写 usage: {:?}",
            logs[0].response
        );
        let usage = state.pre_aggregator.as_ref().expect("pa wired");
        usage.flush().await.expect("flush");
        let stats = usage
            .get_token_stats(Some("v-anthropic"))
            .await
            .expect("token stats");
        assert_eq!(stats.len(), 1, "转换流必须写 keyed 分账");
        assert_eq!(stats[0].model, "claude-x", "分账 model 取上游 model_id");
        assert_eq!(stats[0].prompt_tokens, 3);
        assert_eq!(stats[0].completion_tokens, 2);
        assert_eq!(stats[0].request_count, 1, "不得重复计数");

        mock.assert_async().await;
    }

    /// 转换流式成功（cover_all_paths 开）：换路循环包裹下首派发即成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn openai_conversion_stream_cover_all_paths_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(anthropic_sse())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-anthropic",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("claude-x", true),
            &[],
        )
        .await;

        assert_eq!(response.status(), 200);
        let text = body_text(response).await;
        assert!(text.contains("chat.completion.chunk") && text.contains("[DONE]"));

        mock.assert_async().await;
    }

    /// 转换流式失败（cover_all_paths 开）：上游 429 → 客户端 429
    ///（RateLimited 固定映射，可退避重试）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn openai_conversion_stream_cover_all_paths_upstream_429_yields_429() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_body(r#"{"error":{"message":"rate limited"}}"#)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-anthropic",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("claude-x", true),
            &[],
        )
        .await;

        assert_eq!(response.status(), 429);
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "rate_limited");

        mock.assert_async().await;
    }

    /// 透传流式失败（cover_all_paths 开）：上游 401 → 503 JSON（非 200 伪流）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn openai_passthrough_stream_cover_all_paths_upstream_401_yields_503() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        let state = build_state(config, Wire::default()).await;

        let response =
            post_json(state, "/v1/chat/completions", chat_body("gpt-4", true), &[]).await;

        assert_eq!(response.status(), 503);
        assert_ne!(
            header_of(&response, "content-type").as_deref(),
            Some("text/event-stream"),
            "错误响应不得是 SSE 流"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    /// 转换非流式 + 响应缓存（anthropic vendor）：首次 MISS 回写，
    /// 第二次 HIT（上游仅 1 次，租户键隔离由 body/key 维度承载）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn openai_conversion_nonstream_cache_miss_then_hit() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-anthropic",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.cache = CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        };
        let state = build_state(
            config,
            Wire {
                response_cache: true,
                ..Default::default()
            },
        )
        .await;

        let body = chat_body("claude-x", false);
        let first = post_json(state.clone(), "/v1/chat/completions", body.clone(), &[]).await;
        assert_eq!(first.status(), 200);
        assert_eq!(header_of(&first, "x-cache").as_deref(), Some("MISS"));
        let first_body = body_json(first).await;
        assert_eq!(
            first_body["choices"][0]["message"]["content"], "hi there",
            "body: {first_body}"
        );

        let second = post_json(state, "/v1/chat/completions", body, &[]).await;
        assert_eq!(second.status(), 200);
        assert_eq!(header_of(&second, "x-cache").as_deref(), Some("HIT"));
        assert_eq!(body_json(second).await, first_body);

        mock.assert_async().await;
    }

    /// 转换非流式：上游 200 但返回 error 体 → 解析为 Protocol 错误 →
    /// 换路无候选原错误冒泡 → 客户端 502（protocol_error）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_conversion_nonstream_upstream_error_body_yields_502() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"message":"overloaded","type":"api_error"}}"#)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-anthropic",
                "anthropic",
                server.url(),
                1,
                "claude-x",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("claude-x", false),
            &[],
        )
        .await;

        assert_eq!(response.status(), 502, "上游 error 体映射 Protocol → 502");
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "protocol_error");

        mock.assert_async().await;
    }

    /// 转换非流式换路：主 vendor 404 → route_excluding 重派健康 vendor
    /// 成功（200），两个上游各恰一次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_conversion_nonstream_reroutes_to_healthy_vendor() {
        let mut server = mockito::Server::new_async().await;
        let fail_mock = server
            .mock("POST", "/v1/messages")
            .with_status(404)
            .with_body("not found")
            .expect(1)
            .create_async()
            .await;
        let ok_mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_message_json())
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![
                vendor(
                    "v-anth-a",
                    "anthropic",
                    server.url(),
                    1,
                    "claude-x",
                    1.0,
                    1_000_000,
                    None,
                ),
                vendor(
                    "v-anth-b",
                    "anthropic",
                    server.url(),
                    2,
                    "claude-x",
                    1.0,
                    1_000_000,
                    None,
                ),
            ]),
            Wire::default(),
        )
        .await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("claude-x", false),
            // X-Vendor 锁定初始候选为失败 vendor，换路断言才确定
            &[("x-vendor", "v-anth-a")],
        )
        .await;
        assert_eq!(response.status(), 200, "换路后必须由健康 vendor 服务");
        let body = body_json(response).await;
        assert_eq!(
            body["choices"][0]["message"]["content"], "hi there",
            "body: {body}"
        );

        fail_mock.assert_async().await;
        ok_mock.assert_async().await;
    }

    /// 跨 vendor 时间预算：首 vendor 慢失败（1.5s > 1s 预算）→ 重派前
    /// 放弃，原错误冒泡 503，且上游仅 1 次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_conversion_nonstream_retry_budget_exhausted_aborts_before_reroute() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(404)
            // mockito 1.x 无 with_delay：分块写前睡 1.5s 模拟慢失败上游
            .with_chunked_body(|w| {
                std::thread::sleep(Duration::from_millis(1500));
                std::io::Write::write_all(w, b"not found")
            })
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-anth-slow",
            "anthropic",
            server.url(),
            1,
            "claude-x",
            1.0,
            1_000_000,
            None,
        )]);
        config.retry.retry_budget_secs = 1;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("claude-x", false),
            &[],
        )
        .await;

        assert_eq!(response.status(), 503, "预算耗尽应以原错误（503）冒泡");
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    /// 透传路径（cover_all_paths 开）的跨 vendor 时间预算：慢失败上游
    /// （1.5s > 1s 预算）→ 换路循环内预算超限即放弃，原错误冒泡 503，
    /// 上游仅 1 次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_passthrough_retry_budget_exhausted_aborts_before_reroute() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(404)
            // mockito 1.x 无 with_delay：分块写前睡 1.5s 模拟慢失败上游
            .with_chunked_body(|w| {
                std::thread::sleep(Duration::from_millis(1500));
                std::io::Write::write_all(w, b"not found")
            })
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai-slow",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.resilience.cover_all_paths = true;
        config.retry.retry_budget_secs = 1;
        let state = build_state(config, Wire::default()).await;

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[],
        )
        .await;

        assert_eq!(response.status(), 503, "预算耗尽应以原错误（503）冒泡");
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock.assert_async().await;
    }

    /// logger 已 shutdown：chat 成功响应不受日志 push 被拒影响（warn 降级）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_chat_with_shutdown_logger_still_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                ..Default::default()
            },
        )
        .await;
        state
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");

        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        assert_eq!(body["choices"][0]["message"]["content"], "hi there");

        mock.assert_async().await;
    }

    // ---- completions.rs：embeddings（预聚合记账 + Protocol 门 + logger 降级）----

    /// embeddings 成功且 usage 三元齐全：预聚合器记账（flush 后 token_stats
    /// 非空）；logger 已 shutdown 时 warn 降级不影响响应。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_full_usage_feeds_pre_aggregator_and_tolerates_shutdown_logger() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1]}],"model":"text-embedding-3-small","usage":{"prompt_tokens":3,"completion_tokens":0,"total_tokens":3}}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-embed",
                "openai",
                server.url(),
                1,
                "text-embedding-3-small",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                pre_aggregator: true,
                ..Default::default()
            },
        )
        .await;
        state
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");

        let response = post_json(
            state.clone(),
            "/v1/embeddings",
            json!({"model": "text-embedding-3-small", "input": "hello"}),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        assert_eq!(body["object"], "list");

        let usage_front = state.pre_aggregator.as_ref().unwrap();
        usage_front.flush().await.expect("flush");
        let stats = usage_front
            .get_token_stats(Some("v-embed"))
            .await
            .expect("token stats");
        assert_eq!(stats.len(), 1, "usage 三元齐全必须预聚合记账");
        assert_eq!(stats[0].total_tokens, 3);
        assert_eq!(stats[0].request_count, 1);

        mock.assert_async().await;
    }

    /// embeddings 仅支持 openai vendor：anthropic 型候选 → Protocol 错误
    ///（502 protocol_error），不发上游请求。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embeddings_non_openai_vendor_yields_502_protocol_error() {
        let state = build_state(
            base_config(vec![vendor(
                "v-anthropic",
                "anthropic",
                String::new(),
                1,
                "text-embedding-3-small",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let response = post_json(
            state,
            "/v1/embeddings",
            json!({"model": "text-embedding-3-small", "input": "hello"}),
            &[],
        )
        .await;

        assert_eq!(response.status(), 502);
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "protocol_error");
    }

    // ---- 放大守门器（gate_adjusted_max_attempts 经 cover_all_paths 入口）----

    /// gate=true + 超阈观测（r=1, f=1）：本请求尝试预算收紧到 1 —— 首次
    /// 失败即放弃，健康 vendor 零触达；发散（r·f>0.95）与收紧 warn 同时
    /// 命中。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn amplification_gate_tightens_dispatch_to_single_attempt() {
        let mut server = mockito::Server::new_async().await;
        let fail_mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(404)
            .with_body("not found")
            .expect(1)
            .create_async()
            .await;
        let mut other = mockito::Server::new_async().await;
        let untouched = other
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(OPENAI_CHAT_JSON)
            .expect(0)
            .create_async()
            .await;

        let mut config = base_config(vec![
            vendor(
                "v-a",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                Some(10),
            ),
            vendor(
                "v-b",
                "openai",
                other.url(),
                2,
                "gpt-4",
                1.0,
                1_000_000,
                Some(10),
            ),
        ]);
        config.resilience.cover_all_paths = true;
        config.retry.amplification_gate = true;
        let state = build_state(config, Wire::default()).await;

        // 灌超阈观测：6/6 全失败（f=1.0）、r=1.0（r·f=1.0 发散）、C=20
        let m = state.vendor_manager.metrics();
        for _ in 0..6 {
            m.record_request("v-a", false, Duration::from_millis(1));
        }
        for _ in 0..4 {
            m.record_retry_attempt();
            m.record_failed_request_before_retry();
        }

        // X-Vendor 锁定初始候选为 v-a，收紧语义才可确定性断言
        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[("x-vendor", "v-a")],
        )
        .await;

        assert_eq!(response.status(), 503, "收紧后首次失败即放弃");
        fail_mock.assert_async().await;
        untouched.assert_async().await;
    }

    /// gate=true + 健康观测（f≈0）：守门器不收紧，换路照常发生 → 200。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn amplification_gate_with_healthy_metrics_keeps_fallback_attempts() {
        let mut server = mockito::Server::new_async().await;
        let fail_mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(404)
            .with_body("not found")
            .expect(1)
            .create_async()
            .await;
        let ok_mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![
            vendor(
                "v-a",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                Some(10),
            ),
            vendor(
                "v-b",
                "openai",
                server.url(),
                2,
                "gpt-4",
                1.0,
                1_000_000,
                Some(10),
            ),
        ]);
        config.resilience.cover_all_paths = true;
        config.retry.amplification_gate = true;
        let state = build_state(config, Wire::default()).await;

        // 有失败观测（view Some）但失败率≈0：不收紧、不发散 → 保持基础预算
        let m = state.vendor_manager.metrics();
        for _ in 0..20 {
            m.record_request("v-a", true, Duration::from_millis(1));
        }
        m.record_retry_attempt();
        m.record_failed_request_before_retry();

        // X-Vendor 锁定初始候选为 v-a，"失败后换路 v-b" 才可确定性断言
        let response = post_json(
            state,
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[("x-vendor", "v-a")],
        )
        .await;

        assert_eq!(response.status(), 200, "健康观测不得收紧：换路必须可达 v-b");
        fail_mock.assert_async().await;
        ok_mock.assert_async().await;
    }

    // ---- 全候选限流（handler 入口 → AllVendorsThrottled 429 + Retry-After）----

    /// 双 vendor 均限流（max_tokens=0）+ fail-open + retry_after_hint：
    /// 转换路径（anthropic 型 vendor）首路由 fail-open 进入 fallback 循环，
    /// 耗尽后收敛 AllVendorsThrottled → 429 + Retry-After 头；最终失败
    /// 解除亲和绑定。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn all_vendors_throttled_handler_yields_429_with_retry_after() {
        let mut config = base_config(vec![
            vendor(
                "v-throttled-a",
                "anthropic",
                String::new(),
                1,
                "gpt-4",
                1.0,
                0,
                None,
            ),
            vendor(
                "v-throttled-b",
                "anthropic",
                String::new(),
                2,
                "gpt-4",
                1.0,
                0,
                None,
            ),
        ]);
        config.routing.retry_after_hint = true;
        config.routing.fail_open_when_all_cooling = true;
        let state = build_state(
            config,
            Wire {
                affinity: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[],
        )
        .await;

        assert_eq!(response.status(), 429, "全候选限流必须 429");
        assert!(
            header_of(&response, "retry-after").is_some(),
            "retry_after_hint 开启时必须带 Retry-After 头"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["type"], "all_vendors_throttled");

        // 最终失败必须解除亲和绑定（messages 兜底会话键）
        let session_key =
            fluxgate::api::routing_hints::fallback_session_key(&chat_body("gpt-4", false))
                .expect("messages 兜底键");
        let affinity = state.affinity.as_ref().unwrap();
        assert!(
            affinity.lookup(None, &session_key).await.is_none(),
            "全候选限流失败后亲和绑定必须被解除"
        );
    }

    // ---- 会话亲和 bind/rebind（mod.rs 亲和读写体）----

    /// 流式派发成功回写亲和绑定（tenant_scoped 开）：键带租户维度，
    /// lookup(Some(tenant), session) 命中派发 vendor。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn session_affinity_binds_tenant_scoped_on_stream_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: [DONE]\n\n")
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.routing.affinity_tenant_scoped = true;
        let state = build_state(
            config,
            Wire {
                affinity: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("gpt-4", true),
            &[("x-session-id", "sess-42")],
        )
        .await;
        assert_eq!(response.status(), 200);

        let tenant = cache_api_key_hash(&axum::http::HeaderMap::new());
        let bound = state
            .affinity
            .as_ref()
            .unwrap()
            .lookup(Some(&tenant), "sess-42")
            .await;
        assert_eq!(
            bound.as_deref(),
            Some("v-openai"),
            "流式派发成功必须按租户维度回写亲和绑定"
        );

        mock.assert_async().await;
    }

    /// 流式派发成功回写亲和绑定（tenant_scoped 关）：legacy 键
    /// lookup(None, session) 命中。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn session_affinity_binds_legacy_key_on_stream_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: [DONE]\n\n")
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                affinity: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("gpt-4", true),
            &[("x-session-id", "sess-legacy")],
        )
        .await;
        assert_eq!(response.status(), 200);

        let bound = state
            .affinity
            .as_ref()
            .unwrap()
            .lookup(None, "sess-legacy")
            .await;
        assert_eq!(
            bound.as_deref(),
            Some("v-openai"),
            "legacy 键（无租户维度）必须命中绑定"
        );

        mock.assert_async().await;
    }

    /// 非流式成功回写亲和（迁移重绑定路径）：tenant_scoped 开时以
    /// 无条件 set 语义写入租户维度键。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_affinity_rebinds_with_tenant_on_nonstream_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            server.url(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.routing.affinity_tenant_scoped = true;
        let state = build_state(
            config,
            Wire {
                affinity: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[("x-session-id", "sess-nons")],
        )
        .await;
        assert_eq!(response.status(), 200);

        let tenant = cache_api_key_hash(&axum::http::HeaderMap::new());
        let bound = state
            .affinity
            .as_ref()
            .unwrap()
            .lookup(Some(&tenant), "sess-nons")
            .await;
        assert_eq!(
            bound.as_deref(),
            Some("v-openai"),
            "非流式成功必须经重绑定回写租户维度亲和键"
        );

        mock.assert_async().await;
    }

    // ---- 大 payload 截断 / ApiState Debug / LinUCB 奖励守卫 / 全局状态 ----

    /// >64KB 的 messages 落库截断：按 UTF-8 字符边界截断并附截断标记
    ///（CJK 内容使 64KB 边界落在多字节字符内部，覆盖回退循环）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_messages_truncated_at_char_boundary_in_request_log() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                ..Default::default()
            },
        )
        .await;

        // 3 字节/字符 × 30000 = 90KB：64KiB 边界落在字符内部（65536 % 3 = 1）
        let huge = "中".repeat(30_000);
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": huge}],
            "max_tokens": 16
        });
        let response = post_json(state.clone(), "/v1/chat/completions", body, &[]).await;
        assert_eq!(response.status(), 200);

        state
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");

        // flush 完成时点受并行测试负载影响（固定 sleep 在高负载下会先于
        // 落库断言）——改为有界轮询：正常路径首查即得，超时上限远宽于
        // 正常 flush 耗时。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let logs = loop {
            let logs = state
                .database
                .query_logs(&LogFilter::default())
                .await
                .expect("query logs");
            if !logs.is_empty() || std::time::Instant::now() >= deadline {
                break logs;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(logs.len(), 1);
        let messages = &logs[0].messages;
        assert!(
            messages.len() <= 64 * 1024 + 200,
            "落库 messages 必须被截断到 64KB 附近，实际 {} 字节",
            messages.len()
        );
        assert!(
            messages.ends_with(']'),
            "截断值必须以截断标记收尾: …{:?}",
            &messages[messages.len() - 60..]
        );

        mock.assert_async().await;
    }

    /// ApiState Debug 不泄露内部细节，仅渲染接线摘要（字段存在性布尔）。
    #[tokio::test]
    async fn api_state_debug_renders_wiring_summary_only() {
        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                bandit: true,
                ..Default::default()
            },
        )
        .await;

        let debug = format!("{state:?}");
        assert!(debug.starts_with("ApiState"), "{debug}");
        for field in [
            "config",
            "vendor_manager",
            "router",
            "database",
            "logger: true",
            "pre_aggregator: false",
            "affinity: false",
            "response_cache: false",
            "bandit: true",
        ] {
            assert!(debug.contains(field), "Debug 摘要缺字段 {field}: {debug}");
        }
    }

    /// record_linucb_reward 双守卫：weighted 档不记账；linucb 档但臂键
    /// 缺失（直连路径）不产生伪样本。
    #[tokio::test]
    async fn record_linucb_reward_guards_skip_accounting() {
        let request = json!({"model": "gpt-4", "messages": []});

        // weighted 档：bandit 已接线但选择档不是 linucb → 零记账
        let weighted_state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                bandit: true,
                ..Default::default()
            },
        )
        .await;
        weighted_state.record_linucb_reward(Some("gpt-4"), "v-openai", &request, true);
        assert!(
            !weighted_state
                .bandit
                .as_ref()
                .unwrap()
                .has_arm("gpt-4", "v-openai"),
            "weighted 档不得记账"
        );

        // linucb 档 + 臂键缺失（直连路径）→ 不产生伪样本
        let mut linucb_config = base_config(vec![vendor(
            "v-openai",
            "openai",
            String::new(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        linucb_config.routing.selection = "linucb".into();
        let linucb_state = build_state(
            linucb_config,
            Wire {
                bandit: true,
                ..Default::default()
            },
        )
        .await;
        linucb_state.record_linucb_reward(None, "v-openai", &request, true);
        assert!(
            !linucb_state
                .bandit
                .as_ref()
                .unwrap()
                .has_arm("gpt-4", "v-openai"),
            "臂未被咨询（直连）不得产生伪样本"
        );
    }

    /// 全局状态：init 后 get 返回同一实例（sdforge handler 的取态通道）。
    ///
    /// ⚠ OnceCell 独占：`init_global_state` 二次调用即 panic，本测试把
    /// 进程级全局态永久绑定到自己的 ApiState——本二进制内不得再有其他
    /// 测试调用 init_global_state / get_global_state（与 server_run_*.rs
    /// "一场景独占一二进制"同一约束，只是本文件的其余测试不消费全局态）。
    #[tokio::test]
    async fn global_state_init_then_get_returns_same_instance() {
        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;
        init_global_state(state.clone());
        assert!(
            Arc::ptr_eq(&get_global_state(), &state),
            "get_global_state 必须返回 init 注入的同一实例"
        );
    }

    // ---- admin 查询与配置端点 ----
    // （既有 admin 端点测试在上方 `mod admin`；本段是覆盖率补缺新增的
    // 查询/更新分支，找 admin 测试时两处都要看。）

    /// logs / search_logs / token_stats 三查询端点：以一次真实成功请求的
    /// 落库数据驱动，断言各自返回结构与过滤语义。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_log_query_endpoints_return_recent_activity() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_CHAT_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                server.url(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire {
                logger: true,
                pre_aggregator: true,
                ..Default::default()
            },
        )
        .await;

        let response = post_json(
            state.clone(),
            "/v1/chat/completions",
            chat_body("gpt-4", false),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200);

        state
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;
        state
            .pre_aggregator
            .as_ref()
            .unwrap()
            .flush()
            .await
            .expect("flush");

        // logs：无条件查询返回该请求，状态码/端点为真实值
        let value = admin::logs(
            State(state.clone()),
            Query(LogQuery {
                start_time: None,
                end_time: None,
                vendor: None,
                model: None,
                limit: None,
            }),
        )
        .await
        .expect("logs query")
        .0;
        let arr = value.as_array().expect("logs 必须是数组");
        assert_eq!(arr.len(), 1, "必须返回刚落库的一条日志");
        assert_eq!(arr[0]["endpoint"], "/v1/chat/completions");
        assert_eq!(arr[0]["status_code"], 200);
        assert_eq!(arr[0]["actual_vendor"], "v-openai");

        // search_logs：keyword 命中 messages 中的 "hello"
        let value = admin::search_logs(
            State(state.clone()),
            Query(LogQuery {
                start_time: None,
                end_time: None,
                vendor: Some("hello".into()),
                model: None,
                limit: None,
            }),
        )
        .await
        .expect("search query")
        .0;
        assert_eq!(
            value.as_array().unwrap().len(),
            1,
            "关键词必须命中 messages"
        );

        // token_stats：预聚合 flush 后按 vendor 可查
        let value = admin::token_stats(
            State(state.clone()),
            Query(LogQuery {
                start_time: None,
                end_time: None,
                vendor: Some("v-openai".into()),
                model: None,
                limit: None,
            }),
        )
        .await
        .expect("token stats")
        .0;
        let stats = value.as_array().expect("token_stats 必须是数组");
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0]["vendor"], "v-openai");
        assert_eq!(stats[0]["total_tokens"], 5);

        mock.assert_async().await;
    }

    /// update_config 变更建站期固化段（server）→ 响应显式声明需重启。
    #[tokio::test]
    async fn admin_update_config_reports_restart_required_for_server() {
        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        let mut next = state.config.get_config();
        next.server.port = 9090;
        let response = admin::update_config(State(state.clone()), axum::Json(next))
            .await
            .unwrap()
            .into_response();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], true);
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "server"),
            "server 变更必须提示需重启: {required:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// circuit_open_timeout_ms 变更走"仅新建实例生效"第三类语义（实例
    /// 按 vendor 惰性创建后进程级缓存）：响应必须经 applies_to_new_instances
    /// 如实 surfaced，不得并入 restart_required（该段其余键热生效）也不得
    /// 静默返回 immediate。
    #[tokio::test]
    async fn admin_update_config_surfaces_circuit_window_as_new_instance_scoped() {
        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        let mut next = state.config.get_config();
        next.resilience.circuit_open_timeout_ms = 5000;
        let response = admin::update_config(State(state.clone()), axum::Json(next))
            .await
            .unwrap()
            .into_response();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let scoped = value["applies_to_new_instances"].as_array().unwrap();
        assert!(
            scoped
                .iter()
                .any(|v| v == "resilience.circuit_open_timeout_ms"),
            "CB 窗口变更必须提示仅新建实例生效: {scoped:?}"
        );
        let required = value["restart_required"].as_array().unwrap();
        assert!(
            !required.iter().any(|v| v == "resilience"),
            "CB 窗口不是重启级语义，不得混入 restart_required: {required:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// get_models：global_model 未配置时返回空数组（非错误）。
    #[tokio::test]
    async fn admin_get_models_empty_without_global_model() {
        let mut config = base_config(vec![vendor(
            "v-openai",
            "openai",
            String::new(),
            1,
            "gpt-4",
            1.0,
            1_000_000,
            None,
        )]);
        config.global_model = None;
        let state = build_state(config, Wire::default()).await;

        let models = admin::get_models(State(state)).await.unwrap().0;
        assert!(
            models.is_empty(),
            "无 global_model 配置必须返回空数组: {models:?}"
        );
    }

    /// update_model：id == "global" 更新单例全局模型策略并即时可读。
    #[tokio::test]
    async fn admin_update_model_global_updates_strategy() {
        let state = build_state(
            base_config(vec![vendor(
                "v-openai",
                "openai",
                String::new(),
                1,
                "gpt-4",
                1.0,
                1_000_000,
                None,
            )]),
            Wire::default(),
        )
        .await;

        // update_config_with 原子 RMW 需要路径记忆才落盘
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        state.config.set_path(path.clone());

        let response = admin::update_model(
            State(state.clone()),
            axum::extract::Path("global".into()),
            axum::Json(GlobalModelConfig {
                enabled: true,
                strategy: "linucb".into(),
            }),
        )
        .await
        .unwrap()
        .into_response();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], true);

        // GET /admin/models 响应面为目录条目（global 策略读 config 面）
        assert!(
            admin::get_models(State(state.clone()))
                .await
                .unwrap()
                .0
                .is_empty(),
            "目录列表与 global 策略段独立"
        );
        assert_eq!(
            state.config.get_config().global_model.unwrap().strategy,
            "linucb",
            "更新必须即时可读"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 数据面端点：GET /v1/models 与
/// POST /v1/messages/count_tokens。走 `create_router` 全中间件栈
/// （认证/限流与线上同形），覆盖成功/未鉴权/参数错误三路径 + 去重 +
/// 不缓存。
mod data_plane_endpoints {
    use crate::common;
    use axum::{body::Body, http::Request, http::StatusCode};
    use fluxgate::api::{create_router, response_cache::ResponseCache};
    use fluxgate::config::{
        AuthSection, CacheSection, Config, ConfigManager, ModelCatalogEntry, ModelConfig,
        VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    /// sha256("test-good-key")（与 rate_limit 认证测试同值）。
    const GOOD_KEY_DIGEST: &str =
        "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";

    fn vendor_with_models(id: &str, enabled: bool, models: &[&str]) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, String::new());
        v.enabled = enabled;
        v.models = models
            .iter()
            .map(|m| ModelConfig {
                model_id: m.to_string(),
                abstract_id: None,
                weight: 1.0,
                enabled: true,

                dimensions: None,
                compat_group: None,
                model_type: None,
            })
            .collect();
        v
    }

    fn catalog_entry(id: &str) -> ModelCatalogEntry {
        ModelCatalogEntry {
            model_id: id.into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 0.0,
            ..Default::default()
        }
    }

    async fn make_state(config: Config, with_cache: bool) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let database = Arc::new(Database { pool, repository });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));
        let response_cache = with_cache.then(|| {
            Arc::new(ResponseCache::new(
                Arc::new(oxcache::backend::default_memory_backend()),
                config_manager.clone(),
                Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
            ))
        });

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn base_config(vendors: Vec<VendorConfig>) -> Config {
        let mut config = common::test_config(vendors);
        config.model_catalog = vec![catalog_entry("mmm-shared"), catalog_entry("aaa-catalog")];
        config
    }

    fn authed_config(vendors: Vec<VendorConfig>) -> Config {
        let mut config = base_config(vendors);
        config.auth = Some(AuthSection {
            enabled: true,
            api_keys: vec![GOOD_KEY_DIGEST.to_string()],
            header: Default::default(),
            keys: Vec::new(),
        });
        config
    }

    async fn get(state: Arc<ApiState>, uri: &str, key: Option<&str>) -> axum::response::Response {
        let app = create_router(state).await;
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(k) = key {
            builder = builder.header("Authorization", format!("Bearer {k}"));
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn post_json(
        state: Arc<ApiState>,
        uri: &str,
        key: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let app = create_router(state).await;
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(k) = key {
            builder = builder.header("Authorization", format!("Bearer {k}"));
        }
        app.oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn read_json(response: axum::response::Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }

    ///：模型集合 = 目录 ∪ vendor 配置去重；同模型多
    /// vendor 只出现一次；禁用 vendor 的模型不出现。
    #[tokio::test]
    async fn models_lists_catalog_union_vendor_models_deduped() {
        let state = make_state(
            base_config(vec![
                vendor_with_models("vendor-a", true, &["mmm-shared", "zzz-vendor"]),
                vendor_with_models("vendor-b", true, &["mmm-shared"]),
                vendor_with_models("vendor-c", false, &["hidden-model"]),
            ]),
            false,
        )
        .await;

        let (status, body) = read_json(get(state, "/v1/models", None).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        let ids: Vec<&str> = body["data"]
            .as_array()
            .expect("data 数组")
            .iter()
            .map(|item| {
                assert_eq!(item["object"], "model");
                assert!(item["owned_by"].is_string(), "条目带 owned_by: {item}");
                item["id"].as_str().expect("id 字符串")
            })
            .collect();
        assert_eq!(ids, vec!["aaa-catalog", "mmm-shared", "zzz-vendor"]);
        assert_eq!(body["data"][1]["owned_by"], "vendor-a");
        assert_eq!(body["data"][0]["owned_by"], "fluxgate");
    }

    /// 真机回归：声明 abstract_id 的 vendor 模型，目录必须列路由可解析的
    /// 公开别名。裸列上游 model_id 时，客户端把清单里的 id 回投
    /// /v1/chat/completions 会 400「model not found」——路由按 abstract_id
    /// 匹配（缺省回落 model_id，见 ConfigManager::resolve_model）。
    #[tokio::test]
    async fn models_lists_the_routable_public_id_when_abstract_id_differs() {
        let mut vendor =
            vendor_with_models("stub-anthropic", true, &["claude-3-5-sonnet-20241022"]);
        vendor.models[0].abstract_id = Some("claude-3.5-sonnet".into());
        let state = make_state(base_config(vec![vendor]), false).await;

        let (status, body) = read_json(get(state.clone(), "/v1/models", None).await).await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&str> = body["data"]
            .as_array()
            .expect("data 数组")
            .iter()
            .map(|item| item["id"].as_str().expect("id 字符串"))
            .collect();
        assert_eq!(
            ids,
            vec!["aaa-catalog", "claude-3.5-sonnet", "mmm-shared"],
            "目录只列可路由的公开别名，不列裸上游 model_id"
        );
        assert_eq!(body["data"][1]["owned_by"], "stub-anthropic");

        // 清单里的 vendor 条目回投路由必须命中（不可路由的 id 即错误引导）
        let decision = state.router.route("claude-3.5-sonnet").await;
        assert!(
            decision.is_ok(),
            "目录列出的公开别名必须可路由: {decision:?}"
        );
        assert!(
            state
                .config
                .resolve_model("claude-3-5-sonnet-20241022")
                .is_empty(),
            "声明 abstract_id 后裸 model_id 不是路由名（目录不得再列它）"
        );
    }

    ///：未鉴权返回 401（与其它 /v1 端点一致）。
    #[tokio::test]
    async fn models_requires_auth() {
        let state = make_state(
            authed_config(vec![vendor_with_models("vendor-a", true, &["mmm-shared"])]),
            false,
        )
        .await;

        let (status, _) = read_json(get(state.clone(), "/v1/models", None).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = read_json(get(state, "/v1/models", Some("test-good-key")).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
    }

    ///：模型列表始终现算——缓存开启且接线时二次请求
    /// 仍无 X-Cache 头（不命中响应缓存）。
    #[tokio::test]
    async fn models_responses_are_never_cached() {
        let mut config = base_config(vec![vendor_with_models("vendor-a", true, &["mmm-shared"])]);
        config.cache = CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        };
        let state = make_state(config, true).await;

        for i in 0..2 {
            let response = get(state.clone(), "/v1/models", None).await;
            assert_eq!(response.status(), StatusCode::OK, "第 {i} 次请求");
            assert!(
                response.headers().get("x-cache").is_none(),
                "模型列表不得带 X-Cache 头（第 {i} 次）"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["object"], "list");
        }
    }

    ///：合法请求返回正整数 input_tokens；相同输入两次
    /// 结果一致（确定性）；更长输入更大。
    #[tokio::test]
    async fn count_tokens_estimates_deterministically() {
        let state = make_state(
            base_config(vec![vendor_with_models("vendor-a", true, &["mmm-shared"])]),
            false,
        )
        .await;

        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello world, please help me."},
                {"role": "assistant", "content": "Sure, what do you need?"},
            ],
            "system": "Be helpful.",
            "tools": [{"type": "function", "function": {"name": "get_weather"}}],
        });
        let (status, first) = read_json(
            post_json(
                state.clone(),
                "/v1/messages/count_tokens",
                None,
                body.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let first_tokens = first["input_tokens"].as_u64().expect("input_tokens u64");
        assert!(first_tokens > 0, "估计为正整数");

        let (status, second) =
            read_json(post_json(state.clone(), "/v1/messages/count_tokens", None, body).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            second["input_tokens"].as_u64(),
            Some(first_tokens),
            "相同输入两次结果一致"
        );

        let long = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello world, please write a long essay about the history of computing for me, covering everything from abacus to quantum computers in great detail with many chapters."},
            ],
            "system": "Be helpful and verbose.",
            "tools": [{"type": "function", "function": {"name": "get_weather"}}, {"type": "function", "function": {"name": "search_docs"}}],
        });
        let (status, long_body) =
            read_json(post_json(state, "/v1/messages/count_tokens", None, long).await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            long_body["input_tokens"].as_u64().unwrap() > first_tokens,
            "更长输入估计更大"
        );
    }

    ///：未鉴权返回与其它 /v1 端点一致的 401。
    #[tokio::test]
    async fn count_tokens_requires_auth() {
        let state = make_state(
            authed_config(vec![vendor_with_models("vendor-a", true, &["mmm-shared"])]),
            false,
        )
        .await;

        let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});
        let (status, _) = read_json(
            post_json(
                state.clone(),
                "/v1/messages/count_tokens",
                None,
                body.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, ok) = read_json(
            post_json(
                state,
                "/v1/messages/count_tokens",
                Some("test-good-key"),
                body,
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(ok["input_tokens"].as_u64().unwrap() > 0);
    }

    ///：缺 messages / 非法 body 返回 Validation 错误结构。
    #[tokio::test]
    async fn count_tokens_rejects_bad_params() {
        let state = make_state(
            base_config(vec![vendor_with_models("vendor-a", true, &["mmm-shared"])]),
            false,
        )
        .await;

        for (name, body) in [
            ("空对象", json!({})),
            ("缺 messages", json!({"model": "gpt-4"})),
            (
                "缺 model",
                json!({"messages": [{"role": "user", "content": "hi"}]}),
            ),
            ("空 messages", json!({"model": "gpt-4", "messages": []})),
            ("非对象", json!(42)),
        ] {
            let (status, err) =
                read_json(post_json(state.clone(), "/v1/messages/count_tokens", None, body).await)
                    .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name} 必须 400");
            assert_eq!(err["error"]["type"], "validation_error", "{name} 错误码");
            assert!(
                !err["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{name} 文案非空（i18n）"
            );
        }
    }
}

/// 回退观测头：开关开启时成功响应附
/// 四头（单跳 attempts=1；首跳失败换路 attempts≥2 且链完整；流式与非
/// 流式两类）；开关关闭时四头均不存在。
mod fallback_observation_headers {
    use crate::common;
    use axum::{body::Body, http::Request, http::StatusCode, routing::post, Router};
    use fluxgate::api::{chat_completions, ApiState};
    use fluxgate::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ResilienceSection,
        ServerConfig, VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn make_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
        crate::common::ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            chat_path: None,
            api_style: None,
            id: id.into(),
            name: format!("Test {id}"),
            type_: "openai".into(),
            api_key_env: "FLUXGATE_TEST_KEY".into(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: api_base.into(),
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "gpt-4".into(),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,

                dimensions: None,
                compat_group: None,
                model_type: None,
            }],
            rate_limit: fluxgate::config::RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: fluxgate::config::ConcurrencyConfig {
                requests_per_second: 1000,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
            supports_batch: false,
            anthropic_version: None,
            forward_beta_headers: false,
            send_nonstandard_params: false,
        }
    }

    fn make_config(vendors: Vec<VendorConfig>, expose: bool) -> Config {
        let mut config = common::test_config(vendors);
        config.server = ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        };
        config.database = DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        };
        config.global_model = Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        });
        config.resilience = ResilienceSection {
            cover_all_paths: true,
            expose_fallback_headers: expose,
            ..Default::default()
        };
        config
    }

    async fn make_state(config: Config) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let database = Arc::new(Database { pool, repository });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    async fn post_chat(
        state: Arc<ApiState>,
        vendor_hint: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(v) = vendor_hint {
            builder = builder.header("x-vendor", v);
        }
        app(state)
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn chat_body(stream: bool) -> Value {
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 10,
            "stream": stream
        })
    }

    fn header<'a>(response: &'a axum::response::Response, name: &str) -> Option<&'a str> {
        response.headers().get(name)?.to_str().ok()
    }

    fn assert_no_obs_headers(response: &axum::response::Response) {
        for name in [
            "x-fluxgate-vendor",
            "x-fluxgate-model",
            "x-fluxgate-attempts",
            "x-fluxgate-fallbacks",
        ] {
            assert!(
                header(response, name).is_none(),
                "开关关闭时 {name} 不得存在"
            );
        }
    }

    /// 单跳直连（非流式）：attempts=1，fallbacks 仅含命中者。
    #[tokio::test]
    async fn single_hop_nonstream_carries_single_attempt_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"solo","object":"chat.completion","choices":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(vec![make_vendor("solo-a", server.url())], true)).await;
        let response = post_chat(state, None, chat_body(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("solo-a"));
        assert_eq!(header(&response, "x-fluxgate-model"), Some("gpt-4"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("1"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("solo-a"));

        mock.assert_async().await;
    }

    /// 单跳直连（流式）：响应头阶段即带四头。
    #[tokio::test]
    async fn single_hop_stream_carries_headers_at_response_head() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"solo-sse\"}\n\ndata: [DONE]\n\n")
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(vec![make_vendor("solo-a", server.url())], true)).await;
        let response = post_chat(state, None, chat_body(true)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("solo-a"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("1"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("solo-a"));

        mock.assert_async().await;
    }

    /// 首跳失败换路（非流式）：attempts≥2，fallbacks 按尝试顺序完整。
    #[tokio::test]
    async fn first_hop_failure_nonstream_carries_full_chain_headers() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"from-b","object":"chat.completion","choices":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(
            vec![
                make_vendor("fb-a", server_a.url()),
                make_vendor("fb-b", server_b.url()),
            ],
            true,
        ))
        .await;
        let response = post_chat(state, Some("fb-a"), chat_body(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("fb-b"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("2"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("fb-a,fb-b"));

        mock_a.assert_async().await;
        mock_b.assert_async().await;
    }

    /// 首跳失败换路（流式）：首字节前失败换路，响应头带完整链。
    #[tokio::test]
    async fn first_hop_failure_stream_carries_full_chain_headers() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(503)
            .with_body("overloaded")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"b-sse\"}\n\ndata: [DONE]\n\n")
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(
            vec![
                make_vendor("fb-a", server_a.url()),
                make_vendor("fb-b", server_b.url()),
            ],
            true,
        ))
        .await;
        let response = post_chat(state, Some("fb-a"), chat_body(true)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("fb-b"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("2"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("fb-a,fb-b"));

        mock_a.assert_async().await;
        mock_b.assert_async().await;
    }

    /// 开关关闭（默认）：流式与非流式均无四头。
    #[tokio::test]
    async fn switch_off_omits_all_four_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock_json = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"off","object":"chat.completion","choices":[]}"#)
            .expect(2)
            .create_async()
            .await;

        let state = make_state(make_config(vec![make_vendor("off-a", server.url())], false)).await;
        let response = post_chat(state.clone(), None, chat_body(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_no_obs_headers(&response);

        // 流式同断言（同一 mock 需支持 SSE 体——此处借“响应头断言在 body
        // 前”之便：非流式 JSON 体走流式请求仍 200 建流（上游体原样转
        // 发），头断言不受体形态影响）
        let response = post_chat(state, None, chat_body(true)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_no_obs_headers(&response);

        mock_json.assert_async().await;
    }
}

/// 路由可解释性：只读端点，
/// reason_code 枚举覆盖，入选者权重分解与档位，解释与路由共用同一实现。
mod route_explain {
    use crate::common;
    use axum::{body::Body, http::Request, http::StatusCode};
    use fluxgate::api::create_router;
    use fluxgate::config::{AuthSection, ConfigManager, ModelConfig, RoutingSection, VendorConfig};
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::{AffinityTable, Router as NexusRouter};
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    const GOOD_KEY_DIGEST: &str =
        "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";

    fn vendor_with_models(id: &str, enabled: bool, models: &[&str]) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, String::new());
        v.enabled = enabled;
        v.models = models
            .iter()
            .map(|m| ModelConfig {
                model_id: m.to_string(),
                abstract_id: None,
                weight: 1.0,
                enabled: true,

                dimensions: None,
                compat_group: None,
                model_type: None,
            })
            .collect();
        v
    }

    struct Wire {
        affinity: Option<Arc<AffinityTable>>,
        auth: bool,
        routing: Option<RoutingSection>,
    }

    async fn make_state(config_vendors: Vec<VendorConfig>, wire: Wire) -> Arc<ApiState> {
        let mut config = common::test_config(config_vendors);
        if wire.auth {
            config.auth = Some(AuthSection {
                enabled: true,
                api_keys: vec![GOOD_KEY_DIGEST.to_string()],
                header: Default::default(),
                keys: Vec::new(),
            });
        }
        if let Some(routing) = wire.routing {
            config.routing = routing;
        }
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let database = Arc::new(Database { pool, repository });
        let affinity = wire.affinity.clone();
        let mut router = NexusRouter::new(config_manager.clone(), vendor_manager.clone());
        if let Some(table) = affinity.clone() {
            router = router.with_affinity(table);
        }

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router: Arc::new(router),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn plain() -> Wire {
        Wire {
            affinity: None,
            auth: false,
            routing: None,
        }
    }

    async fn post_explain(
        state: Arc<ApiState>,
        key: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let app = create_router(state).await;
        let mut builder = Request::builder()
            .method("POST")
            .uri("/admin/route/explain")
            .header("content-type", "application/json");
        if let Some(k) = key {
            builder = builder.header("Authorization", format!("Bearer {k}"));
        }
        app.oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn read_json(response: axum::response::Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn row<'a>(body: &'a Value, vendor: &str) -> &'a Value {
        body["candidates"]
            .as_array()
            .expect("candidates 数组")
            .iter()
            .find(|c| c["vendor"] == vendor)
            .unwrap_or_else(|| panic!("缺候选行 {vendor}: {body}"))
    }

    /// 入选者：decision eligible + 5 因子分解自洽（加和 == base）+
    /// 默认配置下 weight == base == 路由同一方法权重 + selection 档位。
    #[tokio::test]
    async fn explain_eligible_carries_breakdown_and_selection() {
        let vendors = vec![vendor_with_models("ex-a", true, &["gpt-4"])];
        let state = make_state(vendors.clone(), plain()).await;

        let (status, body) =
            read_json(post_explain(state.clone(), None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "ex-a");
        assert_eq!(item["decision"], "eligible");
        assert_eq!(item["reason_code"], "eligible");
        assert_eq!(item["selection"], "weighted");
        let f = &item["factors"];
        let sum = f["model"].as_f64().unwrap()
            + f["queue"].as_f64().unwrap()
            + f["window"].as_f64().unwrap()
            + f["latency"].as_f64().unwrap()
            + f["error"].as_f64().unwrap();
        assert!(
            (sum - item["base_weight"].as_f64().unwrap()).abs() < 1e-9,
            "分解加和必须等于合成基准: {item}"
        );
        assert!(
            (item["weight"].as_f64().unwrap() - item["base_weight"].as_f64().unwrap()).abs() < 1e-9,
            "默认配置下生产权重 == 合成基准: {item}"
        );

        // 共用同一实现：路由对同一候选集算出的权重必须一致
        let pair = {
            let config = state.config.read();
            let v = config
                .vendors
                .iter()
                .find(|v| v.id == "ex-a")
                .unwrap()
                .clone();
            let m = v
                .models
                .iter()
                .find(|m| m.model_id == "gpt-4")
                .unwrap()
                .clone();
            vec![(v, m)]
        };
        let finals = state.router.calculate_weights(pair).await;
        assert!(
            (finals[0].0 - item["weight"].as_f64().unwrap()).abs() < 1e-9,
            "解释权重必须与路由同一方法一致"
        );
    }

    /// 未鉴权 401（与既有 admin 一致）；未知模型 400 validation_error。
    #[tokio::test]
    async fn explain_auth_and_unknown_model() {
        let state = make_state(
            vec![vendor_with_models("ex-a", true, &["gpt-4"])],
            Wire {
                affinity: None,
                auth: true,
                routing: None,
            },
        )
        .await;

        let (status, _) =
            read_json(post_explain(state.clone(), None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = read_json(
            post_explain(
                state.clone(),
                Some("test-good-key"),
                json!({"model": "nope-unknown"}),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "validation_error");

        let (status, _) =
            read_json(post_explain(state, Some("test-good-key"), json!({"model": "gpt-4"})).await)
                .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// disabled：vendor 禁用 → excluded/disabled。
    #[tokio::test]
    async fn explain_disabled_vendor() {
        let state = make_state(
            vec![vendor_with_models("dis-a", false, &["gpt-4"])],
            plain(),
        )
        .await;
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "dis-a");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(item["reason_code"], "disabled");
    }

    /// cooling_down：Throttled 状态 → excluded/cooling_down。
    #[tokio::test]
    async fn explain_cooling_down_vendor() {
        let state = make_state(vec![vendor_with_models("cd-a", true, &["gpt-4"])], plain()).await;
        state
            .vendor_manager
            .mark_throttled("cd-a", Duration::from_secs(60));
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "cd-a");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(item["reason_code"], "cooling_down");
    }

    /// cooling_down（key 池粒度，M9/）：唯一 key
    /// 因 401 永久黑名单 → 池耗尽，explain 报 cooling_down 且 detail 注明
    /// key 池耗尽与指纹（vendor 状态本身仍 Available——修复前误报 eligible）。
    #[tokio::test]
    async fn explain_key_pool_exhausted_reports_cooling_down() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"bad key"}}"#)
            .expect(1)
            .create_async()
            .await;
        let mut vendor = common::base_vendor("kp-a", server.url());
        vendor.models = vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: None,
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        let state = make_state(vec![vendor], plain()).await;
        // 一次真实派发驱动 401 → 永久错误 → 唯一 key 进黑名单。
        let app = create_router(state.clone()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "hi"}]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        mock.assert_async().await;

        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "kp-a");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(
            item["reason_code"], "cooling_down",
            "池耗尽必须 cooling_down（修复前 eligible）: {item}"
        );
        let detail = item["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("key 池耗尽") || detail.contains("Key pool exhausted"),
            "detail 必须注明 key 池耗尽: {detail}"
        );
    }

    /// throttled：限流窗口耗尽（Token max 0）→ excluded/throttled。
    #[tokio::test]
    async fn explain_throttled_window() {
        common::ensure_test_api_key();
        let mut v = common::base_vendor("th-a", String::new());
        v.models = vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v.rate_limit = fluxgate::config::RateLimitConfig::Token {
            max_tokens: 0,
            window_hours: None,
            sliding: false,
        };
        let state = make_state(vec![v], plain()).await;
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "th-a");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(item["reason_code"], "throttled");
    }

    /// not_in_catalog：不提供该模型的 vendor 行优势排除。
    #[tokio::test]
    async fn explain_not_in_catalog() {
        let state = make_state(
            vec![
                vendor_with_models("has-a", true, &["gpt-4"]),
                vendor_with_models("other-b", true, &["claude-x"]),
            ],
            plain(),
        )
        .await;
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(row(&body, "has-a")["decision"], "eligible");
        let item = row(&body, "other-b");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(item["reason_code"], "not_in_catalog");
    }

    /// affinity_bound_elsewhere：会话绑定他处 → excluded；绑定者 eligible。
    #[tokio::test]
    async fn explain_affinity_bound_elsewhere() {
        let table = Arc::new(AffinityTable::with_memory_backend(Duration::from_secs(
            3600,
        )));
        let state = make_state(
            vec![
                vendor_with_models("af-a", true, &["gpt-4"]),
                vendor_with_models("af-b", true, &["gpt-4"]),
            ],
            Wire {
                affinity: Some(table.clone()),
                auth: false,
                routing: None,
            },
        )
        .await;
        table.bind(None, "sess-1", "af-b").await;

        let (status, body) = read_json(
            post_explain(
                state.clone(),
                None,
                json!({"model": "gpt-4", "session_id": "sess-1"}),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let item_a = row(&body, "af-a");
        assert_eq!(item_a["decision"], "excluded");
        assert_eq!(item_a["reason_code"], "affinity_bound_elsewhere");
        assert_eq!(row(&body, "af-b")["decision"], "eligible");

        // 无 session 时两家均 eligible（亲和不参与）
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(row(&body, "af-a")["decision"], "eligible");
        assert_eq!(row(&body, "af-b")["decision"], "eligible");
    }

    /// weight_zero：全零系数 → 合成为 0 → excluded/weight_zero。
    #[tokio::test]
    async fn explain_weight_zero() {
        let state = make_state(
            vec![vendor_with_models("wz-a", true, &["gpt-4"])],
            Wire {
                affinity: None,
                auth: false,
                routing: Some(RoutingSection {
                    w_model: 0.0,
                    w_queue: 0.0,
                    w_window: 0.0,
                    w_latency: 0.0,
                    w_error: 0.0,
                    ..Default::default()
                }),
            },
        )
        .await;
        let (status, body) =
            read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
        assert_eq!(status, StatusCode::OK);
        let item = row(&body, "wz-a");
        assert_eq!(item["decision"], "excluded");
        assert_eq!(item["reason_code"], "weight_zero");
    }

    /// selection 档位随配置切换：linucb /
    /// power_of_two / 默认 weighted 均如实上报；linucb 大小写与空白不敏感
    /// （与选择层同口径），未知值回滚 weighted。因子分解不随档位变形。
    #[tokio::test]
    async fn explain_selection_tier_follows_config() {
        for (configured, expected) in [
            ("linucb", "linucb"),
            ("  LinUCB ", "linucb"),
            ("power_of_two", "power_of_two"),
            ("bogus_tier", "weighted"),
            ("weighted", "weighted"),
        ] {
            let state = make_state(
                vec![vendor_with_models("sel-a", true, &["gpt-4"])],
                Wire {
                    affinity: None,
                    auth: false,
                    routing: Some(RoutingSection {
                        selection: configured.to_string(),
                        ..Default::default()
                    }),
                },
            )
            .await;
            let (status, body) =
                read_json(post_explain(state, None, json!({"model": "gpt-4"})).await).await;
            assert_eq!(status, StatusCode::OK, "档位 {configured} 解释必须成功");
            let item = row(&body, "sel-a");
            assert_eq!(item["decision"], "eligible", "档位 {configured}: {item}");
            assert_eq!(
                item["selection"], expected,
                "配置 {configured:?} 应报档位 {expected}: {item}"
            );
            assert!(
                item["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&format!("selection={expected}")),
                "detail 需同步档位: {item}"
            );
            // 档位只改选择层，不改 5 因子分解的自洽性
            let f = &item["factors"];
            let sum = f["model"].as_f64().unwrap()
                + f["queue"].as_f64().unwrap()
                + f["window"].as_f64().unwrap()
                + f["latency"].as_f64().unwrap()
                + f["error"].as_f64().unwrap();
            assert!(
                (sum - item["base_weight"].as_f64().unwrap()).abs() < 1e-9,
                "档位 {configured} 下分解加和仍需等于合成基准: {item}"
            );
        }
    }

    /// 只读性：调用前后 vendor 状态与亲和绑定不变（死 api_base 亦证明
    /// 无上游请求——任何出站尝试都会让解释失败）。
    #[tokio::test]
    async fn explain_is_read_only() {
        let table = Arc::new(AffinityTable::with_memory_backend(Duration::from_secs(
            3600,
        )));
        let state = make_state(
            vec![
                vendor_with_models("ro-a", true, &["gpt-4"]),
                vendor_with_models("ro-b", true, &["gpt-4"]),
            ],
            Wire {
                affinity: Some(table.clone()),
                auth: false,
                routing: None,
            },
        )
        .await;
        table.bind(None, "sess-9", "ro-b").await;

        let before_a = state.vendor_manager.get_status("ro-a").await;
        let (status, _) = read_json(
            post_explain(
                state.clone(),
                None,
                json!({"model": "gpt-4", "session_id": "sess-9"}),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state.vendor_manager.get_status("ro-a").await, before_a);
        assert_eq!(
            state.vendor_manager.get_status("ro-b").await,
            before_a,
            "解释不得改变 vendor 状态"
        );
        assert_eq!(
            table.lookup(None, "sess-9").await.as_deref(),
            Some("ro-b"),
            "解释不得改写亲和绑定"
        );
    }
}

/// Anthropic 路径回退头（/v1/messages 入站）。
mod fallback_observation_headers_anthropic {
    use crate::common;
    use axum::{body::Body, http::Request, http::StatusCode, routing::post, Router};
    use fluxgate::api::{messages, ApiState};
    use fluxgate::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ResilienceSection,
        ServerConfig, VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::{AffinityTable, Router as NexusRouter};
    use fluxgate::vendor::VendorManager;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    fn make_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
        crate::common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.type_ = "anthropic".into();
        v.models = vec![ModelConfig {
            model_id: "claude-x".into(),
            abstract_id: Some("claude-x".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v
    }

    fn make_config(vendors: Vec<VendorConfig>, expose: bool) -> Config {
        let mut config = common::test_config(vendors);
        config.server = ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        };
        config.database = DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        };
        config.global_model = Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        });
        config.resilience = ResilienceSection {
            cover_all_paths: true,
            expose_fallback_headers: expose,
            ..Default::default()
        };
        config
    }

    /// 亲和表无条件接线：Anthropic 入站无 X-Vendor 路由语义（见
    /// `resolve_route`），首跳只能由 hard 亲和绑定钉住（等权候选间的
    /// weighted_select 是随机的，不钉则换路用例不确定）。
    async fn make_state(config: Config) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let database = Arc::new(Database { pool, repository });
        let affinity = Arc::new(AffinityTable::with_memory_backend(Duration::from_secs(
            3600,
        )));
        let router = NexusRouter::new(config_manager.clone(), vendor_manager.clone())
            .with_affinity(affinity.clone());

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router: Arc::new(router),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity: Some(affinity),
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .with_state(state)
    }

    /// `session` 为 X-Session-Id 值（None 时走 messages 前缀兜底键）。
    async fn post_messages(
        state: Arc<ApiState>,
        session: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json");
        if let Some(s) = session {
            builder = builder.header("x-session-id", s);
        }
        app(state)
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn messages_body(stream: bool) -> Value {
        json!({
            "model": "claude-x",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": stream,
        })
    }

    fn header<'a>(response: &'a axum::response::Response, name: &str) -> Option<&'a str> {
        response.headers().get(name)?.to_str().ok()
    }

    const ANTHROPIC_JSON: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#;

    /// Anthropic 非流式单跳：attempts=1，链仅命中者。
    #[tokio::test]
    async fn anthropic_nonstream_single_hop_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ANTHROPIC_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(vec![make_vendor("ax-a", server.url())], true)).await;
        let response = post_messages(state, None, messages_body(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("ax-a"));
        assert_eq!(header(&response, "x-fluxgate-model"), Some("claude-x"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("1"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("ax-a"));

        mock.assert_async().await;
    }

    /// Anthropic 流式换路：首跳 503 → attempts=2，链完整。
    ///
    /// 首跳由亲和绑定钉在 ax-a（Anthropic 入站不消费 X-Vendor）；失败后
    /// 换路走 `route_excluding`，ax-a 被排除 → ax-b 接管。
    #[tokio::test]
    async fn anthropic_stream_failover_headers() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/messages")
            .with_status(503)
            .with_body("overloaded")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: message_stop\ndata: {}\n\n")
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(
            vec![
                make_vendor("ax-a", server_a.url()),
                make_vendor("ax-b", server_b.url()),
            ],
            true,
        ))
        .await;
        state
            .affinity
            .as_ref()
            .expect("亲和表已接线")
            .bind(None, "obs-sess", "ax-a")
            .await;
        let response = post_messages(state, Some("obs-sess"), messages_body(true)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-fluxgate-vendor"), Some("ax-b"));
        assert_eq!(header(&response, "x-fluxgate-model"), Some("claude-x"));
        assert_eq!(header(&response, "x-fluxgate-attempts"), Some("2"));
        assert_eq!(header(&response, "x-fluxgate-fallbacks"), Some("ax-a,ax-b"));

        mock_a.assert_async().await;
        mock_b.assert_async().await;
    }

    /// 开关关闭：Anthropic 路径无四头。
    #[tokio::test]
    async fn anthropic_switch_off_omits_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ANTHROPIC_JSON)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(make_config(vec![make_vendor("ax-a", server.url())], false)).await;
        let response = post_messages(state, None, messages_body(false)).await;
        assert_eq!(response.status(), StatusCode::OK);
        for name in [
            "x-fluxgate-vendor",
            "x-fluxgate-model",
            "x-fluxgate-attempts",
            "x-fluxgate-fallbacks",
        ] {
            assert!(header(&response, name).is_none(), "关闭时 {name} 不得存在");
        }

        mock.assert_async().await;
    }
}
