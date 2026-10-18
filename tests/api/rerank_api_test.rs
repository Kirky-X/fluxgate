#![cfg(feature = "web-server")]
// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `/v1/rerank` 端到端回归：能力过滤、上游形状、错误映射与校验。

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::rerank::rerank;
    use fluxgate::config::{ConfigManager, ModelConfig, RateLimitConfig, VendorConfig};
    use fluxgate::database::{Repository, TokenPreAggregator};
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn rerank_vendor(
        id: &str,
        api_base: String,
        model_id: &str,
        abstract_id: &str,
        weight: f64,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        let mut vendor = common::base_vendor(id, api_base);
        vendor.type_ = "rerank".into();
        vendor.models = vec![ModelConfig {
            model_id: model_id.into(),
            abstract_id: Some(abstract_id.into()),
            weight,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: Some("rerank".into()),
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
        let config_manager = Arc::new(ConfigManager::new(common::test_config(vendors)));
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
            database: Arc::new(fluxgate::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: None,
            pre_aggregator: Some(Arc::new(TokenPreAggregator::new(
                repository,
                std::time::Duration::from_secs(3600),
            ))),
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        })
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/rerank", post(rerank))
            .with_state(state)
    }

    async fn post_rerank(state: Arc<ApiState>, body: Value) -> axum::response::Response {
        app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn rerank_success_returns_real_vendor_and_model() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/rerank")
            .match_header("authorization", "Bearer test-key-for-mockito")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"results":[{"index":1,"relevance_score":0.9,"document":{"text":"b"}},{"index":0,"relevance_score":0.1,"document":{"text":"a"}}]}"#,
            )
            .create();
        let vendor = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "rerank-v3-real",
            "rerank-v3",
            1.0,
        );
        let state = make_state(vec![vendor]).await;

        let response = post_rerank(
            state,
            json!({
                "model": "rerank-v3",
                "query": "什么是重排",
                "documents": ["a", "b"],
                "top_n": 2,
                "return_documents": true,
            }),
        )
        .await;

        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        assert_eq!(
            body["model"], "rerank-v3-real",
            "响应 model 必须是上游真实名"
        );
        assert_eq!(body["meta"]["vendor"], "rr-prod");
        assert_eq!(body["meta"]["billed_documents"], 2);
        assert_eq!(body["results"].as_array().unwrap().len(), 2);
        assert_eq!(body["results"][0]["index"], 1);
        assert_eq!(body["results"][0]["document"]["text"], "b");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn rerank_skips_chat_only_vendor() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .create();
        // 高权重 chat-only 厂商与低权重 rerank 厂商共存
        let mut chat = common::base_vendor("chat-only", "");
        chat.models = vec![ModelConfig {
            model_id: "shared".into(),
            abstract_id: Some("rerank-v3".into()),
            weight: 100.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        let rr = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "shared",
            "rerank-v3",
            0.001,
        );
        let state = make_state(vec![chat, rr]).await;

        let response = post_rerank(
            state,
            json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
        )
        .await;

        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        assert_eq!(body["meta"]["vendor"], "rr-prod", "必须跳过 chat-only 厂商");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn rerank_upstream_5xx_maps_to_503() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/rerank")
            .with_status(500)
            .with_body("{}")
            .create();
        let vendor = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "m",
            "rerank-v3",
            1.0,
        );
        let state = make_state(vec![vendor]).await;

        let response = post_rerank(
            state,
            json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
        )
        .await;

        assert_eq!(response.status(), 503, "上游非 2xx → VendorUnavailable");
        let body = body_json(response).await;
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn rerank_validation_and_missing_capability_reject_400() {
        let state = make_state(vec![]).await;
        for body in [
            json!({"model": "m", "query": "q", "documents": ["a"], "stream": true}),
            json!({"model": "m", "query": "q", "documents": []}),
            json!({"model": "m", "query": "q", "documents": ["a"], "top_n": 5}),
            json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
        ] {
            let response = post_rerank(state.clone(), body).await;
            assert_eq!(response.status(), 400, "校验/无候选必须映射 400");
        }
    }

    /// 点名到 chat-only 厂商：502 且上游零请求（能力过滤不被绕过）
    #[tokio::test]
    async fn rerank_rejects_hint_to_chat_only_vendor_without_upstream_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("POST", "/v1/rerank").expect(0).create();
        let mut chat = common::base_vendor("chat-only", "");
        chat.models = vec![ModelConfig {
            model_id: "shared".into(),
            abstract_id: Some("rerank-v3".into()),
            weight: 1.0,
            enabled: true,
            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        let rr = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "shared",
            "rerank-v3",
            1.0,
        );
        let state = make_state(vec![chat, rr]).await;

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .header("x-vendor", "chat-only")
                    .body(Body::from(
                        serde_json::to_vec(
                            &json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 502, "点名非 rerank 厂商必须拒绝");
        mock.assert_async().await;
    }

    /// 点名校验：合法 rerank 点名放行，X-Model 指向非 rerank 条目拒绝
    #[tokio::test]
    async fn rerank_hint_checks_model_capability() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .expect(1)
            .create();
        let rr = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "rr-real",
            "rerank-v3",
            1.0,
        );
        let state = make_state(vec![rr]).await;

        let ok = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .header("x-vendor", "rr-prod")
                    .header("x-model", "rerank-v3")
                    .body(Body::from(
                        serde_json::to_vec(
                            &json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), 200, "合法 rerank 点名必须放行");
        mock.assert_async().await;

        let bad = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .header("x-model", "chat-model")
                    .body(Body::from(
                        serde_json::to_vec(
                            &json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), 502, "X-Model 指向 chat 条目必须拒绝");
    }
}
