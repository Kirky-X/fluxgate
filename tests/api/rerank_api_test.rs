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
    use fluxweir::api::rerank::rerank;
    use fluxweir::config::{
        Config, ConfigManager, ModelCatalogEntry, ModelConfig, RateLimitConfig, VendorConfig,
    };
    use fluxweir::database::{CostGroupBy, Repository, TokenPreAggregator};
    use fluxweir::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxweir::router::Router as NexusRouter;
    use fluxweir::vendor::VendorManager;
    use fluxweir::ApiState;
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
        make_state_with_catalog(vendors, Vec::new()).await.0
    }

    async fn make_state_with_catalog(
        vendors: Vec<VendorConfig>,
        catalog: Vec<ModelCatalogEntry>,
    ) -> (Arc<ApiState>, Arc<Repository>) {
        make_state_with_config(vendors, catalog, |_| {}).await
    }

    async fn make_state_with_config(
        vendors: Vec<VendorConfig>,
        catalog: Vec<ModelCatalogEntry>,
        configure: impl FnOnce(&mut Config),
    ) -> (Arc<ApiState>, Arc<Repository>) {
        let mut config = Config {
            model_catalog: catalog,
            ..common::test_config(vendors)
        };
        configure(&mut config);
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

        let state = Arc::new(ApiState {
            affinity_writer: None,
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(fluxweir::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: None,
            pre_aggregator: Some(Arc::new(TokenPreAggregator::new(
                repository.clone(),
                std::time::Duration::from_secs(3600),
            ))),
            affinity: Some(Arc::new(
                fluxweir::router::affinity::AffinityTable::with_memory_backend(
                    std::time::Duration::from_secs(3600),
                ),
            )),
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            #[cfg(feature = "web-search")]
            web_search: None,
        });
        (state, repository)
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/rerank", post(rerank))
            .with_state(state)
    }

    fn models_app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route(
                "/v1/models",
                axum::routing::get(fluxweir::api::models::list_models),
            )
            .with_state(state)
    }

    async fn get_models(state: Arc<ApiState>, uri: &str) -> axum::response::Response {
        models_app(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
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

    /// rerank 成本入分账：成功请求 flush 后 /admin/cost 可查，token 列为 0
    #[tokio::test]
    async fn rerank_cost_lands_in_admin_cost_ledger() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .create();
        let vendor = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "rerank-v3-real",
            "rerank-v3",
            1.0,
        );
        let catalog = vec![ModelCatalogEntry {
            model_id: "rerank-v3-real".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 2.0,
            ..Default::default()
        }];
        let (state, repo) = make_state_with_catalog(vec![vendor], catalog).await;

        let response = post_rerank(
            state.clone(),
            json!({"model": "rerank-v3", "query": "q", "documents": ["a", "b", "c"]}),
        )
        .await;
        assert_eq!(response.status(), 200);

        state
            .pre_aggregator
            .as_ref()
            .expect("pre_aggregator wired")
            .flush()
            .await
            .expect("flush");
        let rows = repo
            .query_cost_usage(None, None, CostGroupBy::Vendor, None)
            .await
            .expect("cost rows");
        let row = rows
            .iter()
            .find(|row| row.vendor.as_deref() == Some("rr-prod"))
            .expect("rerank ledger row");
        // 3 docs × $2/1k docs = $0.006
        assert!(
            (row.input_cost - 0.006).abs() < 1e-9,
            "input_cost 归集 rerank 成本"
        );
        assert!(
            (row.total_cost - 0.006).abs() < 1e-9,
            "total_cost 覆盖 rerank"
        );
        assert_eq!(row.input_tokens, 0, "token 列恒 0，不虚增 token 统计");
        assert_eq!(row.output_tokens, 0);
    }

    /// `/v1/models?task=` 按任务类型过滤（大小写不敏感，非法值 400）
    #[tokio::test]
    async fn models_list_filters_by_task() {
        let chat = common::base_vendor("chat-v", "");
        let mut embedding = common::base_vendor("emb-v", "");
        embedding.models = vec![ModelConfig {
            model_id: "emb-real".into(),
            abstract_id: Some("emb".into()),
            weight: 1.0,
            enabled: true,
            dimensions: Some(1024),
            compat_group: None,
            model_type: None,
        }];
        let rr = rerank_vendor("rr-v", "http://127.0.0.1:1/v1".into(), "rr", "rr", 1.0);
        let state = make_state(vec![chat, embedding, rr]).await;

        let all = body_json(get_models(state.clone(), "/v1/models").await).await;
        assert_eq!(all["data"].as_array().unwrap().len(), 3, "缺省不过滤");

        for (task, expected) in [("rerank", "rr"), ("embedding", "emb"), ("chat", "gpt-4")] {
            let body =
                body_json(get_models(state.clone(), &format!("/v1/models?task={task}")).await)
                    .await;
            let items = body["data"].as_array().unwrap();
            assert_eq!(items.len(), 1, "task={task} 只返回一条: {body}");
            assert_eq!(items[0]["id"], expected);
            assert_eq!(items[0]["task"], task);
        }

        let upper = body_json(get_models(state.clone(), "/v1/models?task=RERANK").await).await;
        assert_eq!(upper["data"].as_array().unwrap().len(), 1, "大小写不敏感");

        let bad = get_models(state, "/v1/models?task=unknown").await;
        assert_eq!(bad.status(), 400, "白名单外显式拒绝");
    }

    /// minmax 归一化：分数缩放到 [0,1]，排序与 index 不变
    #[tokio::test]
    async fn rerank_minmax_normalization_scales_scores() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"results":[
                    {"index":0,"relevance_score":0.2},
                    {"index":1,"relevance_score":0.6},
                    {"index":2,"relevance_score":0.4}
                ]}"#,
            )
            .create();
        let vendor = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "m",
            "rerank-v3",
            1.0,
        );
        let (state, _repo) = make_state_with_config(vec![vendor], Vec::new(), |config| {
            config.routing.rerank_score_normalization = "minmax".into();
        })
        .await;

        let response = post_rerank(
            state,
            json!({"model": "rerank-v3", "query": "q", "documents": ["a", "b", "c"]}),
        )
        .await;
        assert_eq!(response.status(), 200);
        let body = body_json(response).await;
        let results = body["results"].as_array().unwrap();
        let scores: Vec<f64> = results
            .iter()
            .map(|result| result["relevance_score"].as_f64().unwrap())
            .collect();
        assert!((scores[0] - 0.0).abs() < 1e-9);
        assert!((scores[1] - 1.0).abs() < 1e-9);
        assert!((scores[2] - 0.5).abs() < 1e-9);
        assert_eq!(results[0]["index"], 0, "index/排序不变");
        assert_eq!(results[1]["index"], 1);
        assert_eq!(results[2]["index"], 2);
    }

    /// 成功请求写回会话亲和；失败不写
    #[tokio::test]
    async fn rerank_binds_session_affinity_on_success_only() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .create();
        let vendor = rerank_vendor(
            "rr-prod",
            format!("{}/v1", server.url()),
            "m",
            "rerank-v3",
            1.0,
        );
        let state = make_state(vec![vendor]).await;

        let ok = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .header("x-session-id", "sess-ok")
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
        assert_eq!(ok.status(), 200);
        let affinity = state.affinity.as_ref().expect("affinity wired");
        assert_eq!(
            affinity.lookup(None, "sess-ok").await.as_deref(),
            Some("rr-prod"),
            "成功请求必须写回实际 vendor"
        );

        // 失败路径：换一个不可达上游，不覆盖既有绑定
        let bad_vendor = rerank_vendor(
            "rr-bad",
            "http://127.0.0.1:1/v1".into(),
            "m",
            "rerank-v3",
            1.0,
        );
        let bad_state = make_state(vec![bad_vendor]).await;
        let failed = app(bad_state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rerank")
                    .header("content-type", "application/json")
                    .header("x-session-id", "sess-fail")
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
        assert!(failed.status().is_server_error());
        assert_eq!(
            bad_state
                .affinity
                .as_ref()
                .unwrap()
                .lookup(None, "sess-fail")
                .await,
            None,
            "失败请求不得写入绑定"
        );
    }

    /// 亲和模式门：rerank 的写回与 chat 同源——`off` 不写（off 的查询/写入
    /// 短路对本端点同样成立），`auto`/`turn` 由 usage 结算点承接（rerank 无
    /// usage 语义，不在此写 1h 无条件 set）。
    #[tokio::test]
    async fn rerank_affinity_write_respects_mode_gate() {
        for (mode, expect_written) in [
            ("legacy", true),
            ("off", false),
            ("auto", false),
            ("turn", false),
        ] {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/v1/rerank")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
                .expect(1)
                .create_async()
                .await;
            let (state, _repo) = make_state_with_config(
                vec![rerank_vendor(
                    "rr-prod",
                    format!("{}/v1", server.url()),
                    "m",
                    "rerank-v3",
                    1.0,
                )],
                Vec::new(),
                |config| config.routing.affinity.mode = mode.into(),
            )
            .await;

            let resp = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/rerank")
                        .header("content-type", "application/json")
                        .header("x-session-id", "sess-mode")
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
            assert_eq!(resp.status(), 200, "模式 {mode} 下 rerank 应正常服务");
            let bound = state
                .affinity
                .as_ref()
                .expect("affinity wired")
                .peek(None, "sess-mode")
                .await;
            if expect_written {
                assert_eq!(
                    bound.as_deref(),
                    Some("rr-prod"),
                    "legacy 必须写回绑定（实测 {bound:?}）"
                );
            } else {
                assert_eq!(bound, None, "模式 {mode} 不得写亲和（实测 {bound:?}）");
            }
            mock.assert_async().await;
        }
    }
}

/// 降级面回归：上游 2xx 形状非法（已计费仍入分账）、日志/分账 sink 失败
/// 不拖垮请求、usage_insights 关闭时会话键脱敏。
#[cfg(test)]
mod degraded_paths {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxweir::api::rerank::rerank;
    use fluxweir::config::{
        Config, ConfigManager, ModelCatalogEntry, ModelConfig, RateLimitConfig,
        UsageInsightsSection, VendorConfig,
    };
    use fluxweir::database::{
        AsyncLogger, AsyncLoggerHandle, CostGroupBy, LogFilter, Repository, RequestLog,
        TokenPreAggregator,
    };
    use fluxweir::error::GateError;
    use fluxweir::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxweir::router::Router as NexusRouter;
    use fluxweir::storage::{RequestLogSink, UsageSink};
    use fluxweir::vendor::VendorManager;
    use fluxweir::ApiState;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    fn rr_vendor(api_base: String) -> VendorConfig {
        common::ensure_test_api_key();
        let mut vendor = common::base_vendor("rr-degraded", api_base);
        vendor.type_ = "rerank".into();
        vendor.models = vec![ModelConfig {
            model_id: "rerank-v3-real".into(),
            abstract_id: Some("rerank-v3".into()),
            weight: 1.0,
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

    /// 日志前沿形态：未接线 / 注入恒失败 / 异步管道落真库。
    enum LoggerKind {
        Unwired,
        Failing,
        AsyncPipe,
    }

    /// 预聚合前沿形态：未接线 / 注入恒失败 / 真聚合器落真库。
    enum AggregatorKind {
        Unwired,
        Failing,
        Real,
    }

    /// sink 形态可注入的状态基座（返回 repository 供落库断言；真聚合器/
    /// 异步管道均接线到同一 repository）。
    async fn make_state(
        vendor: VendorConfig,
        configure: impl FnOnce(&mut Config),
        logger_kind: LoggerKind,
        aggregator_kind: AggregatorKind,
    ) -> (Arc<ApiState>, Arc<Repository>) {
        let mut config = Config {
            model_catalog: vec![ModelCatalogEntry {
                model_id: "rerank-v3-real".into(),
                context_window: 128_000,
                rerank_price_per_1k_docs: 2.0,
                ..Default::default()
            }],
            ..common::test_config(vec![vendor])
        };
        configure(&mut config);
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

        let logger: Option<Arc<dyn RequestLogSink>> = match logger_kind {
            LoggerKind::Unwired => None,
            LoggerKind::Failing => Some(Arc::new(FailingLogSink)),
            LoggerKind::AsyncPipe => Some(Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
                repository.clone(),
                64,
                Duration::from_millis(200),
            )))),
        };
        let pre_aggregator: Option<Arc<dyn UsageSink>> = match aggregator_kind {
            AggregatorKind::Unwired => None,
            AggregatorKind::Failing => Some(Arc::new(FailingUsageSink)),
            AggregatorKind::Real => Some(Arc::new(TokenPreAggregator::new(
                repository.clone(),
                Duration::from_secs(3600),
            ))),
        };

        let state = Arc::new(ApiState {
            affinity_writer: None,
            config: config_manager.clone(),
            vendor_manager: vendor_manager.clone(),
            router: Arc::new(NexusRouter::new(config_manager, vendor_manager)),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database: Arc::new(fluxweir::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger,
            pre_aggregator,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            #[cfg(feature = "web-search")]
            web_search: None,
        });
        (state, repository)
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/rerank", post(rerank))
            .with_state(state)
    }

    async fn post_rerank(
        state: Arc<ApiState>,
        body: Value,
        session: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/rerank")
            .header("content-type", "application/json");
        if let Some(session) = session {
            builder = builder.header("x-session-id", session);
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

    /// 注入失败 sink：log / record_keyed 恒 Err（管道拒绝形态）。
    struct FailingLogSink;
    #[async_trait::async_trait]
    impl RequestLogSink for FailingLogSink {
        async fn log(&self, _log: RequestLog) -> Result<(), GateError> {
            Err(GateError::Config {
                message: "injected log sink failure".into(),
            })
        }
    }

    struct FailingUsageSink;
    #[async_trait::async_trait]
    impl UsageSink for FailingUsageSink {
        async fn record_keyed(
            &self,
            _usage: &fluxweir::storage::KeyedUsage,
        ) -> Result<(), GateError> {
            Err(GateError::Config {
                message: "injected usage sink failure".into(),
            })
        }
    }

    /// 上游 200 但响应形状非法（缺 results）：按协议错误映射 502，且上游
    /// 已 2xx 视为已计费——请求级成本仍入分账（目录 2.0/千文档 × 3 文档）。
    #[tokio::test]
    async fn malformed_2xx_upstream_is_billed_and_maps_502() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/rerank")
            .match_header("authorization", "Bearer test-key-for-mockito")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"unexpected":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let (state, repository) = make_state(
            rr_vendor(format!("{}/v1", server.url())),
            |_| {},
            LoggerKind::Unwired,
            AggregatorKind::Real,
        )
        .await;

        let response = post_rerank(
            state.clone(),
            json!({"model": "rerank-v3", "query": "q", "documents": ["a", "b", "c"]}),
            None,
        )
        .await;
        assert_eq!(response.status(), 502, "2xx 形状非法必须映射协议错误 502");

        state
            .pre_aggregator
            .as_ref()
            .expect("aggregator wired")
            .flush()
            .await
            .expect("flush");
        let rows = repository
            .query_cost_usage(None, None, CostGroupBy::Model, None)
            .await
            .expect("cost rows");
        assert_eq!(rows.len(), 1, "形状非法的 2xx 也必须入分账: {rows:?}");
        assert_eq!(rows[0].model.as_deref(), Some("rerank-v3-real"));
        assert!(
            (rows[0].input_cost - 0.006).abs() < 1e-9,
            "2.0/千文档 × 3 文档 = 0.006，实测 {}",
            rows[0].input_cost
        );
        mock.assert_async().await;
    }

    /// 日志/分账 sink 拒绝均为降级面：请求照常成功（记账失败仅 warn，
    /// 不失败请求——与 chat 记账同语义）；预聚合未接线（None）为 no-op。
    #[tokio::test]
    async fn degraded_logging_and_accounting_never_fail_request() {
        // 形态 1：logger 恒 Err + 预聚合未接线
        let mut server1 = mockito::Server::new_async().await;
        let mock1 = server1
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .expect(1)
            .create_async()
            .await;
        let (state, _repo) = make_state(
            rr_vendor(format!("{}/v1", server1.url())),
            |_| {},
            LoggerKind::Failing,
            AggregatorKind::Unwired,
        )
        .await;
        let response = post_rerank(
            state,
            json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
            None,
        )
        .await;
        assert_eq!(response.status(), 200, "日志失败不得拖垮请求");
        mock1.assert_async().await;

        // 形态 2：分账 record_keyed 恒 Err
        let mut server2 = mockito::Server::new_async().await;
        let mock2 = server2
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .expect(1)
            .create_async()
            .await;
        let (state, _repo) = make_state(
            rr_vendor(format!("{}/v1", server2.url())),
            |_| {},
            LoggerKind::Unwired,
            AggregatorKind::Failing,
        )
        .await;
        let response = post_rerank(
            state,
            json!({"model": "rerank-v3", "query": "q", "documents": ["a"]}),
            None,
        )
        .await;
        assert_eq!(response.status(), 200, "分账失败不得拖垮请求");
        mock2.assert_async().await;
    }

    /// usage_insights 关闭：x-session-id 头照常可带，但落库行 session_key
    /// 必须为 NULL（脱敏口径：insights 关 = 不采会话维度）；对照组（开启）
    /// 同头落 Some，证明头被解析、关闭时是主动剥离。
    #[tokio::test]
    async fn usage_insights_disabled_strips_session_key_in_log() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/rerank")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"results":[{"index":0,"relevance_score":0.5}]}"#)
            .expect(2)
            .create_async()
            .await;
        let body = json!({"model": "rerank-v3", "query": "q", "documents": ["a"]});

        // 对照组：insights 开（默认）→ session_key 落库
        let (state_on, repo_on) = make_state(
            rr_vendor(format!("{}/v1", server.url())),
            |_| {},
            LoggerKind::AsyncPipe,
            AggregatorKind::Unwired,
        )
        .await;
        let response = post_rerank(state_on.clone(), body.clone(), Some("sess-insights")).await;
        assert_eq!(response.status(), 200);
        state_on
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");
        let logs = repo_on
            .query_logs(&LogFilter::default())
            .await
            .expect("logs");
        assert_eq!(logs.len(), 1, "对照组恰一条日志");
        assert_eq!(
            logs[0].session_key.as_deref(),
            Some("sess-insights"),
            "insights 开启时会话键必须落库"
        );

        // 被测组：insights 关 → 同头剥离
        let (state_off, repo_off) = make_state(
            rr_vendor(format!("{}/v1", server.url())),
            |config: &mut Config| config.usage_insights = UsageInsightsSection { enabled: false },
            LoggerKind::AsyncPipe,
            AggregatorKind::Unwired,
        )
        .await;
        let response = post_rerank(state_off.clone(), body, Some("sess-insights")).await;
        assert_eq!(response.status(), 200);
        state_off
            .logger
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .expect("shutdown");
        let logs = repo_off
            .query_logs(&LogFilter::default())
            .await
            .expect("logs");
        assert_eq!(logs.len(), 1, "被测组恰一条日志");
        assert!(
            logs[0].session_key.is_none(),
            "usage_insights 关闭时会话键必须剥离（实测 {:?}）",
            logs[0].session_key
        );
        mock.assert_async().await;
    }
}
