// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod endpoints {
    use axum::{
        body::Body,
        http::Request,
        routing::{get, post},
        Router,
    };
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::{
        api::{chat_completions, messages},
        config::{
            Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
            VendorConfig,
        },
        database::Database,
        protocol::{AnthropicConverter, OpenAIConverter},
        vendor::VendorManager,
        ApiState,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn create_test_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![
                VendorConfig {
                    id: "openai_main".to_string(),
                    name: "OpenAI Main".to_string(),
                    type_: "openai".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    models: vec![ModelConfig {
                        model_id: "gpt-4".to_string(),
                        abstract_id: Some("gpt-4".to_string()),
                        weight: 1.0,
                        enabled: true,
                    }],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 10,
                    },
                },
                VendorConfig {
                    id: "anthropic_main".to_string(),
                    name: "Anthropic Main".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.0,
                        enabled: true,
                    }],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 10,
                    },
                },
            ],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
        }
    }

    async fn create_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config.clone()));

        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            max_connections: 1,
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
        });

        let router = Arc::new(nexusflow::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            client: reqwest::Client::new(),
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

        let unified_response = nexusflow::protocol::unified::UnifiedResponse {
            id: "test-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "gpt-4".to_string(),
            choices: vec![nexusflow::protocol::unified::Choice {
                index: 0,
                message: Some(nexusflow::protocol::unified::Message {
                    role: nexusflow::protocol::unified::Role::Assistant,
                    content: nexusflow::protocol::unified::MessageContent::Text(
                        "Hello!".to_string(),
                    ),
                }),
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: nexusflow::protocol::unified::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };

        let external = state.openai_converter.from_unified(unified_response);
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
        let router = Arc::new(nexusflow::router::Router::new(
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
        let router = Arc::new(nexusflow::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        // gpt-4 should fail because model is disabled
        let decision = router.route("gpt-4").await;
        assert!(decision.is_err());
    }
}

mod integration {
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::api::ApiState;
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
        VendorConfig,
    };
    use nexusflow::database::Database;
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;

    fn create_test_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![VendorConfig {
                id: "openai_main".to_string(),
                name: "OpenAI Main".to_string(),
                type_: "openai".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "".to_string(),
                enabled: true,
                models: vec![ModelConfig {
                    model_id: "gpt-4".to_string(),
                    abstract_id: Some("gpt-4".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 10,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
        }
    }

    async fn create_mock_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));

        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            max_connections: 1,
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
        });

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            client: reqwest::Client::new(),
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

        let unified_response = nexusflow::protocol::unified::UnifiedResponse {
            id: "test-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "gpt-4".to_string(),
            choices: vec![nexusflow::protocol::unified::Choice {
                index: 0,
                message: Some(nexusflow::protocol::unified::Message {
                    role: nexusflow::protocol::unified::Role::Assistant,
                    content: nexusflow::protocol::unified::MessageContent::Text(
                        "Hello!".to_string(),
                    ),
                }),
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: nexusflow::protocol::unified::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };

        let external = converter.from_unified(unified_response);
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
    use nexusflow::middleware::auth::api_auth_middleware;
    use nexusflow::middleware::ApiAuthState;
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
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
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

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_middleware_missing_key() {
        let auth_state = Arc::new(ApiAuthState::new(true));
        let app = build_test_router(auth_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
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
    use axum::extract::{Path, State};
    use axum::Json;
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::api::admin;
    use nexusflow::api::ApiState;
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
        VendorConfig,
    };
    use nexusflow::database::Database;
    use nexusflow::error::NexusFlowError;
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn create_test_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![VendorConfig {
                id: "openai_main".to_string(),
                name: "OpenAI Main".to_string(),
                type_: "openai".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "".to_string(),
                enabled: true,
                models: vec![ModelConfig {
                    model_id: "gpt-4".to_string(),
                    abstract_id: Some("gpt-4".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 10,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
        }
    }

    async fn create_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        // 初始化厂商实例，使 get_all_status() 能返回数据
        vendor_manager.initialize().await.unwrap();

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            max_connections: 1,
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
        });

        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));
        let openai_converter: Arc<dyn ProtocolConverter> = Arc::new(OpenAIConverter::new());
        let anthropic_converter: Arc<dyn ProtocolConverter> = Arc::new(AnthropicConverter::new());

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            client: reqwest::Client::new(),
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
        assert!(arr[0]["queue_length"].is_number(), "queue_length must be a number");
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
        assert!(result.is_ok(), "reset_vendor should succeed for any vendor id");
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

    #[tokio::test]
    async fn test_admin_get_models() {
        let state = create_api_state().await;
        let result = admin::get_models(State(state)).await;
        assert!(result.is_ok());
        let models = result.unwrap().0;
        // Config 有 global_model: Some(...)
        assert_eq!(models.len(), 1);
        assert!(models[0].enabled);
        assert_eq!(models[0].strategy, "weighted");
    }

    #[tokio::test]
    async fn test_admin_update_vendor_not_found() {
        let state = create_api_state().await;
        let vendor = VendorConfig {
            id: "nonexistent".to_string(),
            name: "Test".to_string(),
            type_: "openai".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            api_base: "".to_string(),
            enabled: true,
            models: vec![],
            rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 10,
            },
        };
        let result =
            admin::update_vendor(State(state), Path("nonexistent".to_string()), Json(vendor))
                .await;
        match result {
            Err(NexusFlowError::Config { message }) => {
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
            Err(NexusFlowError::Config { message }) => {
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
}
