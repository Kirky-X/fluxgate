// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! FallbackChain 单元测试。
//!
//! 测试属性：
//! 1. primary 5xx 失败 → 返回 secondary（按 priority 升序）
//! 2. trigger=Http5xx 时 429 错误不触发 fallback
//! 3. trigger=All 时任何错误都触发
//! 4. 全部 fallback 用尽返回 None
//! 5. trigger=CircuitOpen 仅匹配 "circuit open" 错误
//! 6. trigger=RateLimited 匹配 RateLimited variant 与 429 VendorUnavailable

#![cfg(all(feature = "resilience", feature = "vendor-management"))]

#[path = "../common/mod.rs"]
mod common;

use nexusflow::error::GateError;
use nexusflow::vendor::resilience::{FallbackChain, FallbackTarget, FallbackTrigger};

fn target(vendor: &str, priority: u8) -> FallbackTarget {
    FallbackTarget {
        vendor_id: vendor.into(),
        model_id: "gpt-4".into(),
        priority,
    }
}

#[test]
fn test_primary_5xx_failure_returns_secondary() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Http5xx,
    );
    let error = GateError::VendorUnavailable {
        vendor: "primary (500: internal server error)".into(),
    };

    assert!(chain.should_trigger(&error), "500 错误应触发 Http5xx");
    let next = chain
        .next_after_failure("primary", &error)
        .expect("应返回下一个 fallback target");
    assert_eq!(next.vendor_id, "secondary");
    assert_eq!(next.priority, 2);
}

#[test]
fn test_http5xx_trigger_ignores_429() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Http5xx,
    );
    let error = GateError::VendorUnavailable {
        vendor: "primary (429: rate limit exceeded)".into(),
    };

    assert!(
        !chain.should_trigger(&error),
        "429 不应触发 Http5xx fallback"
    );
    assert!(
        chain.next_after_failure("primary", &error).is_none(),
        "未触发 fallback 时应返回 None"
    );
}

#[test]
fn test_all_trigger_fires_on_any_error() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::All,
    );

    // Timeout 错误
    let timeout_err = GateError::Timeout {
        vendor: "primary".into(),
    };
    assert!(chain.should_trigger(&timeout_err), "All 应匹配 Timeout");

    // RateLimited variant
    let rl_err = GateError::RateLimited {
        vendor: "primary".into(),
    };
    assert!(chain.should_trigger(&rl_err), "All 应匹配 RateLimited");

    // VendorUnavailable 5xx
    let vu_err = GateError::VendorUnavailable {
        vendor: "primary (503: unavailable)".into(),
    };
    assert!(
        chain.should_trigger(&vu_err),
        "All 应匹配 VendorUnavailable 5xx"
    );

    // 应返回 secondary
    let next = chain
        .next_after_failure("primary", &vu_err)
        .expect("All 触发后应返回下一个 target");
    assert_eq!(next.vendor_id, "secondary");
}

#[test]
fn test_all_fallbacks_exhausted_returns_none() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::All,
    );
    let error = GateError::VendorUnavailable {
        vendor: "secondary (500: error)".into(),
    };

    assert!(chain.should_trigger(&error));
    // secondary 是 priority 最高的（数值最大），无下一个 target
    assert!(
        chain.next_after_failure("secondary", &error).is_none(),
        "fallback 用尽应返回 None"
    );
}

#[test]
fn test_circuit_open_trigger_matches_only_circuit_open_errors() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::CircuitOpen,
    );

    // CB Open 错误（vendor 字符串含 "circuit open"）
    let cb_open = GateError::VendorUnavailable {
        vendor: "primary (circuit open)".into(),
    };
    assert!(
        chain.should_trigger(&cb_open),
        "CircuitOpen 应匹配 'circuit open' 错误"
    );
    let next = chain
        .next_after_failure("primary", &cb_open)
        .expect("应返回 secondary");
    assert_eq!(next.vendor_id, "secondary");

    // 普通 5xx 不应触发 CircuitOpen
    let normal_5xx = GateError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(
        !chain.should_trigger(&normal_5xx),
        "CircuitOpen 不应匹配普通 5xx"
    );
}

#[test]
fn test_rate_limited_trigger_matches_429_and_variant() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::RateLimited,
    );

    // RateLimited variant
    let rl_variant = GateError::RateLimited {
        vendor: "primary".into(),
    };
    assert!(
        chain.should_trigger(&rl_variant),
        "RateLimited 应匹配 RateLimited variant"
    );

    // VendorUnavailable 含 429 状态码
    let vu_429 = GateError::VendorUnavailable {
        vendor: "primary (429: rate limit)".into(),
    };
    assert!(
        chain.should_trigger(&vu_429),
        "RateLimited 应匹配 VendorUnavailable 429"
    );

    // 5xx 不应触发 RateLimited
    let vu_500 = GateError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(!chain.should_trigger(&vu_500), "RateLimited 不应匹配 5xx");
}

#[test]
fn test_timeout_trigger_matches_only_timeout() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Timeout,
    );

    let timeout_err = GateError::Timeout {
        vendor: "primary".into(),
    };
    assert!(chain.should_trigger(&timeout_err), "Timeout 应匹配 Timeout");
    let next = chain
        .next_after_failure("primary", &timeout_err)
        .expect("应返回 secondary");
    assert_eq!(next.vendor_id, "secondary");

    // 5xx 不应触发 Timeout
    let vu_500 = GateError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(!chain.should_trigger(&vu_500), "Timeout 不应匹配 5xx");
}

/// Router::build_fallback_chain —— 链由 config vendors 的
/// priority 字段构建；无候选（direct 形式不在 abstract 表）返回 None。
#[cfg(all(feature = "resilience", feature = "router"))]
mod build_fallback_chain {
    use nexusflow::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, RetrySection, ServerConfig, VendorConfig,
    };
    use nexusflow::router::Router;
    use nexusflow::vendor::resilience::FallbackTrigger;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn make_vendor(id: &str, priority: u8, enabled: bool) -> VendorConfig {
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            supports_batch: false,
            name: format!("Test {}", id),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled,
            priority,
            models: vec![ModelConfig {
                model_id: format!("model-{}", id),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens: 1000,
                window_hours: None,
                sliding: false,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }
    }

    fn make_manager(vendors: Vec<VendorConfig>) -> Router {
        let config = Config {
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
            retry: RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
        };
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        Router::new(cm, vm)
    }

    #[test]
    fn chain_targets_carry_vendor_priority() {
        let router = make_manager(vec![
            make_vendor("primary", 1, true),
            make_vendor("secondary", 2, true),
        ]);

        let chain = router
            .build_fallback_chain("gpt-4")
            .expect("two candidates should build a chain");
        assert_eq!(chain.targets.len(), 2);
        assert_eq!(chain.trigger, FallbackTrigger::All, "trigger 应为 All");
        assert_eq!(chain.targets[0].vendor_id, "primary");
        assert_eq!(chain.targets[0].priority, 1);
        assert_eq!(chain.targets[1].vendor_id, "secondary");
        assert_eq!(chain.targets[1].priority, 2);
        assert_eq!(chain.targets[0].model_id, "model-primary");
    }

    #[test]
    fn disabled_vendors_are_excluded_from_chain() {
        let router = make_manager(vec![
            make_vendor("primary", 1, true),
            make_vendor("disabled", 2, false),
        ]);

        let chain = router
            .build_fallback_chain("gpt-4")
            .expect("one enabled candidate");
        assert_eq!(chain.targets.len(), 1);
        assert_eq!(chain.targets[0].vendor_id, "primary");
    }

    #[test]
    fn no_candidates_returns_none() {
        let router = make_manager(vec![make_vendor("primary", 1, true)]);

        // direct 形式（无 abstract 匹配）→ None → API 层保持单发语义
        assert!(router.build_fallback_chain("unknown-model").is_none());
    }
}

/// cover_all_paths（熔断/重试/换路覆盖透传与流式路径）集成测试。
///
/// 基建为本文件新建（此前无 HTTP 桩用例）：mockito 双 server 场景，
/// 模式抄 tests/vendor_test.rs 与 tests/api_test.rs::streaming_error_propagation。
///
/// 用例矩阵（开关=true）：
/// 1. passthrough_nonstream_fails_over       A 500 → B 成功，actual_vendor=B
/// 2. stream_fails_over_before_first_byte    A 流式 503 → 重派 B 拿到 SSE
/// 3. mid_stream_error_does_not_failover     B 流中断 → SSE error 事件而非重派
/// 4. cb_open_triggers_fallback              A 熔断 Open → 透传不再触达 A，B 接管
/// 5. cb_open_rejects_passthrough_http       direct 路由绕过路由层过滤 → CB 在
///    派发层拒绝（无 HTTP），证明一期 CB 包裹生效
///
/// 开关=false 对照组（防门控失效）：
///
/// 6/7. 单 vendor 失败行为与现网逐位一致 —— B 存在且健康但**零调用**
#[cfg(all(feature = "resilience", feature = "web-server"))]
mod cover_all_paths {
    use axum::{body::Body, http::Request, routing::post, Router};
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::api::{chat_completions, ApiState};
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ResilienceSection,
        ServerConfig, VendorConfig,
    };
    use nexusflow::database::{
        AsyncLogger, AsyncLoggerHandle, Database, LogFilter, Repository, TokenPreAggregator,
    };
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
    use nexusflow::router::Router as NexusRouter;
    use nexusflow::vendor::VendorManager;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    /// openai 型 vendor（透传路径：入站 OpenAI == 出站 OpenAI）。
    /// api_base 不带 /v1 —— manager 会拼 `{base}/v1/chat/completions`。
    fn make_vendor(id: &str, priority: u8, api_base: String) -> VendorConfig {
        crate::common::ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            supports_batch: false,
            name: format!("Test {}", id),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base,
            enabled: true,
            priority,
            models: vec![ModelConfig {
                model_id: "gpt-4".into(),
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

    fn make_config(vendors: Vec<VendorConfig>, cover_all_paths: bool) -> Config {
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
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: ResilienceSection {
                cover_all_paths,
                ..Default::default()
            },
            docs: Default::default(),
            lexcompress: Default::default(),
        }
    }

    /// 接线完整 ApiState（logger 落库以便断言 actual_vendor）。
    async fn make_state(
        config: Config,
    ) -> (Arc<ApiState>, Arc<AsyncLoggerHandle>, Arc<Repository>) {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

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

        let database = Arc::new(Database {
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
            pre_aggregator: Some(pa),
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        });
        (state, logger, repository)
    }

    fn app(state: Arc<ApiState>) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state)
    }

    /// POST /v1/chat/completions，可选 X-Vendor 提示（强制初始候选）。
    async fn post_chat(
        state: Arc<ApiState>,
        vendor_hint: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(v) = vendor_hint {
            builder = builder.header("x-vendor", v);
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

    fn chat_body(model: &str, stream: bool) -> Value {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 10,
            "stream": stream
        })
    }

    fn chat_ok_body(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{{"index":0,"message":{{"role":"assistant","content":"hi"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}}}"#
        )
    }

    /// ① passthrough_nonstream_fails_over：A 500 → B 成功。
    /// actual_vendor=B（记账口径：换路后记最终成功者）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn passthrough_nonstream_fails_over() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("internal server error")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_ok_body("from-b"))
            .expect(1)
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            true,
        );
        let (state, logger, repository) = make_state(config).await;

        // X-Vendor: fb-a 强制初始候选（否则 weighted 随机可能直接命中 B）
        let response = post_chat(state.clone(), Some("fb-a"), chat_body("gpt-4", false)).await;
        assert_eq!(response.status(), 200, "A 失败后必须换路到 B 成功");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["id"], "from-b", "响应必须来自 vendor B");

        // 换路记账：request_logs.actual_vendor == B，status_code == 200
        logger.shutdown().await.expect("shutdown");
        tokio::time::sleep(Duration::from_millis(150)).await;
        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].actual_vendor, "fb-b",
            "actual_vendor 必须记最终成功者（B），而非初始候选（A）"
        );
        assert_eq!(logs[0].status_code, Some(200));

        mock_a.assert_async().await; // A 恰好 1 次（无内层重试）
        mock_b.assert_async().await; // B 恰好 1 次
    }

    /// ② stream_fails_over_before_first_byte：A 流式 503（首字节前）→
    /// 重派 B，客户端拿到 B 的 SSE 流。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stream_fails_over_before_first_byte() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(503)
            .with_body("upstream overloaded")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"b-sse\"}\n\ndata: [DONE]\n\n")
            .expect(1)
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            true,
        );
        let (state, _logger, _repo) = make_state(config).await;

        let response = post_chat(state, Some("fb-a"), chat_body("gpt-4", true)).await;
        assert_eq!(
            response.status(),
            200,
            "A 流式在响应头前失败（503）必须换路到 B 建立 SSE 流"
        );
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "text/event-stream"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("b-sse"), "客户端必须拿到 B 的流内容: {body}");

        mock_a.assert_async().await; // A 恰好 1 次（首字节前失败）
        mock_b.assert_async().await; // B 恰好 1 次（重派）
    }

    /// ③ mid_stream_error_does_not_failover：B 流建立后中断 → 客户端收
    /// SSE error 事件终止，**不再重派**（OpenRouter 首字节后停止 failover）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mid_stream_error_does_not_failover() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(503)
            .with_body("down")
            .expect(1) // 恰好 1 次：B 流中断后不得回来重派 A
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            // 正常 chunk 后跟上游中流 error 事件
            .with_body(
                "data: {\"id\":\"b-partial\"}\n\nevent: ping\ndata: {}\n\nevent: error\ndata: {\"error\":{\"message\":\"mid-stream blowup\"}}\n\n",
            )
            .expect(1)
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            true,
        );
        let (state, _logger, _repo) = make_state(config).await;

        let response = post_chat(state, Some("fb-a"), chat_body("gpt-4", true)).await;
        assert_eq!(response.status(), 200, "流已建立（B）→ 200 SSE");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains("event: error"),
            "流中错误必须以 SSE error 事件终止: {body}"
        );

        mock_a.assert_async().await; // A 恰好 1 次 —— 流中错误不再换路
        mock_b.assert_async().await;
    }

    /// ④ cb_open_triggers_fallback：连烧 5 次 A（CB failure_threshold=5）
    /// 触发熔断 Open → 后续透传流量不再触达 A（路由层实时过滤或派发层
    /// CB 拒绝后换路），由 B 接管。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cb_open_triggers_fallback() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("burn")
            // expect_at_most(5) 上界锁「第 6 次请求不触达 A 且无隐藏重试」；
            // 不用精确 5：全量并行下 localhost 瞬态连接失败会把失败计入
            // 熔断但不产生 HTTP 命中，「恰 5 次失败」由 CB Open 断言承载
            .expect_at_most(5)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_ok_body("from-b"))
            .expect(6) // 5 次预热换路 + 1 次熔断后接管
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            true,
        );
        let (state, _logger, _repo) = make_state(config).await;

        // 预热：5 次 X-Vendor: fb-a → 每次 A 500（计 CB 失败）→ 换路 B 成功
        for i in 0..5 {
            let response = post_chat(state.clone(), Some("fb-a"), chat_body("gpt-4", false)).await;
            assert_eq!(response.status(), 200, "预热 {i} 应换路成功");
        }

        // A 的熔断器已 Open
        use limiteron::error::CircuitState;
        assert_eq!(
            state.vendor_manager.get_circuit_state("fb-a").await,
            Some(CircuitState::Open),
            "5 次连续失败后 A 的熔断器必须 Open（cover_all_paths 已把 CB 接入透传路径）"
        );

        // 熔断后的普通请求：不再触达 A，B 接管
        let response = post_chat(state, None, chat_body("gpt-4", false)).await;
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["id"], "from-b");

        mock_a.assert_async().await; // 第 6 次请求未触达 A（expect(5) 精确校验）
        mock_b.assert_async().await;
    }

    /// ⑤ direct 路由（"fb-a.gpt-4"）不做 get_status 预过滤（router.rs
    /// try_direct_route）→ 初始候选即熔断中的 A：派发层 CB 必须直接拒绝
    ///（无第 6 次 HTTP），证明一期 CB 包裹真实接入透传路径。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cb_open_rejects_passthrough_http_before_dispatch() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("burn")
            .expect_at_most(5) // 上界锁无隐藏重试；恰 5 次失败由 CB Open 断言承载
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_ok_body("from-b"))
            .expect(5)
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            true,
        );
        let (state, _logger, _repo) = make_state(config).await;

        for i in 0..5 {
            let response = post_chat(state.clone(), Some("fb-a"), chat_body("gpt-4", false)).await;
            assert_eq!(response.status(), 200, "预热 {i} 应换路成功");
        }

        // direct 形式绕过路由层可用性过滤 → 初始候选=A（CB Open）→
        // 派发层 CB 拒绝 → 无候选换路（direct 模型无 fallback 链）→ 503
        let response = post_chat(state, None, chat_body("fb-a.gpt-4", false)).await;
        assert_eq!(
            response.status(),
            503,
            "CB Open 必须在派发层拒绝透传（映射 503）"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("circuit open"),
            "错误消息须含 circuit open: {message}"
        );

        mock_a.assert_async().await; // 第 6 次请求零 HTTP（CB 无调用拒绝）
        mock_b.assert_async().await;
    }

    /// ⑥ 开关=false 对照（非流式）：A 500 → 客户端 503，B 存在且健康
    /// 但**零调用** —— 证明换路行为被 cover_all_paths 门控，关闭时与
    /// 历史行为逐位一致（防门控失效）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cover_off_no_failover_nonstream() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("internal server error")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_ok_body("from-b"))
            .expect(0) // B 一次都不得被调用
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            false,
        );
        let (state, _logger, _repo) = make_state(config).await;

        let response = post_chat(state, Some("fb-a"), chat_body("gpt-4", false)).await;
        assert_eq!(response.status(), 503, "开关关闭：A 失败原样冒泡为 503");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "vendor_unavailable");

        mock_a.assert_async().await;
        mock_b.assert_async().await; // B 零调用
    }

    /// ⑦ 开关=false 对照（流式）：A 流式 503 → 客户端 503 JSON（非 SSE），
    /// B 零调用 —— 流式换路同样被开关门控。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cover_off_no_failover_stream() {
        let mut server_a = mockito::Server::new_async().await;
        let mock_a = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(503)
            .with_body("down")
            .expect(1)
            .create_async()
            .await;
        let mut server_b = mockito::Server::new_async().await;
        let mock_b = server_b
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: {\"id\":\"b-sse\"}\n\ndata: [DONE]\n\n")
            .expect(0)
            .create_async()
            .await;

        let config = make_config(
            vec![
                make_vendor("fb-a", 1, server_a.url()),
                make_vendor("fb-b", 2, server_b.url()),
            ],
            false,
        );
        let (state, _logger, _repo) = make_state(config).await;

        let response = post_chat(state, Some("fb-a"), chat_body("gpt-4", true)).await;
        assert_eq!(response.status(), 503);
        assert_ne!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "text/event-stream",
            "开关关闭：失败不得伪装成 SSE 流"
        );

        mock_a.assert_async().await;
        mock_b.assert_async().await; // B 零调用
    }
}
