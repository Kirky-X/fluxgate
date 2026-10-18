#![cfg(feature = "web-server")]
// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `/v1/responses` 端到端回归：IR 路径、流式事件、校验与错误映射。

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use axum::{
        body::Body,
        http::Request,
        routing::{get, post},
        Router,
    };
    use fluxgate::api::responses::{
        cancel_response, create_response, delete_response, get_response, input_tokens,
    };
    use fluxgate::config::{ConfigManager, ModelConfig, RateLimitConfig, VendorConfig};
    use fluxgate::database::{Repository, TokenPreAggregator};
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn chat_vendor(id: &str, api_base: String) -> VendorConfig {
        common::ensure_test_api_key();
        let mut vendor = common::base_vendor(id, api_base);
        vendor.models = vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        vendor.rate_limit = RateLimitConfig::Token {
            max_tokens: 1_000_000,
            window_hours: None,
            sliding: false,
        };
        vendor.concurrency.requests_per_second = 100;
        vendor
    }

    async fn make_state(vendors: Vec<VendorConfig>) -> Arc<ApiState> {
        make_state_full(vendors, false).await.0
    }

    async fn make_state_full(
        vendors: Vec<VendorConfig>,
        with_logger: bool,
    ) -> (Arc<ApiState>, Arc<Repository>) {
        let config = common::test_config(vendors);
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

        let db_sink = Arc::new(fluxgate::database::Database {
            pool,
            repository: repository.clone(),
        });

        let state = Arc::new(ApiState {
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: db_sink.clone(),
            logger: with_logger
                .then(|| db_sink.clone() as Arc<dyn fluxgate::storage::RequestLogSink>),
            pre_aggregator: Some(pre_aggregator),
            affinity: Some(Arc::new(
                fluxgate::router::affinity::AffinityTable::with_memory_backend(
                    std::time::Duration::from_secs(3600),
                ),
            )),
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: Some(Arc::new(fluxgate::api::responses::ResponsesRuntime::new(
                repository.clone(),
            ))),
        });
        (state, repository)
    }

    fn responses_vendor(id: &str, api_base: String) -> VendorConfig {
        let mut vendor = chat_vendor(id, api_base);
        vendor.api_style = Some("responses".into());
        vendor
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/responses", post(create_response))
            .route("/v1/responses/input_tokens", post(input_tokens))
            .route(
                "/v1/responses/{id}",
                get(get_response).delete(delete_response),
            )
            .route("/v1/responses/{id}/cancel", post(cancel_response))
            .with_state(state)
    }

    async fn post_responses(state: Arc<ApiState>, body: Value) -> axum::response::Response {
        app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("JSON 响应")
    }

    #[tokio::test]
    async fn non_stream_text_response_maps_to_responses_shape() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"chatcmpl_1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let response = post_responses(
            state,
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16}),
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = response_json(response).await;
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(body["output"][0]["content"][0]["text"], "hi");
        assert_eq!(body["usage"]["input_tokens"], 3);
        assert_eq!(body["usage"]["output_tokens"], 1);
    }

    #[tokio::test]
    async fn stream_emits_semantic_events() {
        let mut server = mockito::Server::new_async().await;
        let sse_body = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        );
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_body)
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let response = post_responses(
            state,
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16, "stream": true}),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for expected in [
            "event: response.created",
            "event: response.in_progress",
            "event: response.output_item.added",
            "event: response.content_part.added",
            "event: response.output_text.delta",
            "event: response.output_text.done",
            "event: response.content_part.done",
            "event: response.output_item.done",
            "event: response.completed",
        ] {
            assert!(text.contains(expected), "缺少事件 {expected}:\n{text}");
        }
        assert!(!text.contains("[DONE]"), "Responses 流不发 [DONE]");
        assert!(
            text.contains(r#""input_tokens":3"#),
            "终止事件必须携带 usage:\n{text}"
        );
    }

    #[tokio::test]
    async fn previous_response_id_replays_history() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        // 第二条 mock 先建：按 "follow up" 命中，避免被第一条宽匹配抢走
        let second = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::Regex("follow up".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c2","object":"chat.completion","created":2,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"second"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
            )
            .expect(1)
            .create();
        let first = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::Regex("hello".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"first"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let first_response = post_responses(
            state.clone(),
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16}),
        )
        .await;
        let first_body = response_json(first_response).await;
        let previous_id = first_body["id"].as_str().expect("响应 id").to_string();
        // 直接核验落库（避免 404 归因不明）
        let tenant = fluxgate::utils::tenant::api_key_digest(&axum::http::HeaderMap::new());
        let stored = state
            .responses
            .as_ref()
            .expect("runtime")
            .store
            .get_response(&previous_id, &tenant, 0)
            .await
            .expect("查询存储");
        assert!(stored.is_some(), "首轮响应必须已落库");

        let second_response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "follow up",
                "max_output_tokens": 16,
                "previous_response_id": previous_id
            }),
        )
        .await;
        assert_eq!(second_response.status(), 200);
        let second_body = response_json(second_response).await;
        assert_eq!(second_body["output"][0]["content"][0]["text"], "second");
        first.assert();
        second.assert();
    }

    #[tokio::test]
    async fn previous_and_conversation_conflict_is_400() {
        let state = make_state(Vec::new()).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "previous_response_id": "resp_1",
                "conversation": "conv_1"
            }),
        )
        .await;
        assert_eq!(response.status(), 400, "互斥字段必须显式拒绝");
    }

    #[tokio::test]
    async fn store_false_does_not_persist() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"first"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let first_response = post_responses(
            state.clone(),
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16, "store": false}),
        )
        .await;
        let first_body = response_json(first_response).await;
        assert_eq!(first_body["store"], false, "store=false 必须回显");
        let previous_id = first_body["id"].as_str().unwrap().to_string();

        let second_response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "follow up",
                "max_output_tokens": 16,
                "previous_response_id": previous_id
            }),
        )
        .await;
        assert_eq!(second_response.status(), 404, "未持久化的 id 必须 404");
    }

    #[tokio::test]
    async fn native_only_without_native_vendor_is_400() {
        let state = make_state(vec![chat_vendor("v1", "http://127.0.0.1:1/v1".into())]).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(response.status(), 400, "无原生候选必须显式拒绝");
    }

    #[tokio::test]
    async fn native_only_passes_through_to_responses_upstream() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/responses")
            .match_body(Matcher::Regex("web_search".into()))
            .match_body(Matcher::Regex(r#""store":false"#.into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"resp_up","object":"response","created_at":1,"status":"completed",
                    "model":"gpt-4",
                    "output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant",
                        "content":[{"type":"output_text","text":"native","annotations":[]}]}],
                    "usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;

        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = response_json(response).await;
        assert_eq!(body["output"][0]["content"][0]["text"], "native");
        mock.assert();
    }

    #[tokio::test]
    async fn native_hint_to_chat_vendor_is_502_without_upstream_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("POST", "/v1/responses").expect(0).create();
        let state = make_state(vec![
            chat_vendor("chat-only", format!("{}/v1", server.url())),
            responses_vendor("rr", format!("{}/v1", server.url())),
        ])
        .await;

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .header("x-vendor", "chat-only")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4",
                            "input": "x",
                            "max_output_tokens": 16,
                            "tools": [{"type": "web_search"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 502, "点名非原生厂商必须拒绝");
        mock.assert();
    }

    #[tokio::test]
    async fn get_and_delete_stored_response() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"first"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let created = post_responses(
            state.clone(),
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16}),
        )
        .await;
        let created_body = response_json(created).await;
        let id = created_body["id"].as_str().unwrap().to_string();

        let fetched = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), 200);
        let fetched_body = response_json(fetched).await;
        assert_eq!(fetched_body["id"], id);

        let deleted = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), 200);
        let deleted_body = response_json(deleted).await;
        assert_eq!(deleted_body["deleted"], true);

        let gone = app(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gone.status(), 404);
    }

    #[tokio::test]
    async fn cancel_non_background_is_400() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"first"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        let created = post_responses(
            state.clone(),
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16}),
        )
        .await;
        let id = response_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/responses/{id}/cancel"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "非后台响应不可取消");
    }

    #[tokio::test]
    async fn input_tokens_estimates_and_validates() {
        let state = make_state(Vec::new()).await;
        let post = |body: Value| {
            let state = state.clone();
            async move {
                app(state)
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/responses/input_tokens")
                            .header("content-type", "application/json")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        let response = post(json!({"model": "gpt-4", "input": "hello world"})).await;
        assert_eq!(response.status(), 200);
        let body = response_json(response).await;
        assert!(body["input_tokens"].as_u64().unwrap() >= 1);

        let missing_model = post(json!({"input": "x"})).await;
        assert_eq!(missing_model.status(), 400);

        let empty_input = post(json!({"model": "gpt-4", "input": ""})).await;
        assert_eq!(empty_input.status(), 400);
    }

    fn chat_app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route(
                "/v1/chat/completions",
                post(fluxgate::api::openai::chat_completions),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn chat_client_hits_responses_style_upstream() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/responses")
            .match_body(Matcher::Regex("gpt-4".into()))
            .match_body(Matcher::Regex(r#""input""#.into()))
            .match_body(Matcher::Regex(r#""store":false"#.into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"resp_up","object":"response","created_at":1,"status":"completed",
                    "model":"gpt-4",
                    "output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant",
                        "content":[{"type":"output_text","text":"hi from responses","annotations":[]}]}],
                    "usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;

        let response = chat_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response_json(response).await;
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "hi from responses"
        );
        assert_eq!(body["usage"]["prompt_tokens"], 2);
        assert_eq!(body["usage"]["completion_tokens"], 3);
        mock.assert();
    }

    #[tokio::test]
    async fn chat_stream_client_hits_responses_style_upstream() {
        let mut server = mockito::Server::new_async().await;
        let sse_body = concat!(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"model\":\"gpt-4\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":1,\"total_tokens\":3}}}\n\n"
        );
        server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_body)
            .expect(1)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;

        let response = chat_app(state)
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
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("chat.completion.chunk"),
            "应回转 chat chunk: {text}"
        );
        assert!(text.contains("hi"), "应携带文本增量: {text}");
        assert!(text.contains("[DONE]"), "chat 客户端以 [DONE] 收尾: {text}");
    }

    #[tokio::test]
    async fn background_job_completes_and_is_pollable() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"bg"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;

        let created = post_responses(
            state.clone(),
            json!({
                "model": "gpt-4",
                "input": "hello",
                "max_output_tokens": 16,
                "background": true,
                "store": true
            }),
        )
        .await;
        assert_eq!(created.status(), 200);
        let created_body = response_json(created).await;
        assert_eq!(created_body["status"], "queued");
        let id = created_body["id"].as_str().unwrap().to_string();

        let mut completed = None;
        for _ in 0..100 {
            let fetched = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/v1/responses/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = response_json(fetched).await;
            if body["status"] == "completed" {
                completed = Some(body);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let completed = completed.expect("后台任务应在轮询内完成");
        assert_eq!(completed["output"][0]["content"][0]["text"], "bg");
    }

    #[tokio::test]
    async fn background_without_store_is_400() {
        let state = make_state(Vec::new()).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "background": true,
                "store": false
            }),
        )
        .await;
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn cancel_background_job_marks_cancelled() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"bg"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        let created = post_responses(
            state.clone(),
            json!({
                "model": "gpt-4",
                "input": "hello",
                "max_output_tokens": 16,
                "background": true,
                "store": true
            }),
        )
        .await;
        let id = response_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let cancelled = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/responses/{id}/cancel"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), 200);
        let body = response_json(cancelled).await;
        assert_eq!(body["status"], "cancelled");
    }

    #[tokio::test]
    async fn native_missing_output_is_502() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"resp_bad"}"#)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(response.status(), 502, "缺 output 必须显式 502");
    }

    #[tokio::test]
    async fn failure_request_is_logged() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .create();
        let (state, repository) = make_state_full(
            vec![chat_vendor("v1", format!("{}/v1", server.url()))],
            true,
        )
        .await;

        let response = post_responses(
            state,
            json!({"model": "gpt-4", "input": "hello", "max_output_tokens": 16}),
        )
        .await;
        assert_eq!(response.status(), 503);

        let logs = repository
            .query_logs(&fluxgate::database::LogFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("日志查询");
        let failed = logs
            .iter()
            .find(|log| log.endpoint == "/v1/responses")
            .expect("失败请求必须落日志");
        assert_eq!(
            failed.status_code,
            Some(500),
            "日志记录上游真实状态码（客户端仍收 503）"
        );
        assert!(failed.error_message.is_some());
    }

    #[tokio::test]
    async fn validation_errors_are_400() {
        let state = make_state(Vec::new()).await;
        // 缺 model
        let response = post_responses(state.clone(), json!({"input": "x"})).await;
        assert_eq!(response.status(), 400);
        // max_output_tokens 越界
        let response = post_responses(
            state,
            json!({"model": "gpt-4", "input": "x", "max_output_tokens": 0}),
        )
        .await;
        assert_eq!(response.status(), 400);
    }
}
