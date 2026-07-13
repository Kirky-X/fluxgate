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
