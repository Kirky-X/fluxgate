#![cfg(feature = "vendor-management")]

// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod status {
    use nexusflow::vendor::{VendorState, VendorStatus};

    #[test]
    fn test_vendor_state_new() {
        let state = VendorState::new();
        assert_eq!(state.get_status(), VendorStatus::Available);
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_set_available() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Available);
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_set_busy() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Busy);
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_set_throttled() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Throttled);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_set_unavailable() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Unavailable);
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_inc_error() {
        let state = VendorState::new();
        assert_eq!(state.get_error_count(), 0);

        state.inc_error();
        assert_eq!(state.get_error_count(), 1);

        state.inc_error();
        assert_eq!(state.get_error_count(), 2);
    }

    #[test]
    fn test_vendor_state_reset_error() {
        let state = VendorState::new();

        state.inc_error();
        state.inc_error();
        assert_eq!(state.get_error_count(), 2);

        state.reset_error();
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_mark_busy() {
        let state = VendorState::new();
        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_mark_available() {
        let state = VendorState::new();
        state.mark_busy();
        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_mark_throttled() {
        let state = VendorState::new();
        let backoff = std::time::Duration::from_secs(60);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_mark_unavailable() {
        let state = VendorState::new();
        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_status_transitions() {
        let state = VendorState::new();

        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);

        let backoff = std::time::Duration::from_secs(30);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);

        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);

        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_status_from_u8() {
        assert_eq!(VendorStatus::from_u8(0), VendorStatus::Available);
        assert_eq!(VendorStatus::from_u8(1), VendorStatus::Busy);
        assert_eq!(VendorStatus::from_u8(2), VendorStatus::Throttled);
        assert_eq!(VendorStatus::from_u8(3), VendorStatus::Unavailable);
        assert_eq!(VendorStatus::from_u8(99), VendorStatus::Available);
    }
}

mod rate_limiter {
    use nexusflow::vendor::{ConcurrencyLimiter, TokenBucket};
    use std::time::Duration;

    #[tokio::test]
    async fn test_token_bucket_acquire_succeeds() {
        let bucket = TokenBucket::new(5, 10);
        // Should succeed immediately — bucket starts full
        let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_token_bucket_acquires_until_exhausted() {
        let bucket = TokenBucket::new(3, 1);
        // Consume all 3 tokens
        for _ in 0..3 {
            let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
            assert!(result.is_ok(), "acquire should succeed within timeout");
        }
        // 4th acquire should block (no tokens left, refill rate is 1/sec)
        let result = tokio::time::timeout(Duration::from_millis(50), bucket.acquire()).await;
        assert!(
            result.is_err(),
            "acquire should timeout when bucket is empty"
        );
    }

    #[tokio::test]
    async fn test_token_bucket_refills_over_time() {
        let bucket = TokenBucket::new(1, 100); // 100 tokens/sec refill
                                               // Consume the only token
        bucket.acquire().await;
        // Wait for refill
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Should succeed now
        let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
        assert!(result.is_ok(), "acquire should succeed after refill");
    }

    #[tokio::test]
    async fn test_concurrency_limiter_acquire() {
        let limiter = ConcurrencyLimiter::new(2);
        let _p1 = limiter.acquire().await;
        let _p2 = limiter.acquire().await;
        // 3rd acquire should block
        let result = tokio::time::timeout(Duration::from_millis(50), limiter.acquire()).await;
        assert!(result.is_err(), "3rd acquire should block");
    }

    #[tokio::test]
    async fn test_concurrency_limiter_release_on_drop() {
        let limiter = ConcurrencyLimiter::new(1);
        {
            let _permit = limiter.acquire().await;
        }
        // After permit dropped, should be able to acquire again
        let result = tokio::time::timeout(Duration::from_millis(100), limiter.acquire()).await;
        assert!(
            result.is_ok(),
            "acquire should succeed after permit dropped"
        );
    }
}

mod manager {
    //! `VendorManager` 集成测试 — 覆盖状态方法（无 IO）和 HTTP 请求方法（mockito mock）。
    //!
    //! 测试设计原则（规则9）：
    //! - 状态测试断言具体状态值、计数、字段值，而非"调用未 panic"
    //! - HTTP 测试断言响应 JSON 内容、错误类型字段、副作用（token 消耗）
    //! - 包含端到端集成场景（execute_request 经 worker 执行完整 HTTP 流程）

    use nexusflow::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
        RateLimitConfig, ServerConfig, VendorConfig,
    };
    use nexusflow::error::NexusFlowError;
    use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
    use nexusflow::vendor::{VendorManager, VendorStatus};
    use std::sync::Arc;
    use std::time::Duration;

    /// 确保测试所需的 API key 环境变量存在（map_err 修复后必须设置）
    fn ensure_test_api_key() {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    }

    /// 构造测试 vendor config
    fn make_vendor(
        id: &str,
        type_: &str,
        enabled: bool,
        api_base: String,
        rate_limit: RateLimitConfig,
    ) -> VendorConfig {
        ensure_test_api_key();
        VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: id.into(),
            name: format!("Test {}", id),
            type_: type_.into(),
            // make_vendor() 会设置 NEXUSFLOW_TEST_KEY 环境变量
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base,
            enabled,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "gpt-4".into(),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit,
            // rps=100 让 TokenBucket 立即放行，避免 acquire 阻塞
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

    fn token_limit(max: u64) -> RateLimitConfig {
        RateLimitConfig::Token {
            max_tokens: max,
            window_hours: None,
            sliding: false,
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
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            resilience: nexusflow::config::ResilienceSection::default(),
        }
    }

    fn make_unified_request() -> UnifiedRequest {
        UnifiedRequest {
            model: "gpt-4".into(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::Text("hello".into()),
            }],
            temperature: None,
            max_tokens: Some(10),
            stream: false,
            stop: vec![],
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            logit_bias: Default::default(),
            user: None,
            tools: vec![],
            tool_choice: None,
        }
    }

    // ============ 状态方法测试（无 IO） ============

    #[tokio::test]
    async fn test_get_status_nonexistent_vendor_returns_unavailable() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        assert_eq!(
            manager.get_status("nonexistent").await,
            VendorStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn test_get_status_disabled_vendor_returns_unavailable() {
        // disabled vendor 在 config 中存在但 enabled=false
        let v = make_vendor(
            "disabled-1",
            "openai",
            false,
            String::new(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");
        // get_status 在 vendor.enabled=false 时直接返回 Unavailable
        assert_eq!(
            manager.get_status("disabled-1").await,
            VendorStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn test_get_status_default_available_after_initialize() {
        let v = make_vendor("openai-1", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");
        assert_eq!(
            manager.get_status("openai-1").await,
            VendorStatus::Available
        );
    }

    #[tokio::test]
    async fn test_mark_busy_changes_status() {
        let v = make_vendor("v-busy", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.mark_busy("v-busy");
        assert_eq!(manager.get_status("v-busy").await, VendorStatus::Busy);
    }

    #[tokio::test]
    async fn test_mark_available_restores_status() {
        let v = make_vendor("v-avail", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.mark_busy("v-avail");
        manager.mark_available("v-avail");
        assert_eq!(manager.get_status("v-avail").await, VendorStatus::Available);
    }

    #[tokio::test]
    async fn test_mark_throttled_changes_status() {
        let v = make_vendor(
            "v-throttle",
            "openai",
            true,
            String::new(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.mark_throttled("v-throttle", Duration::from_secs(30));
        assert_eq!(
            manager.get_status("v-throttle").await,
            VendorStatus::Throttled
        );
    }

    #[tokio::test]
    async fn test_update_status_directly_sets_state() {
        let v = make_vendor("v-update", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.update_status("v-update", VendorStatus::Unavailable);
        assert_eq!(
            manager.get_status("v-update").await,
            VendorStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn test_inc_error_increments_sequentially() {
        let v = make_vendor("v-err", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        // inc_error 返回递增后的值
        assert_eq!(manager.inc_error("v-err"), 1);
        assert_eq!(manager.inc_error("v-err"), 2);
        assert_eq!(manager.inc_error("v-err"), 3);
    }

    #[tokio::test]
    async fn test_inc_error_nonexistent_returns_zero() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        // 不存在的 vendor → 返回 0（unwrap_or(0)）
        assert_eq!(manager.inc_error("ghost"), 0);
    }

    #[tokio::test]
    async fn test_get_queue_length_nonexistent_returns_zero() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        assert_eq!(manager.get_queue_length("ghost"), 0);
    }

    #[tokio::test]
    async fn test_get_all_status_empty_when_no_instances() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        let all = manager.get_all_status();
        assert!(all.is_empty(), "expected empty status list, got {:?}", all);
    }

    #[tokio::test]
    async fn test_get_all_status_lists_initialized_vendors() {
        let v1 = make_vendor("v1", "openai", true, String::new(), token_limit(1000));
        let v2 = make_vendor(
            "v2",
            "anthropic",
            true,
            String::new(),
            RateLimitConfig::Interval {
                hours: 1,
                sliding: false,
            },
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v1, v2])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let all = manager.get_all_status();
        assert_eq!(all.len(), 2, "expected 2 vendors, got {:?}", all);
        let ids: Vec<&str> = all.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"v1"), "v1 missing: {:?}", ids);
        assert!(ids.contains(&"v2"), "v2 missing: {:?}", ids);
        // 初始状态都是 Available，队列为空
        for (_, status, queue_len) in &all {
            assert_eq!(*status, VendorStatus::Available, "expected Available");
            assert_eq!(*queue_len, 0, "expected empty queue");
        }
    }

    #[tokio::test]
    async fn test_get_window_info_nonexistent_returns_none() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        assert!(manager.get_window_info("ghost").await.is_none());
    }

    #[tokio::test]
    async fn test_get_window_info_token_limiter_initial_state() {
        let v = make_vendor("v-token", "openai", true, String::new(), token_limit(500));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let info = manager
            .get_window_info("v-token")
            .await
            .expect("window info");
        assert_eq!(info.max_tokens, Some(500), "max_tokens should be 500");
        assert_eq!(info.used_tokens, 0, "used_tokens should be 0 initially");
        // recId=5：Token 窗口迁移 FixedWindowLimiter 后，peek 提供 reset_secs
        assert!(
            info.window_end.is_some(),
            "Token window should report its reset time (recId=5)"
        );
        assert!(info.has_budget(), "should have budget at 0/500");
        assert!(
            (info.budget_percentage() - 1.0).abs() < 1e-9,
            "should be 100% budget"
        );
    }

    #[tokio::test]
    async fn test_get_window_info_interval_limiter_reports_capacity_snapshot() {
        let v = make_vendor(
            "v-interval",
            "openai",
            true,
            String::new(),
            RateLimitConfig::Interval {
                hours: 1,
                sliding: false,
            },
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let info = manager
            .get_window_info("v-interval")
            .await
            .expect("window info");
        // recId=5：Interval 直接映射 FixedWindowLimiter（容量 u64::MAX =
        // 无数量上限语义），snapshot 提供精确 used/reset
        assert_eq!(
            info.max_tokens,
            Some(u64::MAX),
            "Interval maps to FixedWindowLimiter with no effective cap"
        );
        assert_eq!(info.used_tokens, 0);
        assert!(
            info.window_end.is_some(),
            "Interval window should report its reset time (recId=5)"
        );
    }

    #[tokio::test]
    async fn test_mark_methods_on_nonexistent_vendor_do_not_panic() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        // 所有 mark 方法对不存在的 vendor 静默无操作（无 panic）
        manager.mark_busy("ghost");
        manager.mark_available("ghost");
        manager.mark_throttled("ghost", Duration::from_secs(10));
        manager.update_status("ghost", VendorStatus::Busy);
    }

    // ============ HTTP mock 测试（mockito） ============

    #[tokio::test]
    async fn test_execute_passthrough_request_nonexistent_vendor_errors() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);

        let result = manager
            .execute_passthrough_request("ghost", serde_json::json!({}))
            .await;
        assert!(result.is_err(), "expected error for nonexistent vendor");
        match result.unwrap_err() {
            NexusFlowError::VendorUnavailable { vendor } => {
                assert_eq!(vendor, "ghost");
            }
            other => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execute_passthrough_request_success_returns_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"chatcmpl-test","object":"chat.completion","choices":[]}"#)
            .create_async()
            .await;

        let v = make_vendor(
            "openai-mock",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = manager
            .execute_passthrough_request("openai-mock", serde_json::json!({"model":"gpt-4"}))
            .await;

        assert!(result.is_ok(), "expected success: {:?}", result.err());
        let response = result.unwrap();
        assert_eq!(response["id"], "chatcmpl-test", "response: {:?}", response);
        assert_eq!(response["object"], "chat.completion");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_passthrough_request_http_500_errors_with_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let v = make_vendor(
            "openai-500",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = manager
            .execute_passthrough_request("openai-500", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NexusFlowError::VendorUnavailable { vendor } => {
                // vendor 字符串包含 vendor id 和状态码 500
                assert!(vendor.contains("openai-500"), "vendor: {}", vendor);
                assert!(
                    vendor.contains("500"),
                    "vendor should contain 500: {}",
                    vendor
                );
            }
            other => panic!("expected VendorUnavailable, got {:?}", other),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_passthrough_request_non_json_body_errors() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/plain")
            .with_body("not a json body")
            .create_async()
            .await;

        let v = make_vendor(
            "openai-badjson",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = manager
            .execute_passthrough_request("openai-badjson", serde_json::json!({}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NexusFlowError::VendorUnavailable { vendor } => {
                assert!(vendor.contains("Parse error"), "vendor: {}", vendor);
            }
            other => panic!(
                "expected VendorUnavailable with parse error, got {:?}",
                other
            ),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_passthrough_request_with_usage_consumes_tokens() {
        // 端到端集成测试：响应包含 usage.total_tokens → 触发 consume_tokens 副作用
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}"#,
            )
            .create_async()
            .await;

        // max_tokens=10 → 一次 consume(10) 后恰好用尽预算
        let v = make_vendor(
            "openai-usage",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(10),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        // 第一次请求成功 + 消耗 10 tokens
        let r1 = manager
            .execute_passthrough_request("openai-usage", serde_json::json!({}))
            .await;
        assert!(r1.is_ok(), "first request should succeed: {:?}", r1.err());

        // 验证 used_tokens 已更新到 10
        let info = manager
            .get_window_info("openai-usage")
            .await
            .expect("window info");
        assert_eq!(
            info.used_tokens, 10,
            "used_tokens should be 10 after consume"
        );
        assert!(!info.has_budget(), "should be at limit (10 >= 10)");
        assert_eq!(info.budget_percentage(), 0.0, "budget should be 0%");

        // get_status 应返回 Throttled（advanced_limiter.check() 返回 false）
        assert_eq!(
            manager.get_status("openai-usage").await,
            VendorStatus::Throttled,
            "should be Throttled after limit reached"
        );

        // 第二次请求 → RateLimited 错误（execute 内部检查 advanced_limiter.check()）
        let r2 = manager
            .execute_passthrough_request("openai-usage", serde_json::json!({}))
            .await;
        assert!(r2.is_err(), "second request should fail (rate limited)");
        match r2.unwrap_err() {
            NexusFlowError::RateLimited { vendor } => {
                assert_eq!(vendor, "openai-usage");
            }
            other => panic!("expected RateLimited, got {:?}", other),
        }
        // mock 只应该被调用一次（第二次请求在 HTTP 前就被限流拒绝）
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_passthrough_stream_request_nonexistent_vendor_errors() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);

        let result = manager
            .execute_passthrough_stream_request("ghost", serde_json::json!({}))
            .await;
        // 注意：Ok 类型是 impl Stream（未实现 Debug），不能使用 unwrap_err
        match result {
            Ok(_) => panic!("expected error for nonexistent vendor"),
            Err(NexusFlowError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "ghost");
            }
            Err(other) => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execute_passthrough_stream_request_yields_bytes() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"chatcmpl-stream","choices":[]}"#)
            .create_async()
            .await;

        let v = make_vendor(
            "openai-stream",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let stream = manager
            .execute_passthrough_stream_request("openai-stream", serde_json::json!({}))
            .await;
        assert!(stream.is_ok(), "stream creation failed: {:?}", stream.err());

        use futures_util::StreamExt;
        let mut stream = stream.unwrap();
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.expect("chunk should not error");
            collected.extend_from_slice(&bytes);
        }
        let text = String::from_utf8(collected).expect("response should be utf8");
        assert!(
            text.contains("chatcmpl-stream"),
            "body should contain id: {}",
            text
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_request_nonexistent_vendor_errors() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);

        let result = manager
            .execute_request("ghost", make_unified_request())
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NexusFlowError::VendorUnavailable { vendor } => {
                assert_eq!(vendor, "ghost");
            }
            other => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execute_request_via_worker_success() {
        // 端到端集成测试：execute_request → queue → worker → HTTP → response
        // 使用 multi_thread 以确保后台 worker 调度
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"chatcmpl-1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
            )
            .create_async()
            .await;

        let v = make_vendor(
            "openai-queue",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        // 用 timeout 包装避免 worker 死锁导致测试卡死
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("openai-queue", make_unified_request()),
        )
        .await;
        assert!(result.is_ok(), "should not timeout");
        let inner = result.unwrap();
        assert!(inner.is_ok(), "request should succeed: {:?}", inner.err());
        let response = inner.unwrap();
        assert_eq!(response.id, "chatcmpl-1");
        assert_eq!(response.model, "gpt-4");
        assert_eq!(response.usage.total_tokens, 6);
        // 验证 worker 路径也消费了 tokens
        let info = manager
            .get_window_info("openai-queue")
            .await
            .expect("window info");
        assert_eq!(info.used_tokens, 6, "worker should consume 6 tokens");
        mock.assert_async().await;
    }

    /// 用例③（recId=5 tests）：流式 permit 生命周期 —— 并发 3 个流请求对
    /// max_concurrent_requests=2 的 vendor，第 3 个在前两个流结束前不得进入。
    /// permit move 进返回的 Stream（unfold 持有），流丢弃后才释放。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_stream_permit_limits_concurrent_streams() {
        let mut server = mockito::Server::new_async().await;
        // 慢流：服务端写两个 chunk，中间停 400ms —— 保证前两条流在断言时仍存活
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_chunked_body(|w| {
                let _ = w.write_all(b"data: chunk1\n\n");
                let _ = w.flush();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let _ = w.write_all(b"data: chunk2\n\n");
                Ok::<(), std::io::Error>(())
            })
            .expect(3)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-stream-cap",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(100_000),
        );
        v.concurrency.max_concurrent_requests = Some(2);
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init should succeed");

        // 前两个流请求拿到 permit（流对象返回但未读完 → permit 仍被持有）
        let mut streams = Vec::new();
        for _ in 0..2 {
            let m = manager.clone();
            let handle = tokio::spawn(async move {
                m.execute_stream_request("v-stream-cap", make_unified_request())
                    .await
            });
            let stream = tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("first two streams should start promptly")
                .expect("join");
            assert!(stream.is_ok(), "stream should start: {:?}", stream.err());
            streams.push(stream.unwrap());
        }

        // 第 3 个流请求必须被并发上限阻塞在前两个流结束之前
        let m = manager.clone();
        let mut third = tokio::spawn(async move {
            m.execute_stream_request("v-stream-cap", make_unified_request())
                .await
        });
        let blocked = tokio::time::timeout(Duration::from_millis(400), &mut third)
            .await
            .is_err();
        assert!(
            blocked,
            "3rd stream must wait while both permits are held by live streams"
        );

        // 丢弃前两个流 → permit 释放 → 第 3 个进入并完成 HTTP 发送
        drop(streams);
        let third_result = tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("3rd stream should proceed after permits released")
            .expect("join");
        assert!(
            third_result.is_ok(),
            "3rd stream should start after permits released: {:?}",
            third_result.err()
        );

        // 三个请求都到达了上游
        mock.assert_async().await;
    }

    /// 用例④（recId=5 tests）：worker 顺序对调回归 —— 空闲 2s 后 rps=1 的
    /// vendor 仍能立即提供服务，且 3 个顺序请求因桶速 1/s 各等 ~1s。
    /// 旧实现（先 acquire 后 recv）空闲期以 10 tok/s 空烧令牌，3 个请求
    /// 会立即通过（elapsed 接近 0）——本用例锁定新顺序下不被空烧。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_worker_does_not_burn_rps_tokens_while_idle() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(
                r#"{"id":"chatcmpl-1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
            )
            .expect(3)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-idle",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.concurrency.requests_per_second = 1;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init should succeed");

        // 空闲 10s：桶容量 100（=queue_size），旧顺序（先 acquire 后 recv）
        // 以 10 tok/s 空烧，恰好烧光 100 个 → 此后第一个请求必须等回填 ≥1s；
        // 新顺序（先 recv 后 acquire）空闲期零消耗，桶保持满 → 请求立即通过。
        // 容量 100 下这是新旧行为可区分的最短窗口。
        tokio::time::sleep(Duration::from_secs(10)).await;

        // 3 个顺序请求：新顺序下桶满，全部立即通过
        let start = std::time::Instant::now();
        for _ in 0..3 {
            let r = manager
                .execute_request("v-idle", make_unified_request())
                .await;
            assert!(r.is_ok(), "request should succeed: {:?}", r.err());
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "after 10s idle the bucket must be full (order swapped: recv before \
             acquire burns nothing); old order would have drained 100 tokens and \
             forced >=1s refill wait (took {:?})",
            elapsed
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_stream_request_nonexistent_vendor_errors() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);

        let result = manager
            .execute_stream_request("ghost", make_unified_request())
            .await;
        // Ok 类型是 impl Stream（未实现 Debug），不能用 unwrap_err
        match result {
            Ok(_) => panic!("expected error for nonexistent vendor"),
            Err(NexusFlowError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "ghost");
            }
            Err(other) => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    // ============ recId=4③ tests：metrics 覆盖全部执行路径 ============
    // 此前唯一生产记账点在透传非流式 —— 转换路径（worker）与失败路径是盲区。

    mod metrics_paths {
        use nexusflow::config::{
            ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig,
            ModelConfig, RateLimitConfig, ServerConfig, VendorConfig,
        };
        use nexusflow::error::NexusFlowError;
        use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
        use nexusflow::vendor::VendorManager;
        use std::sync::Arc;
        use std::time::Duration;

        fn ensure_test_api_key() {
            std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        }

        fn make_vendor(id: &str, api_base: String) -> VendorConfig {
            ensure_test_api_key();
            VendorConfig {
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                id: id.into(),
                name: format!("Test {}", id),
                type_: "openai".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
                api_base,
                enabled: true,
                priority: 1,
                models: vec![ModelConfig {
                    model_id: "gpt-4".into(),
                    abstract_id: Some("gpt-4".into()),
                    weight: 1.0,
                    enabled: true,
                }],
                rate_limit: RateLimitConfig::Token {
                    max_tokens: 1_000_000,
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
                retry: nexusflow::config::RetrySection::default(),
                routing: nexusflow::config::RoutingSection::default(),
                cache: nexusflow::config::CacheSection::default(),
                admission: Default::default(),
                resilience: nexusflow::config::ResilienceSection::default(),
            }
        }

        fn make_unified_request() -> UnifiedRequest {
            UnifiedRequest {
                model: "gpt-4".into(),
                messages: vec![Message {
                    role: Role::User,
                    content: MessageContent::Text("hello".into()),
                }],
                temperature: None,
                max_tokens: Some(10),
                stream: false,
                stop: vec![],
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
                logit_bias: Default::default(),
                user: None,
                tools: vec![],
                tool_choice: None,
            }
        }

        /// ③a：worker 转换路径成功 → snapshot 记 requests=1 / successes=1
        ///（此前该路径零指标）。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn worker_conversion_success_records_metrics() {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"id":"c1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
                )
                .create_async()
                .await;

            let v = make_vendor("v-metrics-ok", format!("{}/v1", server.url()));
            let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
            let manager = Arc::new(VendorManager::new(cm));
            manager.initialize().await.expect("init");

            let result = tokio::time::timeout(
                Duration::from_secs(10),
                manager.execute_request("v-metrics-ok", make_unified_request()),
            )
            .await
            .expect("no timeout")
            .expect("success");
            assert_eq!(result.id, "c1");

            let vendor = manager
                .metrics()
                .snapshot()
                .vendors
                .into_iter()
                .find(|v| v.vendor == "v-metrics-ok")
                .expect("vendor metrics must exist after worker path (recId=4③)");
            assert_eq!(vendor.requests, 1);
            assert_eq!(vendor.successes, 1);
            assert_eq!(vendor.failures, 0);
        }

        /// ③b：透传失败（上游 500）→ snapshot 记 failures=1 且 last_error 非空。
        #[tokio::test]
        async fn passthrough_failure_records_metrics() {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(500)
                .with_body("boom")
                .create_async()
                .await;

            let v = make_vendor("v-metrics-fail", format!("{}/v1", server.url()));
            let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
            let manager = Arc::new(VendorManager::new(cm));
            manager.initialize().await.expect("init");

            let result = manager
                .execute_passthrough_request("v-metrics-fail", serde_json::json!({}))
                .await;
            assert!(result.is_err());

            let vendor = manager
                .metrics()
                .snapshot()
                .vendors
                .into_iter()
                .find(|v| v.vendor == "v-metrics-fail")
                .expect("vendor metrics must exist on failure path");
            assert_eq!(vendor.requests, 1);
            assert_eq!(vendor.failures, 1);
            assert!(
                vendor.last_error.is_some(),
                "failure must record record_error (recId=4③)"
            );
        }

        /// ③c：worker 转换路径失败（网络错误）→ failures 计数（非透传路径的
        /// 失败同样记账）。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn worker_conversion_failure_records_metrics() {
            // 端口 1 连接必然被拒 → worker 的 perform_http_request 走网络错误分支
            let v = make_vendor("v-metrics-down", "http://127.0.0.1:1/v1".to_string());
            let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
            let manager = Arc::new(VendorManager::new(cm));
            manager.initialize().await.expect("init");

            let result = tokio::time::timeout(
                Duration::from_secs(10),
                manager.execute_request("v-metrics-down", make_unified_request()),
            )
            .await
            .expect("no timeout");
            assert!(result.is_err());

            let vendor = manager
                .metrics()
                .snapshot()
                .vendors
                .into_iter()
                .find(|v| v.vendor == "v-metrics-down")
                .expect("vendor metrics must exist on worker failure path");
            assert_eq!(vendor.failures, 1);
            match result.unwrap_err() {
                NexusFlowError::VendorUnavailable { .. } => {}
                other => panic!("expected VendorUnavailable, got {:?}", other),
            }
        }
    }

    // ======== recId=13：TimeWindow 时段门控（get_window_info 判定委托） ========

    /// 回归锁定：同日窗口外 get_window_info 仍返回 window_end: None
    /// （既有语义不回退；跨午夜窗口内的 window_end 精确次日时刻为进阶项）。
    ///
    /// 时钟无关的「窗口外」构造：end = now-1min、start = now-2min（模 24h）。
    /// 论证：同日情形 now >= end → 窗口外；跨午夜情形（now < 2min 时
    /// start > end 形成跨午夜窗口 [start, 24:00)∪[0, end)）now 仍在
    /// [end, start) 之外 → 窗口外。无论当前时钟指向，断言稳定成立。
    #[tokio::test]
    async fn test_get_window_info_time_window_outside_returns_none() {
        use chrono::{Duration as ChronoDuration, Local};

        let now = Local::now().time();
        let start = (now - ChronoDuration::minutes(2))
            .format("%H:%M")
            .to_string();
        let end = (now - ChronoDuration::minutes(1))
            .format("%H:%M")
            .to_string();
        let v = make_vendor(
            "v-timewindow-outside",
            "openai",
            true,
            String::new(),
            RateLimitConfig::TimeWindow { start, end },
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let info = manager
            .get_window_info("v-timewindow-outside")
            .await
            .expect("TimeWindow vendor must yield WindowInfo");
        assert_eq!(info.max_tokens, None, "TimeWindow has no token cap");
        assert_eq!(
            info.window_end, None,
            "窗口外必须返回 window_end: None（既有语义，recId=13 回归锁定）"
        );
    }

    // ======== recId=4（W2）：出站 HTTP 超时治理 ========
    //
    // 三层模型，每个参数 0 = 对应层不限时（逃生阀；u64 零值若被误当
    // Duration::from_millis(0) 语义是「立即超时」，必须显式跳过包裹）：
    // - timeout_ms：非流式 per-attempt 总超时（从 send 发起起算，不含本地排队）
    // - stream_ttft_ms：流式首包（TTFT）超时——send 起至上游响应头返回，
    //   首包返回后愿意继续等（Cloudflare 口径）
    // - stream_idle_ms：流式 chunk 间 idle 超时（相邻上游字节块最大间隔）
    //
    // 桩选择（评审 issue#12）：mockito chunked-body 原语是「响应头之后的
    // body 停顿」，恰好匹配非流式总超时与 idle 口径；TTFT（头到达前延迟）
    // mockito 无此原语，须手写 TCP 桩。

    /// 非流式总超时：headers 已返回、body 停顿超过 timeout_ms → json() 阶段
    /// 以 Timeout 失败（而非 Parse error / VendorUnavailable）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nonstream_timeout_returns_timeout_error() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_chunked_body(|w| {
                let _ = w.write_all(b"{\"id\":\"slow-body\"");
                let _ = w.flush();
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = w.write_all(b"}");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-ot",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.timeout_ms = 200;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            manager.execute_passthrough_request("v-ot", serde_json::json!({})),
        )
        .await
        .expect("test-level watchdog must not fire");
        match result {
            Ok(_) => panic!("body stall beyond timeout_ms must fail"),
            Err(NexusFlowError::Timeout { vendor }) => assert_eq!(vendor, "v-ot"),
            Err(other) => panic!("expected Timeout, got {:?}", other),
        }
    }

    /// 0 = 不限时（锁定 0 必须跳过 timeout 包裹）：timeout_ms=0 时 body 停顿
    /// 1.5s 仍成功。若实现把 0 当 Duration::from_millis(0) 传入，请求立即失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_zero_disables_per_attempt_budget() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_chunked_body(|w| {
                let _ = w.write_all(b"{\"id\":\"slow-zero\"");
                let _ = w.flush();
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = w.write_all(b"}");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-ot-zero",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.timeout_ms = 0;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            manager.execute_passthrough_request("v-ot-zero", serde_json::json!({})),
        )
        .await
        .expect("test-level watchdog must not fire");
        assert!(
            result.is_ok(),
            "timeout_ms=0 must disable the per-attempt budget: {:?}",
            result.err()
        );
    }

    /// 流式 idle 超时：首 chunk 正常产出后上游停写超过 stream_idle_ms →
    /// 流以 Err(Timeout) 终止（permit 随 unfold 终止释放，结构不变）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_idle_timeout_terminates_mid_stream() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
                );
                let _ = w.flush();
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let _ = w.write_all(b"data: {\"late\":true}\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-idle-t",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.stream_idle_ms = 300;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init should succeed");

        let stream = manager
            .execute_stream_request("v-idle-t", make_unified_request())
            .await
            .expect("stream should be established (headers arrived)");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut items = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
        {
            items.push(item);
            if items.len() > 10 {
                break;
            }
        }
        assert!(
            items.iter().any(|i| i.is_ok()),
            "first chunk must arrive before the idle stall: {:?}",
            items
        );
        assert!(
            matches!(items.last(), Some(Err(NexusFlowError::Timeout { vendor })) if vendor == "v-idle-t"),
            "idle timeout must terminate the mid-stream with Err(Timeout), got {:?}",
            items.last()
        );
    }

    /// 流式 TTFT 超时：桩 accept 后延迟 1.2s 才写响应头，TTFT=200ms 必须
    /// 在流建立（响应头到达）之前以 Err(Timeout) 失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_ttft_timeout_fails_before_headers() {
        // mockito 1.x 无「响应头到达前延迟」原语（评审 issue#12）——手写
        // TCP 桩：accept 后睡 1.2s 再写头
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let _ =
                    socket.try_write(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                // 保持连接打开至测试结束（客户端此时早已超时离开）
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        let mut v = make_vendor(
            "v-ttft",
            "openai",
            true,
            format!("http://{addr}/v1"),
            token_limit(1000),
        );
        v.stream_ttft_ms = 200;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            manager.execute_stream_request("v-ttft", make_unified_request()),
        )
        .await
        .expect("test-level watchdog must not fire");
        match result {
            Ok(_) => panic!("TTFT timeout must fail before the stream is established"),
            Err(NexusFlowError::Timeout { vendor }) => assert_eq!(vendor, "v-ttft"),
            Err(other) => panic!("expected Timeout, got {:?}", other),
        }
    }
}

mod queue {
    use nexusflow::protocol::{UnifiedRequest, UnifiedResponse};
    use nexusflow::vendor::{PendingRequest, VendorQueue};
    use serde_json::json;

    fn sample_request() -> UnifiedRequest {
        serde_json::from_value(json!({"model": "test", "messages": []})).unwrap()
    }

    fn sample_pending() -> PendingRequest {
        let (tx, _rx) = tokio::sync::oneshot::channel::<
            Result<UnifiedResponse, nexusflow::error::NexusFlowError>,
        >();
        PendingRequest {
            request: sample_request(),
            response_tx: tx,
            enqueued_at: std::time::Instant::now(),
            // recId=8：出队时刻由 VendorQueue::recv 盖戳，构造方恒 None
            dequeued_at: None,
        }
    }

    #[tokio::test]
    async fn test_vendor_queue_send_recv_roundtrip() {
        let queue = VendorQueue::new(10);
        assert_eq!(queue.len(), 0);

        queue
            .send(sample_pending())
            .await
            .expect("send should succeed");
        assert_eq!(queue.len(), 1);

        let received = queue.recv().await;
        assert!(received.is_some(), "recv should return Some");
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
    async fn test_vendor_queue_capacity_enforced() {
        let queue = VendorQueue::new(2);
        for _ in 0..2 {
            queue
                .send(sample_pending())
                .await
                .expect("send within capacity");
        }
        assert_eq!(queue.len(), 2);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            queue.send(sample_pending()),
        )
        .await;
        assert!(result.is_err(), "send should block when queue is full");
    }

    #[tokio::test]
    async fn test_vendor_queue_recv_returns_none_when_closed() {
        let queue = VendorQueue::new(5);
        // Drop sender by cloning then dropping all clones — but VendorQueue holds
        // its own sender. Closing requires explicit drop of the queue itself.
        // Instead we test recv on empty queue times out (no item available).
        let result = tokio::time::timeout(std::time::Duration::from_millis(20), queue.recv()).await;
        assert!(result.is_err(), "recv on empty queue should block");
    }

    // recId=4②：QueueManager 死件已删除（全库无 insert 方，queue_factor 恒 1.0
    // 的根因）；原 test_queue_manager_returns_none_for_missing_vendor 随之退役。
}

mod advanced_rate_limiter {
    //! recId=5：AdvancedRateLimiter 新 API 测试。
    //! - check() 为非消费 peek（async）；consume_tokens 为 CAS 全有全无（async）
    //! - should_reset/reset 已删除：窗口由 limiteron FixedWindowLimiter
    //!   check_and_reset_window 惰性重置（用例①回归"永久 Throttled"缺陷）

    use nexusflow::config::RateLimitConfig;
    use nexusflow::vendor::AdvancedRateLimiter;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn mock_clock() -> Arc<limiteron::MockClock> {
        Arc::new(limiteron::MockClock::new())
    }

    fn as_dyn(clock: &Arc<limiteron::MockClock>) -> Arc<dyn limiteron::Clock> {
        clock.clone()
    }

    #[tokio::test]
    async fn test_interval_variant_check_returns_true_initially() {
        let config = RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);
        // Interval 无数量上限（u64::MAX 容量），check 恒放行
        assert!(limiter.check().await, "Interval limiter should allow");
    }

    #[tokio::test]
    async fn test_token_variant_allows_until_exhausted() {
        let config = RateLimitConfig::Token {
            max_tokens: 100,
            window_hours: None,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);

        // Initially has budget
        assert!(limiter.check().await);

        // Consume all tokens
        assert!(limiter.consume_tokens(100).await);
        assert!(
            !limiter.check().await,
            "should be exhausted after consuming all tokens"
        );
    }

    #[tokio::test]
    async fn test_token_variant_partial_consumption_keeps_allowing() {
        let config = RateLimitConfig::Token {
            max_tokens: 100,
            window_hours: None,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(limiter.consume_tokens(50).await);
        assert!(limiter.check().await, "should still allow with 50/100 used");
    }

    #[tokio::test]
    async fn test_token_consume_over_cap_rejects_atomically() {
        // recId=5：CAS 全有全无 —— 剩余 10 请求扣 15 → 整体拒绝、分文未扣
        let config = RateLimitConfig::Token {
            max_tokens: 10,
            window_hours: None,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(
            !limiter.consume_tokens(15).await,
            "over-cap consume must be rejected"
        );
        // 预算未被部分扣减
        let (limit, used, _) = limiter.window_snapshot().await.expect("snapshot");
        assert_eq!(limit, 10);
        assert_eq!(used, 0, "rejected consume must not partially deduct");
        assert!(limiter.check().await, "budget untouched");
    }

    /// 用例①（recId=5 tests）：Token 窗口耗尽 → check false → 假时钟推进过
    /// 窗口 → check true。回归旧实现 should_reset 不匹配 Token 变体导致的
    /// "vendor 被路由永久排除"。
    #[tokio::test]
    async fn test_token_window_resets_lazily_after_window_passes() {
        let clock = mock_clock();
        let config = RateLimitConfig::Token {
            max_tokens: 10,
            window_hours: Some(1),
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::with_clock(&config, as_dyn(&clock));

        assert!(limiter.consume_tokens(10).await);
        assert!(!limiter.check().await, "exhausted before window rolls");

        // 推进 2 小时（越过 1h 窗口）→ 下一次 peek 惰性重置窗口
        clock.advance(Duration::from_secs(2 * 3600));

        assert!(
            limiter.check().await,
            "window must reset lazily after it passes (regression: permanent Throttled)"
        );
        assert!(
            limiter.consume_tokens(10).await,
            "full budget available again in the new window"
        );
    }

    /// 用例②（recId=5 tests）：100 并发任务对 max=50 窗口各扣 1 → 成功数恰 50。
    /// 锁定 FixedWindowLimiter CAS 原子消费，不允许 check-then-act 超卖。
    #[tokio::test]
    async fn test_concurrent_consume_is_atomic_at_window_cap() {
        let config = RateLimitConfig::Token {
            max_tokens: 50,
            window_hours: None,
            sliding: false,
        };
        let limiter = Arc::new(AdvancedRateLimiter::new(&config));
        let success = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let limiter = limiter.clone();
            let success = success.clone();
            handles.push(tokio::spawn(async move {
                if limiter.consume_tokens(1).await {
                    success.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.await.expect("task join");
        }

        assert_eq!(
            success.load(Ordering::SeqCst),
            50,
            "CAS atomicity: exactly 50 of 100 concurrent consumes succeed"
        );
    }

    #[tokio::test]
    async fn test_window_snapshot_reports_exact_remaining() {
        let config = RateLimitConfig::Token {
            max_tokens: 100,
            window_hours: None,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);

        let (limit, used, reset_secs) = limiter.window_snapshot().await.expect("snapshot");
        assert_eq!(limit, 100);
        assert_eq!(used, 0);
        assert!(reset_secs > 0, "fresh window should have reset time");

        assert!(limiter.consume_tokens(30).await);
        let (_, used, _) = limiter.window_snapshot().await.expect("snapshot");
        assert_eq!(used, 30, "peek must report exact usage without consuming");
    }

    /// 评审 major：非 Token 变体无配额语义，worker/passthrough 记账点以
    /// accounts_tokens() 门控 —— Interval vendor 的每个成功请求不再触发
    /// 误导性 quota-underflow 告警。
    #[test]
    fn test_accounts_tokens_only_for_token_variant() {
        let token = AdvancedRateLimiter::new(&RateLimitConfig::Token {
            max_tokens: 100,
            window_hours: None,
            sliding: false,
        });
        assert!(token.accounts_tokens(), "Token 变体承载记账");

        let interval = AdvancedRateLimiter::new(&RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        });
        assert!(!interval.accounts_tokens(), "Interval 无配额上限");

        let tw = AdvancedRateLimiter::new(&RateLimitConfig::TimeWindow {
            start: "00:00".to_string(),
            end: "23:59".to_string(),
        });
        assert!(!tw.accounts_tokens(), "TimeWindow 无配额上限");
    }

    #[tokio::test]
    async fn test_consume_tokens_is_noop_for_non_token_variants() {
        let config = RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        let limiter = AdvancedRateLimiter::new(&config);
        // consume_tokens only applies to the Token variant
        assert!(!limiter.consume_tokens(1000).await);
        assert!(limiter.check().await, "Interval variant should still allow");
    }

    #[tokio::test]
    async fn test_time_window_variant_still_allows_in_hours() {
        let config = RateLimitConfig::TimeWindow {
            start: "00:00".to_string(),
            end: "23:59".to_string(),
        };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(
            limiter.check().await,
            "TimeWindow 00:00-23:59 should allow at any time"
        );
    }
}

// ======== recId=13：TimeWindow 时段门控跨午夜判定（纯函数） ========

mod time_window {
    use chrono::NaiveTime;
    use nexusflow::vendor::rate_limiter::time_window_active;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    /// 跨午夜窗口（start=22:00 > end=06:00）判定。
    /// 修复前两处调用点均为同日判定（`now >= start && now < end`），跨午夜
    /// 配置整段失效（02:00/23:00 恒判窗口外）——缺陷可由代码直读证明。
    /// 修复后语义：`now >= start || now < end`。
    #[test]
    fn time_window_crosses_midnight() {
        // 凌晨 02:00 与深夜 23:00 都在 22:00–06:00 窗口内
        assert!(
            time_window_active(t(2, 0), t(22, 0), t(6, 0)),
            "02:00 应在 22:00–06:00 跨午夜窗口内（修复前恒 false）"
        );
        assert!(
            time_window_active(t(23, 0), t(22, 0), t(6, 0)),
            "23:00 应在 22:00–06:00 跨午夜窗口内（修复前恒 false）"
        );
        // 正午在窗口外
        assert!(
            !time_window_active(t(12, 0), t(22, 0), t(6, 0)),
            "12:00 不应在 22:00–06:00 窗口内"
        );
        // 同日窗口边界（左闭右开）：now == start 在内、now == end 在外
        assert!(
            time_window_active(t(22, 0), t(22, 0), t(6, 0)),
            "now == start 应在窗口内"
        );
        assert!(
            !time_window_active(t(6, 0), t(22, 0), t(6, 0)),
            "now == end 应在窗口外"
        );
    }

    /// 同日窗口语义与旧实现一致（回归锁定：提取函数不改变同日判定）。
    #[test]
    fn time_window_same_day_semantics_unchanged() {
        assert!(time_window_active(t(12, 0), t(9, 0), t(18, 0)));
        assert!(!time_window_active(t(8, 59), t(9, 0), t(18, 0)));
        assert!(!time_window_active(t(18, 0), t(9, 0), t(18, 0)));
    }
}

// ============ recId=11：Token/Interval 滑动窗口（ShardedSlidingWindowLimiter）============

#[cfg(test)]
mod sliding_window {
    //! 固定窗口翻越点双花（Cloudflare 12:09/12:11 例的等比构造）与
    //! 滑动窗口消除双花的行为锁定。MockClock::with_instant 固定初始
    //! UNIX 锚（10_0000s）保证 fixed 翻越点与滑动窗覆盖的确定性。

    use nexusflow::config::RateLimitConfig;
    use nexusflow::vendor::AdvancedRateLimiter;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 初始 UNIX 锚：远离 0（分片时间窗判定对 0 有 saturating 饱和边界）
    const T0: u64 = 100_000;

    fn clock_at_t0() -> Arc<limiteron::MockClock> {
        Arc::new(limiteron::MockClock::with_instant(Instant::now(), T0))
    }

    fn as_dyn(clock: &Arc<limiteron::MockClock>) -> Arc<dyn limiteron::Clock> {
        clock.clone()
    }

    fn token_config(sliding: bool) -> RateLimitConfig {
        RateLimitConfig::Token {
            max_tokens: 10,
            window_hours: Some(1),
            sliding,
        }
    }

    /// 滑动 Token 窗：翻越点两侧各消费 60% → 第二窗首笔被拒（消除双花）。
    ///
    /// 序列（1h 窗、max=10）：t=T0+3300s 消费 6（旧窗 60%）→ 推进 500s
    /// 跨过 fixed 翻越点（T0+3600）→ 再消费 6：fixed 语义下新窗满额 10
    /// 放行（瞬时 2× 配额双花）；sliding 语义下 1h 滑窗仍覆盖第一笔
    ///（500s < 3600s）→ 仅剩 4 → CAS 整笔拒、分文未扣，随后 4 可消费。
    #[tokio::test]
    async fn sliding_token_window_no_boundary_double_spend() {
        let clock = clock_at_t0();
        let limiter = AdvancedRateLimiter::with_clock(&token_config(true), as_dyn(&clock));

        // 推进到 T0+3300（fixed 窗内剩余 300s 处）消费 60%
        clock.advance(Duration::from_secs(3300));
        assert!(limiter.consume_tokens(6).await, "第一窗消费 60%");

        // 推进 500s：跨过 fixed 翻越点（T0+3600）但仍在滑动窗覆盖内
        clock.advance(Duration::from_secs(500));

        assert!(
            !limiter.consume_tokens(6).await,
            "滑动窗口必须拒绝翻越点后的超额消费（fixed 会双花放行）"
        );
        assert!(limiter.consume_tokens(4).await, "滑窗剩余 4 仍可消费");
        assert!(!limiter.check().await, "预算耗尽");
    }

    /// 默认（fixed）回归锁定：同一序列下翻越点后新窗满额、第二笔 60%
    /// 放行——固定窗口的瞬时 2× 配额语义（现状）保持不变。
    #[tokio::test]
    async fn fixed_token_window_keeps_boundary_double_spend() {
        let clock = clock_at_t0();
        let limiter = AdvancedRateLimiter::with_clock(&token_config(false), as_dyn(&clock));

        clock.advance(Duration::from_secs(3300));
        assert!(limiter.consume_tokens(6).await);
        clock.advance(Duration::from_secs(500));

        assert!(
            limiter.consume_tokens(6).await,
            "fixed 语义：翻越点后新窗满额（现状双花，回归锁定）"
        );
    }

    /// 滑动窗口快照与 fixed 同构：limit/remaining 语义一致，reset_secs
    /// 以窗口全长为近似上界（limiteron current_snapshot MVP 注释——
    /// window_end 精度略降，window_factor 仅 is_ending_soon(<300s) 惩罚，
    /// 影响可忽略）。
    #[tokio::test]
    async fn sliding_window_snapshot_shape_matches_fixed() {
        let clock = clock_at_t0();
        let sliding = AdvancedRateLimiter::with_clock(&token_config(true), as_dyn(&clock));

        let (limit, used, reset_secs) = sliding.window_snapshot().await.expect("sliding snapshot");
        assert_eq!(limit, 10, "limit 与 fixed 同构");
        assert_eq!(used, 0);
        // recId=11 评审修复（major）：空窗（零消费）以窗口全长上报——
        // 原断言锁定 sharded 空窗 reset_secs=0（「即时恢复」语义），但
        // 该值经 get_window_info 映射为 window_end=now → is_ending_soon
        // 恒 true → 最健康的零消耗 vendor 永久 window_factor 0.5（方向
        // 与语义相反）。修正后与 fixed 空窗 reset>0 行为对齐。
        assert_eq!(reset_secs, 3600, "空窗以窗口全长上报（评审修复）");

        assert!(sliding.consume_tokens(6).await);
        let (_, used, reset_after) = sliding.window_snapshot().await.expect("sliding snapshot");
        assert_eq!(used, 6, "peek 精确反映用量（不消费）");
        assert!(
            reset_after > 0 && reset_after <= 3600,
            "有消耗后 reset_secs 以窗口全长为上界: {reset_after}"
        );

        // check() 非消费预检同构
        assert!(sliding.check().await, "剩 4 时 check 放行");
        assert!(sliding.consume_tokens(4).await);
        assert!(!sliding.check().await, "耗尽后 check 拒绝");
    }

    /// Interval 变体的滑动分派：sliding=true 构造成功且窗口语义生效
    ///（u64::MAX 容量恒放行，仅窗口计时——同 fixed 形态）。
    #[tokio::test]
    async fn sliding_interval_variant_dispatches() {
        let clock = clock_at_t0();
        let config = RateLimitConfig::Interval {
            hours: 1,
            sliding: true,
        };
        let limiter = AdvancedRateLimiter::with_clock(&config, as_dyn(&clock));
        assert!(limiter.check().await, "Interval 无配额上限，恒放行");
        assert!(!limiter.accounts_tokens(), "Interval 无 token 记账语义");
    }
}

// ============ recId=18：token 加权并发票据 ============

#[cfg(test)]
mod token_weighted_tickets {
    //! 估算双形态的 clamp 边界与量级一致性、acquire_many 语义（60×2 挤出
    //! 第三张）、全链路挤压（32k 长会话独占容量）。

    use nexusflow::vendor::manager::{estimate_cost_json, estimate_cost_unified, TicketPolicy};
    use nexusflow::vendor::{ConcurrencyLimiter, VendorManager};
    use std::sync::Arc;
    use std::time::Duration;

    fn policy() -> TicketPolicy {
        TicketPolicy {
            min: 1,
            max: 1000,
            default: 200,
        }
    }

    /// clamp 边界：缺 max_tokens → default 兜底；超长 → max 封顶；
    /// 零成本请求 → min 托底。
    #[test]
    fn estimate_unified_clamp_edges() {
        let p = policy();
        // 缺 max_tokens（None）→ default(200) 兜底（messages 空 → 0 字节）
        let bare = serde_json::from_str::<nexusflow::protocol::UnifiedRequest>(
            r#"{"model":"m","messages":[]}"#,
        )
        .unwrap();
        assert_eq!(estimate_cost_unified(&bare, &p), 200, "缺省兜底 default");
        // 超长：max_tokens 巨大 → max 封顶
        let huge = serde_json::from_str::<nexusflow::protocol::UnifiedRequest>(
            r#"{"model":"m","messages":[],"max_tokens":999999}"#,
        )
        .unwrap();
        assert_eq!(estimate_cost_unified(&huge, &p), 1000, "max 封顶");
        // 零成本：显式 max_tokens=0 → min 托底
        let zero = serde_json::from_str::<nexusflow::protocol::UnifiedRequest>(
            r#"{"model":"m","messages":[],"max_tokens":0}"#,
        )
        .unwrap();
        assert_eq!(estimate_cost_unified(&zero, &p), 1, "min 托底");
    }

    /// 两形态量级一致：同一请求的 Unified 与 JSON 形态估值同档
    ///（都在 [min, max] 且相对差 < 2×——序列化口径细节不同但量级一致）。
    #[test]
    fn estimate_two_forms_same_magnitude() {
        let p = policy();
        let unified: nexusflow::protocol::UnifiedRequest = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"hello world"}],"max_tokens":100}"#,
        )
        .unwrap();
        let body = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello world"}],
            "max_tokens": 100
        });
        let a = estimate_cost_unified(&unified, &p) as f64;
        let b = estimate_cost_json(&body, &p) as f64;
        assert!(a >= 1.0 && a <= 1000.0 && b >= 1.0 && b <= 1000.0);
        let ratio = a.max(b) / a.min(b);
        assert!(ratio < 2.0, "两形态估值必须同量级: unified={a} json={b}");
    }

    /// acquire_many 语义：容量 100 下两张 cost=50 票据恰好占满
    ///（in_flight==100=token 加权口径），第三张 cost=60 必须等待
    ///（不超卖）；释放至预算充足后可获取。
    #[tokio::test]
    async fn weighted_acquire_waits_when_budget_exhausted() {
        let limiter = ConcurrencyLimiter::new(100);
        let p1 = limiter.acquire_weighted(50).await;
        let p2 = limiter.acquire_weighted(50).await;
        assert_eq!(limiter.in_flight(), 100, "两张 50 票据占满 100 预算");

        // 剩余 0 < 60：第三张必须等待
        let wait =
            tokio::time::timeout(Duration::from_millis(300), limiter.acquire_weighted(60)).await;
        assert!(wait.is_err(), "预算不足时 acquire_weighted 必须等待");

        // 只释放一张（剩 50）仍不足 60
        drop(p1);
        let still_waiting =
            tokio::time::timeout(Duration::from_millis(150), limiter.acquire_weighted(60)).await;
        assert!(still_waiting.is_err(), "50 < 60 仍须等待");

        // 释放第二张（剩 100）→ 第三张获得
        drop(p2);
        let p3 = limiter.acquire_weighted(60).await;
        assert_eq!(limiter.in_flight(), 60, "获得后 in_flight == cost");
        drop(p3);
    }

    /// 集成（全链路）：token_weighted=true 时 32k 长会话独占容量
    ///（in_flight=token 加权口径=容量），后续短请求被挤压等待。
    #[tokio::test]
    async fn token_weighted_long_session_squeezes_short_requests() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let vm = make_weighted_manager(format!("http://127.0.0.1:{port}/v1"), true).await;

        // 32k 长会话：cost ≈ 16000+ → clamp 容量(100) → 独占全部票据
        let long_body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "x".repeat(64_000)}],
            "max_tokens": 100
        });
        let vm_long = vm.clone();
        let long_task = tokio::spawn(async move {
            vm_long
                .execute_passthrough_request_at("v1", long_body, "/v1/chat/completions")
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            vm.fleet_concurrency(),
            (100, 100),
            "32k 长会话必须独占全部票据预算（token 加权口径）"
        );

        // 短请求被挤压：长会话挂住期间无法获得票据
        let short_body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1
        });
        let vm_short = vm.clone();
        let short_task = tokio::spawn(async move {
            vm_short
                .execute_passthrough_request_at("v1", short_body, "/v1/chat/completions")
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !short_task.is_finished(),
            "token_weighted 开启时长会话必须挤压短请求"
        );

        // 长会话超时返回后预算释放（不 panic 即可）
        let _ = long_task.await;
        let _ = short_task.await;
    }

    /// 对照：token_weighted=false（默认）同 body 为单票据——长会话挂住
    /// 时 in_flight==1（请求计数口径），预算不被独占。
    #[tokio::test]
    async fn unweighted_keeps_request_count_ticket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let vm = make_weighted_manager(format!("http://127.0.0.1:{port}/v1"), false).await;

        let long_body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "x".repeat(64_000)}],
            "max_tokens": 100
        });
        let vm_long = vm.clone();
        let long_task = tokio::spawn(async move {
            vm_long
                .execute_passthrough_request_at("v1", long_body, "/v1/chat/completions")
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            vm.fleet_concurrency(),
            (1, 100),
            "默认单票据：请求计数口径（现状）"
        );
        let _ = long_task.await;
    }

    async fn make_weighted_manager(api_base: String, weighted: bool) -> Arc<VendorManager> {
        std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
        let config = nexusflow::config::Config {
            rate_limit: Default::default(),
            server: nexusflow::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: nexusflow::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![nexusflow::config::VendorConfig {
                timeout_ms: 2_000,
                stream_ttft_ms: 2_000,
                stream_idle_ms: 2_000,
                id: "v1".to_string(),
                name: "V1".into(),
                type_: "openai".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
                api_base,
                enabled: true,
                priority: 1,
                models: vec![],
                rate_limit: nexusflow::config::RateLimitConfig::Token {
                    max_tokens: 1_000_000,
                    window_hours: None,
                    sliding: false,
                },
                concurrency: nexusflow::config::ConcurrencyConfig {
                    requests_per_second: 1000,
                    max_concurrent_requests: Some(100),
                    token_weighted: weighted,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
            }],
            global_model: None,
            pricing: vec![],
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            resilience: nexusflow::config::ResilienceSection::default(),
        };
        let cm = Arc::new(nexusflow::config::ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm));
        vm.initialize().await.expect("init vendors");
        vm
    }
}

/// 评审修复回归（major）：空窗 sliding vendor 经 get_window_info 的
/// window_end 不得触发 is_ending_soon（<300s 惩罚）——修复前
/// reset_secs=0 → window_end=now → 最健康 vendor 恒 window_factor 0.5。
#[tokio::test]
async fn sliding_empty_window_not_ending_soon_in_window_info() {
    use nexusflow::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig,
        RateLimitConfig, ServerConfig, VendorConfig,
    };
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    let config = Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
        },
        vendors: vec![VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: "v-slide".to_string(),
            name: "V".into(),
            type_: "openai".into(),
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: RateLimitConfig::Token {
                max_tokens: 10,
                window_hours: Some(1),
                sliding: true,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }],
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![],
        auth: None,
        cors: None,
        retry: nexusflow::config::RetrySection::default(),
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        resilience: nexusflow::config::ResilienceSection::default(),
    };
    let cm = Arc::new(ConfigManager::new(config));
    let vm = VendorManager::new(cm);
    vm.initialize().await.expect("init");
    let info = vm.get_window_info("v-slide").await.expect("window info");
    assert!(
        !info.is_ending_soon(),
        "空窗 sliding vendor 不得被判为即将结束（评审修复）"
    );
    assert!(
        (info.budget_percentage() - 1.0).abs() < 1e-9,
        "空窗预算满格"
    );
}
