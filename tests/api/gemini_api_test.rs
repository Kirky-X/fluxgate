// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! Gemini 原生入站端点回归：IR 路由、流式事件、原生直通、鉴权、Embeddings
//! 与模型清单。

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware::from_fn_with_state,
        routing::{get, post},
        Router,
    };
    use fluxgate::api::gemini::{dispatch, get_model, list_models};
    use fluxgate::config::{AuthSection, ConfigManager, ModelConfig, VendorConfig};
    use fluxgate::database::{Repository, TokenPreAggregator};
    use fluxgate::middleware::auth::auth_middleware;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use mockito::Matcher;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn digest(key: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(key.as_bytes()))
    }

    fn vendor(
        id: &str,
        type_: &str,
        api_base: &str,
        model_id: &str,
        abstract_id: &str,
        model_type: Option<&str>,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        let mut vendor = common::base_vendor(id, api_base);
        vendor.type_ = type_.into();
        vendor.models = vec![ModelConfig {
            model_id: model_id.into(),
            abstract_id: Some(abstract_id.into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: model_type.map(str::to_string),
        }];
        vendor.concurrency.requests_per_second = 1000;
        vendor
    }

    async fn make_state(vendors: Vec<VendorConfig>, auth: Option<AuthSection>) -> Arc<ApiState> {
        let mut config = common::test_config(vendors);
        config.auth = auth;
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
            router: Arc::new(NexusRouter::new(config_manager.clone(), vendor_manager)),
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
            .route("/v1beta/models", get(list_models))
            .route(
                "/v1beta/models/{model_action}",
                post(dispatch).get(get_model),
            )
            .with_state(state)
    }

    fn authed_app(state: Arc<ApiState>, key: &str) -> Router {
        let mut config = (*state.config.read()).clone();
        config.auth = Some(AuthSection {
            enabled: true,
            api_keys: vec![digest(key)],
            keys: vec![],
            header: Default::default(),
        });
        let config_manager = Arc::new(ConfigManager::new(config));
        Router::new()
            .route("/v1beta/models", get(list_models))
            .route(
                "/v1beta/models/{model_action}",
                post(dispatch).get(get_model),
            )
            .route("/v1/chat/completions", post(|| async { "ok" }))
            .layer(from_fn_with_state(config_manager, auth_middleware))
            .with_state(state)
    }

    async fn post_json(
        app: Router,
        path: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = app
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn openai_chat_response() -> Value {
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "gemini-2.5-pro",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
        })
    }

    #[tokio::test]
    async fn generate_content_ir_path_routes_to_openai_vendor() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "messages": [{"role": "user", "content": "hi"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(openai_chat_response().to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai",
                "openai",
                &server.url(),
                "gpt-4",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["candidates"][0]["content"]["parts"][0]["text"],
            "hello"
        );
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");
        assert_eq!(body["usageMetadata"]["promptTokenCount"], 3);
        assert_eq!(body["usageMetadata"]["candidatesTokenCount"], 2);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn stream_generate_content_emits_gemini_sse_without_done() {
        let mut server = mockito::Server::new_async().await;
        let sse = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"he\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"total_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        );
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse)
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai",
                "openai",
                &server.url(),
                "gpt-4",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\"text\":\"he\""), "{text}");
        assert!(text.contains("\"text\":\"llo\""), "{text}");
        assert!(text.contains("\"finishReason\":\"STOP\""), "{text}");
        assert!(text.contains("\"usageMetadata\""), "{text}");
        assert!(!text.contains("[DONE]"), "Gemini 无 [DONE]: {text}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn tools_and_response_schema_cross_to_openai() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "tools": [{"type": "function", "function": {"name": "get_weather"}}],
                "response_format": {"type": "json_schema"}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(openai_chat_response().to_string())
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai",
                "openai",
                &server.url(),
                "gpt-4",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({
                "contents": [{"role": "user", "parts": [{"text": "weather?"}]}],
                "tools": [{"functionDeclarations": [{
                    "name": "get_weather",
                    "description": "weather",
                    "parameters": {"type": "object"}
                }]}],
                "toolConfig": {"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["get_weather"]}},
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseSchema": {"type": "object"}
                }
            }),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn xvendor_hint_pins_low_weight_vendor_on_ir_path() {
        let mut cheap = mockito::Server::new_async().await;
        let mut expensive = mockito::Server::new_async().await;
        let cheap_mock = cheap
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(openai_chat_response().to_string())
            .expect(1)
            .create_async()
            .await;
        let expensive_mock = expensive
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(openai_chat_response().to_string())
            .expect(0)
            .create_async()
            .await;
        let mut low = vendor(
            "cheap",
            "openai",
            &cheap.url(),
            "gpt-4",
            "gemini-2.5-pro",
            None,
        );
        low.models[0].weight = 0.001;
        let mut high = vendor(
            "expensive",
            "openai",
            &expensive.url(),
            "gpt-4",
            "gemini-2.5-pro",
            None,
        );
        high.models[0].weight = 100.0;
        let state = make_state(vec![low, high], None).await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}),
            &[("X-Vendor", "cheap"), ("X-Model", "gpt-4")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        cheap_mock.assert_async().await;
        expensive_mock.assert_async().await;
    }

    #[tokio::test]
    async fn native_only_without_gemini_vendor_is_explicit_400() {
        let state = make_state(vec![], None).await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "safetySettings": []
            }),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("Gemini"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn native_only_xvendor_must_be_gemini() {
        let state = make_state(
            vec![vendor(
                "openai",
                "openai",
                "http://127.0.0.1:1",
                "gpt-4",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "safetySettings": []
            }),
            &[("X-Vendor", "openai")],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("openai"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn native_only_passthrough_preserves_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock(
                "POST",
                "/v1beta/models/gemini-2.5-pro:generateContent",
            )
            .match_body(Matcher::PartialJson(json!({
                "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_LOW_AND_ABOVE"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "candidates": [{"content": {"role": "model", "parts": [{"text": "ok"}]}, "finishReason": "STOP"}],
                    "modelVersion": "gemini-2.5-pro",
                    "responseId": "r-1"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "gemini",
                "gemini",
                &server.url(),
                "gemini-2.5-pro",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_LOW_AND_ABOVE"}]
            }),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["candidates"][0]["content"]["parts"][0]["text"], "ok");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn native_only_stream_passthrough_forwards_gemini_sse() {
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
        let state = make_state(
            vec![vendor(
                "gemini",
                "gemini",
                &server.url(),
                "gemini-2.5-pro",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                            "safetySettings": []
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
        assert!(text.contains("\"text\":\"he\""), "{text}");
        assert!(text.contains("\"text\":\"llo\""), "{text}");
        assert!(text.contains("\"usageMetadata\""), "{text}");
        assert!(!text.contains("[DONE]"), "{text}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn count_tokens_is_local_and_requires_contents() {
        let state = make_state(vec![], None).await;
        let (status, body) = post_json(
            app(state.clone()),
            "/v1beta/models/gemini-2.5-pro:countTokens",
            json!({"contents": [{"role": "user", "parts": [{"text": "hello world"}]}]}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["totalTokens"].as_u64().unwrap() > 0);

        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:countTokens",
            json!({}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
    }

    #[tokio::test]
    async fn models_list_deduplicates_and_detail_404s() {
        let state = make_state(
            vec![
                vendor("a", "openai", "http://127.0.0.1:1", "gpt-4", "shared", None),
                vendor(
                    "b",
                    "openai",
                    "http://127.0.0.1:1",
                    "gpt-4o",
                    "shared",
                    None,
                ),
                vendor(
                    "e",
                    "openai",
                    "http://127.0.0.1:1",
                    "emb",
                    "embed-1",
                    Some("embedding"),
                ),
            ],
            None,
        )
        .await;
        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/v1beta/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let models = body["models"].as_array().unwrap();
        assert_eq!(models.len(), 2, "同名 abstract 去重: {body}");
        assert!(models.iter().any(|m| m["name"] == "models/shared"));
        let embed = models
            .iter()
            .find(|m| m["name"] == "models/embed-1")
            .unwrap();
        assert_eq!(embed["supportedGenerationMethods"][0], "embedContent");

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/v1beta/models/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn embed_content_wraps_openai_embeddings() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .match_body(Matcher::PartialJson(json!({
                "model": "embed-1",
                "input": "hello"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "object": "list",
                    "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
                    "model": "embed-1",
                    "usage": {"prompt_tokens": 2, "total_tokens": 2}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai-embed",
                "openai",
                &server.url(),
                "embed-1",
                "embed-1",
                Some("embedding"),
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/embed-1:embedContent",
            json!({"content": {"parts": [{"text": "hello"}]}}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["embedding"]["values"], json!([0.1, 0.2]));
        assert_eq!(body["usageMetadata"]["promptTokenCount"], 2);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn batch_embed_contents_returns_ordered_vectors() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .match_body(Matcher::PartialJson(json!({
                "model": "embed-1",
                "input": ["a", "b"]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "object": "list",
                    "data": [
                        {"object": "embedding", "index": 0, "embedding": [0.1]},
                        {"object": "embedding", "index": 1, "embedding": [0.2]}
                    ],
                    "model": "embed-1",
                    "usage": {"prompt_tokens": 2, "total_tokens": 2}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai-embed",
                "openai",
                &server.url(),
                "embed-1",
                "embed-1",
                Some("embedding"),
            )],
            None,
        )
        .await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/embed-1:batchEmbedContents",
            json!({"requests": [
                {"content": {"parts": [{"text": "a"}]}},
                {"content": {"parts": [{"text": "b"}]}}
            ]}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let embeddings = body["embeddings"].as_array().unwrap();
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0]["values"], json!([0.1]));
        assert_eq!(embeddings[1]["values"], json!([0.2]));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn invalid_body_returns_gemini_error_shape() {
        let state = make_state(vec![], None).await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            json!({}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], 400);
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let state = make_state(vec![], None).await;
        let (status, body) = post_json(
            app(state),
            "/v1beta/models/gemini-2.5-pro:predict",
            json!({"contents": []}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("predict"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn auth_accepts_goog_header_and_query_key() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(openai_chat_response().to_string())
            .expect(2)
            .create_async()
            .await;
        let state = make_state(
            vec![vendor(
                "openai",
                "openai",
                &server.url(),
                "gpt-4",
                "gemini-2.5-pro",
                None,
            )],
            None,
        )
        .await;
        let app = authed_app(state, "test-key-123");
        let body = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});

        let (status, response) = post_json(
            app.clone(),
            "/v1beta/models/gemini-2.5-pro:generateContent",
            body.clone(),
            &[("x-goog-api-key", "test-key-123")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");

        let (status, response) = post_json(
            app.clone(),
            "/v1beta/models/gemini-2.5-pro:generateContent?key=test-key-123",
            body,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");

        // 非 /v1beta 路径不接受 query key
        let (status, _) =
            post_json(app, "/v1/chat/completions?key=test-key-123", json!({}), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        mock.assert_async().await;
    }
}
