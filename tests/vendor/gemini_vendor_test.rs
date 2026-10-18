// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! `type_ = "gemini"` 出站回归：OpenAI 客户端 → Gemini 上游（非流式/流式/
//! 工具/结构化输出）。

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use fluxgate::api::completions::chat_completions;
    use fluxgate::config::{ConfigManager, ModelConfig, VendorConfig};
    use fluxgate::database::{Repository, TokenPreAggregator};
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use mockito::Matcher;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn gemini_vendor(api_base: &str) -> VendorConfig {
        common::ensure_test_api_key();
        let mut vendor = common::base_vendor("gemini", api_base);
        vendor.type_ = "gemini".into();
        vendor.models = vec![ModelConfig {
            model_id: "gemini-2.5-pro".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        vendor.concurrency.requests_per_second = 1000;
        vendor
    }

    async fn make_state(vendor: VendorConfig) -> Arc<ApiState> {
        let config = common::test_config(vec![vendor]);
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
        let pre_aggregator = Arc::new(TokenPreAggregator::new(
            repository.clone(),
            std::time::Duration::from_secs(3600),
        ));
        let database = Arc::new(fluxgate::database::Database {
            pool,
            repository: repository.clone(),
        });
        Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: Some(pre_aggregator),
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

    async fn post_json(app: Router, body: Value) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn gemini_response() -> Value {
        json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hello"}]},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 2,
                "totalTokenCount": 5
            },
            "modelVersion": "gemini-2.5-pro",
            "responseId": "r-1"
        })
    }

    #[tokio::test]
    async fn gemini_missing_model_version_falls_back_to_request_model() {
        let mut server = mockito::Server::new_async().await;
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hello"}]},
                "finishReason": "STOP"
            }]
        });
        let mock = server
            .mock("POST", "/v1beta/models/gemini-2.5-pro:generateContent")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(gemini_vendor(&server.url())).await;
        let (status, body) = post_json(
            app(state),
            json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["model"], "gpt-4", "缺 modelVersion 时回退请求 model");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_client_reaches_gemini_vendor() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1beta/models/gemini-2.5-pro:generateContent")
            .match_header("x-goog-api-key", common::TEST_API_KEY_VALUE)
            .match_body(Matcher::PartialJson(json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {"maxOutputTokens": 32}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gemini_response().to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(gemini_vendor(&server.url())).await;
        let (status, body) = post_json(
            app(state),
            json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 32
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["choices"][0]["message"]["content"], "hello");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["prompt_tokens"], 3);
        assert_eq!(body["usage"]["completion_tokens"], 2);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_stream_reaches_gemini_stream() {
        let mut server = mockito::Server::new_async().await;
        let sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"he\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"llo\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":2,\"totalTokenCount\":4}}\n\n"
        );
        let mock = server
            .mock(
                "POST",
                Matcher::Regex(
                    r"^/v1beta/models/gemini-2\.5-pro:streamGenerateContent\?alt=sse$".into(),
                ),
            )
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse)
            .expect(1)
            .create_async()
            .await;
        let state = make_state(gemini_vendor(&server.url())).await;
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\"content\":\"he\""), "{text}");
        assert!(text.contains("\"content\":\"llo\""), "{text}");
        assert!(text.contains("[DONE]"), "{text}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_tools_map_to_function_declarations() {
        let mut server = mockito::Server::new_async().await;
        let function_call = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "get_weather", "args": {"city": "SH"}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 3, "totalTokenCount": 7}
        });
        let mock = server
            .mock("POST", "/v1beta/models/gemini-2.5-pro:generateContent")
            .match_body(Matcher::PartialJson(json!({
                "tools": [{"functionDeclarations": [{"name": "get_weather"}]}],
                "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(function_call.to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(gemini_vendor(&server.url())).await;
        let (status, body) = post_json(
            app(state),
            json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "weather?"}],
                "tools": [{"type": "function", "function": {
                    "name": "get_weather",
                    "description": "weather",
                    "parameters": {"type": "object"}
                }}],
                "tool_choice": "auto"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "get_weather",
            "{body}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_response_format_maps_to_response_schema() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1beta/models/gemini-2.5-pro:generateContent")
            .match_body(Matcher::PartialJson(json!({
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseSchema": {"type": "object"}
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gemini_response().to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(gemini_vendor(&server.url())).await;
        let (status, body) = post_json(
            app(state),
            json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {"name": "out", "schema": {"type": "object"}}
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        mock.assert_async().await;
    }
}
