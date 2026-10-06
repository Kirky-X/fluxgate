// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(all(feature = "web-server", feature = "resilience"))]

//! recId=7⑤：换 vendor fallback E2E（HTTP 全链路，留在 nexusflow 应用面）。
//!
//! 自 retry_test 拆出（N1 workspace 化）：纯 utils/retry 单元与策略构造
//! 测试随 llm-gate 走（tests/retry_test.rs），本文件只留经 api/ApiState
//! 全栈的 fallback 集成（依赖 api/database/web-server 门控）。

mod common;

mod vendor_fallback_integration {
    use axum::{body::Body, http::Request, routing::post, Router};
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::api::{chat_completions, messages};
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, RetrySection,
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

    /// type_ = "anthropic"：入站 OpenAI 请求走转换路径
    /// （execute_request），fallback 循环生效；避免同格式透传绕过。
    fn make_vendor(id: &str, priority: u8, api_base: String) -> VendorConfig {
        crate::common::ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            name: format!("Test {}", id),
            type_: "anthropic".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_key_envs: Vec::new(),
            api_base,
            enabled: true,
            priority,
            models: vec![ModelConfig {
                model_id: "claude-x".into(),
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
                url: String::new(),
            },
            vendors,
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            // recId=7：max_retries=2 + jitter=0（确定性 1s 间隔）——
            // 主 vendor 500×3（1 初试 + 2 重试）后耗尽换路
            retry: RetrySection {
                max_retries: 2,
                jitter: 0.0,
                ..Default::default()
            },
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
            docs: Default::default(),
        }
    }

    async fn make_api_state(config: Config) -> Arc<ApiState> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

        let pool = DbPool::with_config(DbConfig {
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

    fn app(state: Arc<ApiState>) -> axum::Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    fn openai_body() -> serde_json::Value {
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 10
        })
    }

    const ANTHROPIC_SUCCESS_BODY: &str = r#"{
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-x",
        "content": [{"type": "text", "text": "Hi from fallback"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    }"#;

    /// recId=7⑤③：主 vendor 连续 500×3（1 初试 + 2 重试，max_retries=2）
    /// 耗尽 → 换路：第二 vendor 收到请求且客户端拿到成功响应。
    ///
    /// 用 X-Vendor 路由提示直达主 vendor（weighted_select 在等权候选间
    /// 随机，不加提示则测试不确定）；fallback 的 route_excluding 不带
    /// 提示，被排除的主 vendor 不再参与 → 副本 vendor 接管。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn exhausted_primary_falls_over_to_secondary() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/messages")
            .with_status(500)
            .with_body("upstream error")
            .expect(3) // 1 初试 + 2 重试（RetryableFixed，jitter=0 → 1s×2）
            .create_async()
            .await;

        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_body(ANTHROPIC_SUCCESS_BODY)
            .expect(1)
            .create_async()
            .await;

        let v_a = make_vendor("vendor_a", 1, server_a.url());
        let v_b = make_vendor("vendor_b", 2, server_b.url());
        let state = make_api_state(make_config(vec![v_a, v_b])).await;
        state.vendor_manager.initialize().await.expect("init");

        // 入站 OpenAI + 出站 anthropic vendor → 转换路径（fallback 生效）
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("x-vendor", "vendor_a")
                    .body(Body::from(serde_json::to_vec(&openai_body()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            200,
            "fallback 到 vendor_b 后客户端应拿到成功响应"
        );

        mock_a.assert_async().await;
        mock_b.assert_async().await;
    }

    /// recId=7⑤④：仅一个合格 vendor 失败 → 客户端收 503 冒泡
    /// （VendorUnavailable 经 error.rs 固定映射 SERVICE_UNAVAILABLE），
    /// 而非死循环。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn single_vendor_failure_bubbles_up_as_503() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/messages")
            .with_status(500)
            .with_body("upstream error")
            .expect(1) // max_retries=0：无内部重试，也无死循环
            .create_async()
            .await;

        let v_a = make_vendor("vendor_solo", 1, server_a.url());
        let mut config = make_config(vec![v_a]);
        config.retry.max_retries = 0;
        let state = make_api_state(config).await;
        state.vendor_manager.initialize().await.expect("init");

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&openai_body()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "唯一 vendor 失败应以 503 冒泡"
        );

        mock_a.assert_async().await;
    }
}
