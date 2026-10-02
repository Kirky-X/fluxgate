// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Integration tests for `nexusflow::utils::retry` module + recId=7 重试升级。
//!
//! 测试覆盖：
//! - `ErrorCategory`、`RetryStrategy`、`BackoffStrategy`、`extract_status_code`、
//!   `is_retryable_error`（无 feature 门控）
//! - `with_retry`（手写实现，重试延迟已迁 limiteron）
//! - recId=7①：`limiteron::retry::delay_for_attempt` 首试 1s（回归旧实现的
//!   2s 起步偏移）
//! - recId=7②：`retry_delay_for` 纯函数（Retry-After 覆盖 / 分类退避），
//!   不使用 tokio::time::pause（在库先例 vendor_circuit_breaker_test.rs:9-14
//!   已记录 paused clock auto-advance 与真实 mockito IO 冲突，禁用该组合）
//! - recId=7⑤：换 vendor fallback —— mockito 集成（真实时钟）：
//!   主 vendor 重试耗尽 → 第二 vendor 接管且客户端拿到成功响应；
//!   仅一个合格 vendor 失败 → 503 冒泡而非死循环

#[cfg(test)]
mod tests {
    use nexusflow::error::NexusFlowError;
    use nexusflow::utils::{
        extract_status_code, is_retryable_error, BackoffStrategy, ErrorCategory, RetryStrategy,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // -------- ErrorCategory --------

    #[test]
    fn test_error_category_from_status_code_fatal() {
        assert_eq!(ErrorCategory::from_status_code(400), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(401), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(403), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(404), ErrorCategory::Fatal);
    }

    #[test]
    fn test_error_category_from_status_code_retryable_exponential() {
        assert_eq!(
            ErrorCategory::from_status_code(429),
            ErrorCategory::RetryableExponential
        );
    }

    #[test]
    fn test_error_category_from_status_code_retryable_fixed() {
        assert_eq!(
            ErrorCategory::from_status_code(500),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(502),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(503),
            ErrorCategory::RetryableFixed
        );
    }

    #[test]
    fn test_error_category_from_status_code_unknown_defaults_to_fatal() {
        assert_eq!(ErrorCategory::from_status_code(418), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(999), ErrorCategory::Fatal);
    }

    #[test]
    fn test_error_category_from_error_message_timeout() {
        let cat = ErrorCategory::from_error_message("Request timeout");
        assert_eq!(cat, ErrorCategory::RetryableShort);

        let cat = ErrorCategory::from_error_message("Connection timed out");
        assert_eq!(cat, ErrorCategory::RetryableShort);
    }

    #[test]
    fn test_error_category_is_retryable() {
        assert!(!ErrorCategory::Fatal.is_retryable());
        assert!(ErrorCategory::RetryableExponential.is_retryable());
        assert!(ErrorCategory::RetryableFixed.is_retryable());
        assert!(ErrorCategory::RetryableShort.is_retryable());
    }

    // -------- RetryStrategy / BackoffStrategy（存量锁定） --------

    #[test]
    fn test_retry_strategy_for_exponential_category() {
        let strategy = RetryStrategy::for_category(ErrorCategory::RetryableExponential);
        assert_eq!(strategy.max_retries, 5);
        match strategy.backoff {
            BackoffStrategy::Exponential { base_ms, max_ms } => {
                assert_eq!(base_ms, 1000);
                assert_eq!(max_ms, 16000);
            }
            _ => panic!("expected Exponential backoff"),
        }
    }

    #[test]
    fn test_backoff_fixed_delay_constant() {
        let backoff = BackoffStrategy::Fixed { interval_ms: 1000 };
        assert_eq!(backoff.delay(0), Duration::from_millis(1000));
        assert_eq!(backoff.delay(10), Duration::from_millis(1000));
    }

    // -------- extract_status_code / is_retryable_error --------

    #[test]
    fn test_extract_status_code_with_paren_format() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
    }

    #[test]
    fn test_extract_status_code_no_status_returns_default_500() {
        assert_eq!(extract_status_code("no status code here"), 500);
    }

    #[test]
    fn test_is_retryable_error_variants() {
        assert!(is_retryable_error(&NexusFlowError::Timeout {
            vendor: "v".into()
        }));
        assert!(is_retryable_error(&NexusFlowError::VendorUnavailable {
            vendor: "v (429: Rate limit)".into()
        }));
        assert!(!is_retryable_error(&NexusFlowError::VendorUnavailable {
            vendor: "v (400: Bad Request)".into()
        }));
        assert!(!is_retryable_error(&NexusFlowError::Config {
            message: "x".into()
        }));
    }

    // -------- with_retry（手写实现，延迟计算已迁 limiteron） --------

    #[tokio::test]
    async fn test_with_retry_success_first_attempt_no_initial_error() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = nexusflow::utils::retry::with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(42)
                }
            },
            None,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_with_retry_succeeds_after_retryable_failures() {
        let initial = NexusFlowError::Timeout {
            vendor: "test_vendor".into(),
        };
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = nexusflow::utils::retry::with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n < 1 {
                        Err(NexusFlowError::Timeout {
                            vendor: "test_vendor".into(),
                        })
                    } else {
                        Ok(42)
                    }
                }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    // -------- recId=7①：delay_for_attempt 首试偏移回归 --------

    /// 旧实现「先 retry_count += 1 再 2^retry_count」导致首试 2s，
    /// 与注释 '1s, 2s, 4s' 矛盾。limiteron delay_for_attempt 的 attempt
    /// 从 1 起：attempt=1 → initial × factor^0 = 1s。
    #[cfg(feature = "vendor-management")]
    #[test]
    fn test_delay_for_attempt_first_attempt_is_initial_delay() {
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                1,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(1),
            "首试延迟必须是 initial（1s），回归旧实现 2s 起步偏移"
        );
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                2,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(2)
        );
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                5,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(16),
            "指数退避应封顶 max_delay"
        );
    }

    // -------- recId=7②：retry_delay_for 纯函数（无 IO、无 paused clock） --------

    #[cfg(feature = "vendor-management")]
    mod retry_delay_for_fn {
        use super::*;
        use nexusflow::config::RetrySection;
        use nexusflow::utils::retry::{retry_delay_for, RetryDelayPolicy};
        use rand::SeedableRng;

        /// spread_jitter=0 的存量断言不消耗 RNG；种子任意（固定以满足可复现）。
        fn legacy_rng() -> rand::rngs::StdRng {
            rand::rngs::StdRng::seed_from_u64(0)
        }

        fn deterministic_policy() -> RetryDelayPolicy {
            RetryDelayPolicy {
                initial_delay: Duration::from_secs(1),
                factor: 2.0,
                max_delay: Duration::from_secs(16),
                jitter: 0.0,        // 确定性：便于纯函数断言
                spread_jitter: 0.0, // recId=5 回滚阀：存量断言锁定旧确定性语义
            }
        }

        #[test]
        fn retry_after_header_takes_precedence() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(Duration::from_secs(7)),
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(
                delay,
                Duration::from_secs(7),
                "429 带 Retry-After(7s) → 直接采用上游指示"
            );
        }

        #[test]
        fn exponential_category_uses_delay_for_attempt() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                2,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(
                delay,
                Duration::from_secs(2),
                "429 无头 → delay_for_attempt 值（attempt=2 → 2s）"
            );
        }

        #[test]
        fn fixed_category_uses_constant_one_second() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableFixed,
                3,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(delay, Duration::from_secs(1), "5xx → 固定 1s");
        }

        #[test]
        fn fatal_category_zero_delay() {
            let delay = retry_delay_for(
                ErrorCategory::Fatal,
                1,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(delay, Duration::ZERO);
        }

        #[test]
        fn policy_from_config_section() {
            let section = RetrySection {
                initial_delay_ms: 250,
                factor: 3.0,
                max_delay_ms: 4000,
                jitter: 0.1,
                spread_jitter: 0.7,
                ..Default::default()
            };
            let policy = RetryDelayPolicy::from_config(&section);
            assert_eq!(policy.initial_delay, Duration::from_millis(250));
            assert!((policy.factor - 3.0).abs() < 1e-9);
            assert_eq!(policy.max_delay, Duration::from_millis(4000));
            assert!((policy.jitter - 0.1).abs() < 1e-9);
            assert!((policy.spread_jitter - 0.7).abs() < 1e-9);
        }

        /// [retry].spread_jitter 缺省必须为 1.0（修复本体：全范围展宽即
        /// 惊群修复），[retry] 段缺省也必须带出该值。
        #[test]
        fn retry_section_default_spread_jitter_is_one() {
            let section = RetrySection::default();
            assert!((section.spread_jitter - 1.0).abs() < 1e-9);
            let policy = RetryDelayPolicy::from_config(&section);
            assert!((policy.spread_jitter - 1.0).abs() < 1e-9);
        }

        // ======== recId=5：同步惊群展宽 ========

        /// spread_jitter=1.0（默认）下 Retry-After 分支必须以 ra 为 base
        /// 全范围展宽 ∈ [ra, 2×ra)（半开：gen_range(0.0..spread) 永不取上
        /// 界）。修复前恒等于 ra——上游同一 Retry-After 下所有并发在同一
        /// 刻唤醒重试（同步惊群源，Red 实测：delay == 7s）。
        ///
        /// 固定种子（StdRng::seed_from_u64）注入，确定性断言精确值。
        #[test]
        fn retry_after_gets_full_range_spread() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
            let ra = Duration::from_secs(7);

            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(ra),
                &policy,
                &mut rng,
            );

            assert!(
                delay >= ra && delay < ra + ra,
                "Retry-After 分支必须展宽到 [ra, 2×ra)，got {:?}",
                delay
            );
            assert!(
                delay > ra,
                "展宽后必须 > ra（修复前恒 == ra），got {:?}",
                delay
            );
        }

        /// spread_jitter=1.0 下 RetryableFixed（5xx）分支必须以 1s 为 base
        /// 展宽 ∈ [1s, 2s)。修复前恒 1s——5xx 惊群下所有实例同步重试
        /// （Red 实测：delay == 1s）。
        #[test]
        fn retryable_fixed_gets_full_range_spread() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);

            let delay = retry_delay_for(ErrorCategory::RetryableFixed, 3, None, &policy, &mut rng);

            assert!(
                delay > Duration::from_secs(1) && delay <= Duration::from_secs(2),
                "5xx 固定间隔分支必须展宽到 (1s, 2s]，got {:?}（修复前恒 = 1s）",
                delay
            );
        }

        /// 回滚阀锁定：spread_jitter=0 完全恢复旧行为——Retry-After 原样
        /// 返回、5xx 固定 1s（且不消耗 RNG：两分支确定性，与旧实现逐位
        /// 一致）。
        #[test]
        fn spread_jitter_zero_restores_legacy_delay() {
            let policy = deterministic_policy(); // spread_jitter = 0.0
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);

            let delay_after = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(Duration::from_secs(7)),
                &policy,
                &mut rng,
            );
            assert_eq!(
                delay_after,
                Duration::from_secs(7),
                "spread_jitter=0：Retry-After 必须原样返回（旧行为）"
            );

            let delay_fixed =
                retry_delay_for(ErrorCategory::RetryableFixed, 3, None, &policy, &mut rng);
            assert_eq!(
                delay_fixed,
                Duration::from_secs(1),
                "spread_jitter=0：5xx 固定间隔必须恒 1s（旧行为）"
            );
        }

        /// 固定种子精确值断言：同种子产出与 RNG 序列一致（漂移检测），
        /// 不同种子产出不同延迟（证明 RNG 真实参与，非伪注入）。
        #[test]
        fn spread_delay_is_seed_deterministic_and_seed_sensitive() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;

            let mut rng42 = rand::rngs::StdRng::seed_from_u64(42);
            let d42 = retry_delay_for(ErrorCategory::RetryableFixed, 1, None, &policy, &mut rng42);
            // StdRng(42).gen_range(0.0..1.0) 的值由 Cargo.lock 固定的 rand
            // 版本决定；此处锁定该精确值（重锁依赖导致漂移时显式暴露）。
            assert_eq!(
                d42,
                Duration::from_nanos(1_526_557_409),
                "固定种子精确值漂移：rand 版本或实现变更，需重新固化"
            );

            let mut rng7 = rand::rngs::StdRng::seed_from_u64(7);
            let d7 = retry_delay_for(ErrorCategory::RetryableFixed, 1, None, &policy, &mut rng7);
            assert_ne!(d42, d7, "不同种子必须产生不同延迟（证明 RNG 真实参与）");
        }
    }

    // -------- recId=7⑤：换 vendor fallback（mockito 集成，真实时钟） --------

    #[cfg(all(feature = "web-server", feature = "resilience"))]
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

        fn ensure_test_api_key() {
            std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        }

        /// type_ = "anthropic"：入站 OpenAI 请求走转换路径
        /// （execute_request），fallback 循环生效；避免同格式透传绕过。
        fn make_vendor(id: &str, priority: u8, api_base: String) -> VendorConfig {
            ensure_test_api_key();
            VendorConfig {
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                id: id.into(),
                name: format!("Test {}", id),
                type_: "anthropic".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
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
                },
                vendors,
                global_model: Some(GlobalModelConfig {
                    enabled: true,
                    strategy: "weighted".into(),
                }),
                pricing: vec![],
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
                resilience: nexusflow::config::ResilienceSection::default(),
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
}
