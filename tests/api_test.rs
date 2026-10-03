#![cfg(feature = "web-server")]

// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod endpoints {
    use axum::{
        body::Body,
        http::Request,
        routing::{get, post},
        Router,
    };
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
            rate_limit: Default::default(),
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
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    id: "openai_main".to_string(),
                    name: "OpenAI Main".to_string(),
                    type_: "openai".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
                    models: vec![ModelConfig {
                        model_id: "gpt-4".to_string(),
                        abstract_id: Some("gpt-4".to_string()),
                        weight: 1.0,
                        enabled: true,
                    }],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 10,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                },
                VendorConfig {
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    id: "anthropic_main".to_string(),
                    name: "Anthropic Main".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.0,
                        enabled: true,
                    }],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 10,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                },
            ],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
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
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![VendorConfig {
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                id: "openai_main".to_string(),
                name: "OpenAI Main".to_string(),
                type_: "openai".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "".to_string(),
                enabled: true,
                priority: 1,
                models: vec![ModelConfig {
                    model_id: "gpt-4".to_string(),
                    abstract_id: Some("gpt-4".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: nexusflow::config::RateLimitConfig::Interval {
                    hours: 1,
                    sliding: false,
                },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 10,
                    max_concurrent_requests: None,
                    token_weighted: false,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
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
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::Json;
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
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![VendorConfig {
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                id: "openai_main".to_string(),
                name: "OpenAI Main".to_string(),
                type_: "openai".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "".to_string(),
                enabled: true,
                priority: 1,
                models: vec![ModelConfig {
                    model_id: "gpt-4".to_string(),
                    abstract_id: Some("gpt-4".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: nexusflow::config::RateLimitConfig::Interval {
                    hours: 1,
                    sliding: false,
                },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 10,
                    max_concurrent_requests: None,
                    token_weighted: false,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
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
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: "nonexistent".to_string(),
            name: "Test".to_string(),
            type_: "openai".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            api_base: "".to_string(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: nexusflow::config::RateLimitConfig::Interval {
                hours: 1,
                sliding: false,
            },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 10,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        };
        let result =
            admin::update_vendor(State(state), Path("nonexistent".to_string()), Json(vendor)).await;
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
        async fn body_json<R: IntoResponse>(resp: Result<R, NexusFlowError>) -> serde_json::Value {
            let bytes =
                axum::body::to_bytes(resp.unwrap().into_response().into_body(), 1024 * 1024)
                    .await
                    .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        let state = create_api_state().await;
        // update_config 需要路径记忆才落盘
        let dir = std::env::temp_dir().join("nx-admin-restart-required");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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

    /// update_vendor 经原子 RMW：改单字段不回退其他字段，且未找到时
    /// 内存不变
    #[tokio::test]
    async fn test_admin_update_vendor_atomic_rmw() {
        let state = create_api_state().await;
        let dir = std::env::temp_dir().join("nx-admin-rmw");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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

/// recId=14：流式路径错误传播 —— 上游非 2xx 不再以 200 正文伪装成 SSE 流。
///
/// - send 后先校验状态码：401 → VendorUnavailable（error.rs 固定映射 503
///   JSON），429 → RateLimited（固定映射 429）；
/// - 透传流式与转换流式两路径同口径（消除同网关两套行为）；
/// - 错误分支补 GlobalMetrics 记账（此前流式路径零指标）。
mod streaming_error_propagation {
    use axum::{body::Body, http::Request, routing::post, Router};
    use nexusflow::api::{chat_completions, messages};
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
        VendorConfig,
    };
    use nexusflow::database::Database;
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::Router as NexusRouter;
    use nexusflow::vendor::VendorManager;
    use nexusflow::ApiState;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn ensure_test_api_key() {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    }

    /// type_ 决定出站协议与 URL 形状：anthropic → {base}/v1/messages，
    /// openai → {base}/v1/chat/completions。
    fn make_vendor(id: &str, type_: &str, api_base: String) -> VendorConfig {
        ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            name: format!("Test {}", id),
            type_: type_.into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base,
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "target-model".into(),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: nexusflow::config::RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }
    }

    fn make_config(vendors: Vec<VendorConfig>) -> Config {
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors,
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
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

// ============ recId=9：日志管道接线（异步批量 + token 语义 + 失败落库） ============

#[cfg(test)]
mod logging_pipeline {
    use axum::{body::Body, http::Request, routing::post, Router};
    use nexusflow::api::{chat_completions, ApiState};
    use nexusflow::config::{
        CacheSection, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, RoutingSection, ServerConfig, VendorConfig,
    };
    use nexusflow::database::{
        AsyncLogger, AsyncLoggerHandle, LogFilter, Repository, TokenPreAggregator,
    };
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::Router as NexusRouter;
    use nexusflow::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    fn ensure_test_api_key() {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    }

    fn make_config(api_base: String) -> Config {
        ensure_test_api_key();
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![VendorConfig {
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                id: "openai_main".into(),
                name: "OpenAI Main".into(),
                type_: "openai".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
                api_base,
                enabled: true,
                priority: 1,
                models: vec![ModelConfig {
                    model_id: "gpt-4".into(),
                    abstract_id: Some("gpt-4".into()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: RateLimitConfig::Token {
                    max_tokens: 1_000_000,
                    window_hours: None,
                    sliding: false,
                },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 100,
                    max_concurrent_requests: None,
                    token_weighted: false,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: RoutingSection::default(),
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
        let config_manager = Arc::new(ConfigManager::new(make_config(api_base)));
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

        let database = Arc::new(nexusflow::database::Database {
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

    /// ⑦（评审修正回归）Anthropic 格式响应的 usage 键是 input_tokens/
    /// output_tokens（透传路径原始上游 JSON；转换路径
    /// response_to_external 同格式）——落库必须双格式取值：prompt_tokens
    /// 回退 input_tokens、total 以 prompt+completion 合成，且预聚合器记账
    /// （此前三个 token 字段恒 NULL、token_stats 对 anthropic 流量恒空）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_usage_keys_are_mapped_end_to_end() {
        use axum::routing::post as post_route;
        use nexusflow::api::messages;

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
            database: Arc::new(nexusflow::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: Some(logger.clone()),
            pre_aggregator: Some(pa.clone()),
            affinity: None,
            response_cache: None,
            bandit: None,
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

// ============ recId=15：embeddings 回归供应商治理 ============

#[cfg(test)]
mod embeddings_governance {
    use axum::{body::Body, http::Request, routing::post, Router};
    use nexusflow::api::{embeddings, ApiState};
    use nexusflow::config::{
        CacheSection, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, RoutingSection, ServerConfig, VendorConfig,
    };
    use nexusflow::database::{AsyncLogger, AsyncLoggerHandle, Repository, TokenPreAggregator};
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::Router as NexusRouter;
    use nexusflow::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn ensure_test_api_key() {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    }

    fn make_vendor(id: &str, api_base: String, max_tokens: u64) -> VendorConfig {
        ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            name: format!("Test {id}"),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base,
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "text-embedding-3-small".into(),
                abstract_id: Some("text-embedding-3-small".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens,
                window_hours: None,
                sliding: false,
            },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }
    }

    async fn make_state(vendors: Vec<VendorConfig>) -> Arc<ApiState> {
        let config = Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors,
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: RoutingSection::default(),
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(nexusflow::database::Database {
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

    /// ③ 上游 401 → 客户端 503（与 recId=14 错误传播一致：
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
            "上游 401 必须映射 503（VendorUnavailable，与 recId=14 同口径）"
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
}

// ============ recId=12：精确响应缓存（oxcache，租户隔离键 + 确定性门控） ============

#[cfg(test)]
mod response_cache_tests {
    use axum::{body::Body, http::Request, routing::post, Router};
    use nexusflow::api::{chat_completions, response_cache::ResponseCache};
    use nexusflow::config::{
        CacheSection, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        ServerConfig, VendorConfig,
    };
    use nexusflow::database::Database;
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::Router as NexusRouter;
    use nexusflow::vendor::VendorManager;
    use nexusflow::ApiState;
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn ensure_test_api_key() {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    }

    fn make_vendor(api_base: String) -> VendorConfig {
        ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: "cached-openai".into(),
            name: "Cached OpenAI".into(),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base,
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "target-model".into(),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: nexusflow::config::RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }
    }

    /// [cache] 显式开启（enabled=true, ttl=300）——缓存行为测试的前提
    fn make_cache_enabled_config(api_base: String) -> Config {
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![make_vendor(api_base)],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
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
                Arc::new(nexusflow::utils::metrics::GlobalMetrics::new()),
            ))),
            bandit: None,
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

// ============ recId=15（评审修复 major）：handler→奖励臂键全链 ============

#[cfg(test)]
mod linucb_reward_chain {
    //! recId=15（评审修复 major）：handler→奖励臂键全链回归——
    //! 修复前奖励写原始 request.model（("GPT-4"/"auto", vendor) 与被咨询
    //! 臂 ("gpt-4"/"__global__", vendor) 永不相交），且 grep 全 tests/
    //! 无 record_linucb_reward 调用、handler→奖励链路零覆盖。

    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, RoutingSection,
        ServerConfig, VendorConfig,
    };
    use nexusflow::database::Database;
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::bandit::BanditState;
    use nexusflow::vendor::VendorManager;
    use nexusflow::ApiState;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn linucb_config() -> Config {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![VendorConfig {
                timeout_ms: 2_000,
                stream_ttft_ms: 2_000,
                stream_idle_ms: 2_000,
                id: "openai_main".to_string(),
                name: "OpenAI Main".into(),
                type_: "openai".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
                // 拒绝端口：路由成功、执行快速失败 → 奖励 r=0 落账
                api_base: "http://127.0.0.1:1".into(),
                enabled: true,
                priority: 1,
                models: vec![ModelConfig {
                    model_id: "gpt-4".to_string(),
                    abstract_id: Some("gpt-4".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: nexusflow::config::RateLimitConfig::Token {
                    max_tokens: 1_000_000,
                    window_hours: None,
                    sliding: false,
                },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 1000,
                    max_concurrent_requests: None,
                    token_weighted: false,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            // auth: None → 放行（测试聚焦奖励链路而非认证）
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
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
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
        });
        let bandit = Arc::new(BanditState::new(1.0));
        let router = Arc::new(
            nexusflow::router::Router::new(config_manager.clone(), vendor_manager.clone())
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
        });
        (state, bandit)
    }

    fn app(state: Arc<ApiState>) -> axum::Router {
        use axum::routing::post;
        axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(nexusflow::api::openai::chat_completions),
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

    /// 评审修复 major 回归（handler→奖励全链，修复前零覆盖）：
    /// 1) 大写 model：选择键归一 "gpt-4"，奖励必须落同键
    ///    （修复前写 ("GPT-4", vendor) 与被咨询臂永不相交）；
    /// 2) auto 路径：选择走 "__global__" 臂，奖励必须同键
    ///    （修复前写 ("auto", vendor)——主路径永不学习）；
    /// 3) 失败奖励（r=0）压低被咨询臂 UCB（学习闭环生效）。
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
