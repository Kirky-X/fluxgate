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
    use fluxweir::api::responses::{
        cancel_response, create_response, delete_response, get_response, input_tokens,
    };
    use fluxweir::config::{ConfigManager, ModelConfig, RateLimitConfig, VendorConfig};
    use fluxweir::database::{Repository, TokenPreAggregator};
    use fluxweir::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxweir::router::Router as NexusRouter;
    use fluxweir::vendor::VendorManager;
    use fluxweir::ApiState;
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

        let db_sink = Arc::new(fluxweir::database::Database {
            pool,
            repository: repository.clone(),
        });

        let state = Arc::new(ApiState {
            affinity_writer: None,
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: db_sink.clone(),
            logger: with_logger
                .then(|| db_sink.clone() as Arc<dyn fluxweir::storage::RequestLogSink>),
            pre_aggregator: Some(pre_aggregator),
            affinity: Some(Arc::new(
                fluxweir::router::affinity::AffinityTable::with_memory_backend(
                    std::time::Duration::from_secs(3600),
                ),
            )),
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            #[cfg(feature = "web-search")]
            web_search: None,
            responses: Some(Arc::new(fluxweir::api::responses::ResponsesRuntime::new(
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
        let tenant = fluxweir::utils::tenant::api_key_digest(&axum::http::HeaderMap::new());
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
                post(fluxweir::api::completions::chat_completions),
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
            .query_logs(&fluxweir::database::LogFilter {
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

    /// 后台槽位计数的纯单元语义：限额占用、释放、abort 取消。
    #[tokio::test]
    async fn background_jobs_slot_semantics() {
        use fluxweir::api::responses::ResponsesBackgroundJobs;
        let jobs = ResponsesBackgroundJobs::default();
        assert!(jobs.try_begin(2), "首个槽位必须可占用");
        assert!(jobs.try_begin(2), "第二个槽位必须可占用");
        assert!(!jobs.try_begin(2), "超限必须拒绝");
        jobs.end();
        assert_eq!(jobs.active_count(), 1, "释放后计数递减");
        assert!(jobs.try_begin(2), "释放后的槽位可复用");

        // register + cancel：登记句柄可被 abort，未登记 id 的 cancel 是 no-op
        let handle =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        jobs.register("job_1", handle.abort_handle());
        jobs.cancel("job_1");
        let joined = handle
            .await
            .expect_err("cancel 后任务必须以 cancelled 收场");
        assert!(joined.is_cancelled(), "abort 语义: {joined:?}");
        jobs.cancel("job_absent"); // no-op，不 panic
        jobs.remove("job_1"); // 已被 cancel 移除，再 remove 亦 no-op
    }

    /// [responses].enabled=false 时端点整体 404。
    #[tokio::test]
    async fn responses_disabled_returns_404() {
        let state = make_state(Vec::new()).await;
        let mut config = state.config.get_config();
        config.responses.enabled = false;
        // 测试态 ConfigManager 无磁盘路径（update_config 拒绝持久化），
        // 走热载桥接的纯内存切换
        state.config.swap_memory(config).expect("关闭 responses 段");
        let response = post_responses(state, json!({"model": "gpt-4", "input": "x"})).await;
        assert_eq!(response.status(), 404, "disabled 必须整端点 404");
    }

    /// native-only 请求（内置工具）与 background 组合必须显式拒绝。
    #[tokio::test]
    async fn native_background_combo_is_400() {
        let state = make_state(Vec::new()).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "x",
                "max_output_tokens": 16,
                "background": true,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(response.status(), 400, "native-only 不支持后台模式");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("not supported in background"),
            "错误信息必须说明限制:\n{text}"
        );
    }

    /// 原生流透传：上游 SSE 字节原样透出，response.completed 的完整响应
    /// 被 observe 回调捕获并异步持久化（GET 回读验证）。
    #[tokio::test]
    async fn native_stream_passthrough_persists_completed() {
        let mut server = mockito::Server::new_async().await;
        let completed = r#"{"type":"response.completed","response":{"id":"resp_nat_s","object":"response","created_at":1,"status":"completed","model":"native-m","output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"streamed","annotations":[]}]}],"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}"#;
        let sse_body = format!(
            "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            r#"{"type":"response.created","response":{"id":"resp_nat_s","object":"response","status":"in_progress"}}"#,
            completed
        );
        server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_body)
            .expect(1)
            .create();
        let mut native = responses_vendor("rr", format!("{}/v1", server.url()));
        native.models = vec![ModelConfig {
            model_id: "native-m".into(),
            abstract_id: Some("native-m".into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        let state = make_state(vec![native]).await;

        let response = post_responses(
            state.clone(),
            json!({
                "model": "native-m",
                "input": "x",
                "max_output_tokens": 16,
                "stream": true,
                "store": true,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("event: response.completed"),
            "原生流字节必须原样透传:\n{text}"
        );
        assert!(
            text.contains("\"id\":\"resp_nat_s\""),
            "completed 帧的响应体必须透传:\n{text}"
        );

        // 持久化在 spawn 的后台任务中完成——有界轮询 GET 回读
        let mut fetched = None;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let r = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/v1/responses/resp_nat_s")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if r.status() == 200 {
                fetched = Some(response_json(r).await);
                break;
            }
        }
        let body = fetched.expect("completed 响应必须在流结束后可回读");
        assert_eq!(body["id"], "resp_nat_s");
        assert_eq!(body["status"], "completed");
    }

    /// 原生路径上游失败：vendor 层不可用语义映射 503 且不吞错误。
    #[tokio::test]
    async fn native_upstream_failure_maps_to_503() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/responses")
            .with_status(500)
            .with_body("boom")
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
        assert_eq!(
            response.status(),
            503,
            "上游 500 映射为 503（vendor 层不可用语义）"
        );
    }

    /// x-model 点名无原生能力的模型：路由前 gate 直接 502（Protocol 语义，不发上游）。
    #[tokio::test]
    async fn native_hint_preferred_model_gate_is_502() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("POST", "/v1/responses").expect(0).create();
        let mut native = responses_vendor("rr", format!("{}/v1", server.url()));
        native.models = vec![ModelConfig {
            model_id: "native-m".into(),
            abstract_id: Some("native-m".into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        let state = make_state(vec![
            chat_vendor("chat-only", format!("{}/v1", server.url())),
            native,
        ])
        .await;

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .header("x-model", "gpt-4")
                    .body(Body::from(
                        json!({
                            "model": "native-m",
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
        assert_eq!(
            response.status(),
            502,
            "x-model 指向无 responses 能力的模型必须在 gate 拒绝（Protocol 语义 502，不发上游）"
        );
        mock.assert();
    }

    /// native + previous_response_id：回放历史合并进改写后的 input 打上游，
    /// 且首响经非流式 store 路径持久化。
    #[tokio::test]
    async fn native_previous_response_id_rewrites_input() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        // 第二轮回放请求：body 必含首轮的 assistant 文本
        let replay = server
            .mock("POST", "/v1/responses")
            .match_body(Matcher::Regex("past_text".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"resp_prev_2","object":"response","created_at":1,"status":"completed",
                    "model":"gpt-4",
                    "output":[{"type":"message","id":"msg_2","status":"completed","role":"assistant",
                        "content":[{"type":"output_text","text":"second","annotations":[]}]}],
                    "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}}"#,
            )
            .expect(1)
            .create();
        // 第一轮兜底 mock（无 past_text）
        server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"resp_prev_1","object":"response","created_at":1,"status":"completed",
                    "model":"gpt-4",
                    "output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant",
                        "content":[{"type":"output_text","text":"past_text","annotations":[]}]}],
                    "usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;

        // 第一轮：native 非流式 + store，产出可回放历史
        let first = post_responses(
            state.clone(),
            json!({
                "model": "gpt-4",
                "input": "first",
                "max_output_tokens": 16,
                "store": true,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(first.status(), 200);
        assert_eq!(response_json(first).await["id"], "resp_prev_1");

        // 等待非流式 store 路径的后台持久化落地
        let mut ready = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let probe = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/v1/responses/resp_prev_1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if probe.status() == 200 {
                ready = true;
                break;
            }
        }
        assert!(ready, "首轮响应必须持久化可回读");

        // 第二轮：previous_response_id 回放 → 上游 body 含历史 assistant 文本
        let second = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": "next",
                "max_output_tokens": 16,
                "previous_response_id": "resp_prev_1",
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert_eq!(second.status(), 200);
        replay.assert();
    }
    /// native + x-vendor 点名合法 responses 厂商：路由钉到该厂商。
    #[tokio::test]
    async fn native_xvendor_hint_routes_to_named_responses_vendor() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/responses")
            .match_body(Matcher::Regex("web_search".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"resp_hint","object":"response","created_at":1,"status":"completed",
                    "model":"gpt-4",
                    "output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant",
                        "content":[{"type":"output_text","text":"hinted","annotations":[]}]}],
                    "usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![
            responses_vendor("other", "http://127.0.0.1:1/v1".into()),
            responses_vendor("rr", format!("{}/v1", server.url())),
        ])
        .await;
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .header("x-vendor", "rr")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4", "input": "x", "max_output_tokens": 16,
                            "tools": [{"type": "web_search"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "合法 hint 必须路由到点名厂商");
        let body = response_json(response).await;
        assert_eq!(body["id"], "resp_hint");
        mock.assert();
    }

    /// native 流式上游失败：5xx 映射且不吞错误。
    #[tokio::test]
    async fn native_stream_upstream_failure_maps_5xx() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/responses")
            .with_status(503)
            .with_body("unavailable")
            .expect(1)
            .create();
        let state = make_state(vec![responses_vendor("rr", format!("{}/v1", server.url()))]).await;
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4", "input": "x", "max_output_tokens": 16,
                "stream": true,
                "tools": [{"type": "web_search"}]
            }),
        )
        .await;
        assert!(
            response.status().as_u16() >= 500,
            "上游 503 必须映射 5xx: {}",
            response.status()
        );
    }

    /// background 总开关关闭：显式 400 拒绝。
    #[tokio::test]
    async fn background_disabled_is_400() {
        let state = make_state(Vec::new()).await;
        let mut config = state.config.get_config();
        config.responses.background_enabled = false;
        state
            .config
            .swap_memory(config)
            .expect("关闭 background 段");
        let response = post_responses(
            state,
            json!({
                "model": "gpt-4", "input": "x", "max_output_tokens": 16,
                "background": true
            }),
        )
        .await;
        assert_eq!(response.status(), 400, "background 关闭必须显式拒绝");
    }

    /// 后台槽位满载：第二个 background 请求 429。
    #[tokio::test]
    async fn background_slots_exhausted_is_429() {
        let mut server = mockito::Server::new_async().await;
        // 挂起响应：占住首个后台任务直到测试结束（mockito 200 带延迟体）
        let slow_body = std::io::Cursor::new(vec![0u8; 64]);
        let _slow = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body_from_file("/dev/null")
            .create_async()
            .await;
        let _ = slow_body;
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        let mut config = state.config.get_config();
        config.responses.max_background_jobs = 1;
        state.config.swap_memory(config).expect("单槽位");

        let first = post_responses(
            state.clone(),
            json!({
                "model": "gpt-4", "input": "x", "max_output_tokens": 16,
                "background": true
            }),
        )
        .await;
        assert_eq!(first.status(), 200, "首个任务入队: {}", first.status());

        let second = post_responses(
            state,
            json!({
                "model": "gpt-4", "input": "y", "max_output_tokens": 16,
                "background": true
            }),
        )
        .await;
        assert_eq!(second.status(), 429, "槽位满载必须 429");
    }

    /// 后台任务失败：状态回写 failed + error.code=background_failed。
    #[tokio::test]
    async fn background_job_failure_writes_failed_status() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .expect_at_least(1)
            .create_async()
            .await;
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        // 压缩生产重试退避：失败回写时延从 ~4s 降到毫秒级（断言语义不变）
        let mut config = state.config.get_config();
        config.retry.initial_delay_ms = 1;
        config.retry.max_delay_ms = 2;
        state.config.swap_memory(config).expect("压缩重试退避");

        let queued = post_responses(
            state.clone(),
            json!({
                "model": "gpt-4", "input": "x", "max_output_tokens": 16,
                "background": true
            }),
        )
        .await;
        assert_eq!(queued.status(), 200, "任务入队");
        let id = response_json(queued).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        // 轮询直到状态 failed（含重试退避，预算 30s）
        let mut final_body = None;
        for _ in 0..150 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
            if fetched.status() == 200 {
                let body = response_json(fetched).await;
                if body["status"] == "failed" {
                    final_body = Some(body);
                    break;
                }
            }
        }
        let body = final_body.expect("失败状态必须回写（预算 30s）");
        assert_eq!(body["error"]["code"], "background_failed", "{body}");
    }
    /// GET/DELETE 的错误面：disabled→404、未知 id→404、delete 未知→404。
    #[tokio::test]
    async fn get_delete_error_surfaces() {
        let state = make_state(Vec::new()).await;
        let missing = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/responses/resp_nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), 404, "未知 id 必须 404");

        let deleted = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/responses/resp_nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), 404, "删除未知 id 必须 404");

        let mut config = state.config.get_config();
        config.responses.enabled = false;
        state.config.swap_memory(config).expect("关闭 responses");
        let disabled = app(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/responses/resp_any")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(disabled.status(), 404, "disabled 时 GET 亦 404");
    }

    /// truncation="auto"：按 model_catalog 的 context_window 丢弃最旧 items。
    #[tokio::test]
    async fn truncation_auto_drops_oldest_items() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        // 断言上游只收到最后一条消息（旧 item 被裁）
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "messages": [{"role": "user", "content": "newest"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .expect(1)
            .create();
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        let mut config = state.config.get_config();
        // 极小 context_window：budget = 2*4 = 8 字符
        config.model_catalog = vec![fluxweir::config::ModelCatalogEntry {
            model_id: "gpt-4".into(),
            context_window: 2,
            ..Default::default()
        }];
        state.config.swap_memory(config).expect("注入目录");

        let response = post_responses(
            state,
            json!({
                "model": "gpt-4",
                "input": [
                    {"role": "user", "content": "oldest message with long text"},
                    {"role": "user", "content": "newest"}
                ],
                "max_output_tokens": 16,
                "truncation": "auto"
            }),
        )
        .await;
        assert_eq!(response.status(), 200, "裁剪后请求必须成功");
        mock.assert();
    }

    /// background + X-Vendor hint：后台任务走 hint 路由分支并完成。
    #[tokio::test]
    async fn background_with_hint_completes() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "messages": [{"role": "user", "content": "hi"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1,"model":"gpt-4",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let state = make_state(vec![chat_vendor("v1", format!("{}/v1", server.url()))]).await;
        let queued = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .header("x-vendor", "v1")
                    .body(Body::from(
                        json!({
                            "model": "gpt-4", "input": "hi", "max_output_tokens": 16,
                            "background": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), 200);
        let id = response_json(queued).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let mut completed = false;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let r = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/v1/responses/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if r.status() == 200 {
                let body = response_json(r).await;
                if body["status"] == "completed" {
                    completed = true;
                    break;
                }
            }
        }
        assert!(completed, "hint 路由的后台任务必须完成");
        mock.assert_async().await;
    }
    /// cancel/delete 的 disabled 与未知 id 分支。
    #[tokio::test]
    async fn cancel_delete_disabled_and_unknown() {
        let state = make_state(Vec::new()).await;
        // cancel 未知 id → 404
        let cancelled = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses/resp_nope/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), 404, "cancel 未知 id 必须 404");

        let mut config = state.config.get_config();
        config.responses.enabled = false;
        state.config.swap_memory(config).expect("关闭 responses");
        for (method, uri) in [
            ("POST", "/v1/responses/resp_x/cancel"),
            ("DELETE", "/v1/responses/resp_x"),
        ] {
            let r = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), 404, "{method} {uri} 在 disabled 下 404");
        }
    }
}
