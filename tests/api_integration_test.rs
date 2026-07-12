// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use nexusflow::api::ApiState;
    use nexusflow::config::{ConfigManager, ServerConfig, DatabaseConfig, VendorConfig, ModelConfig, GlobalModelConfig, Config};
    use nexusflow::protocol::{OpenAIConverter, AnthropicConverter, ProtocolConverter};
    use nexusflow::vendor::VendorManager;
    use nexusflow::database::Database;
    use nexusflow::router::Router;
    use std::sync::Arc;
    use serde_json::json;
    use sqlx::SqlitePool;

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
                    models: vec![
                        ModelConfig {
                            model_id: "gpt-4".to_string(),
                            abstract_id: Some("gpt-4".to_string()),
                            weight: 1.0,
                            enabled: true,
                        },
                    ],
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

    fn create_mock_api_state() -> Arc<ApiState> {
        let config = create_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        
        let router = Arc::new(Router::new(
            config_manager.clone(),
            vendor_manager.clone()
        ));
        
        let database = Arc::new(Database {
            pool: SqlitePool::connect_lazy(":memory:").unwrap(),
            repository: Arc::new(nexusflow::database::Repository::new(
                SqlitePool::connect_lazy(":memory:").unwrap(),
            )),
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
        let state = create_mock_api_state();
        
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
        let state = create_mock_api_state();
        
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
            choices: vec![
                nexusflow::protocol::unified::Choice {
                    index: 0,
                    message: Some(nexusflow::protocol::unified::Message {
                        role: nexusflow::protocol::unified::Role::Assistant,
                        content: nexusflow::protocol::unified::MessageContent::Text("Hello!".to_string()),
                    }),
                    finish_reason: Some("stop".to_string()),
                    logprobs: None,
                }
            ],
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
