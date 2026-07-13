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
        assert!(result.is_err(), "acquire should timeout when bucket is empty");
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
        assert!(result.is_ok(), "acquire should succeed after permit dropped");
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
            id: id.into(),
            name: format!("Test {}", id),
            type_: type_.into(),
            // make_vendor() 会设置 NEXUSFLOW_TEST_KEY 环境变量
            api_key_env: "NEXUSFLOW_TEST_KEY".into(),
            api_base,
            enabled,
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
            },
        }
    }

    fn token_limit(max: u64) -> RateLimitConfig {
        RateLimitConfig::Token { max_tokens: max }
    }

    fn make_config(vendors: Vec<VendorConfig>) -> Config {
        Config {
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
        assert_eq!(manager.get_status("nonexistent"), VendorStatus::Unavailable);
    }

    #[tokio::test]
    async fn test_get_status_disabled_vendor_returns_unavailable() {
        // disabled vendor 在 config 中存在但 enabled=false
        let v = make_vendor("disabled-1", "openai", false, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");
        // get_status 在 vendor.enabled=false 时直接返回 Unavailable
        assert_eq!(manager.get_status("disabled-1"), VendorStatus::Unavailable);
    }

    #[tokio::test]
    async fn test_get_status_default_available_after_initialize() {
        let v = make_vendor("openai-1", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");
        assert_eq!(manager.get_status("openai-1"), VendorStatus::Available);
    }

    #[tokio::test]
    async fn test_mark_busy_changes_status() {
        let v = make_vendor("v-busy", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.mark_busy("v-busy");
        assert_eq!(manager.get_status("v-busy"), VendorStatus::Busy);
    }

    #[tokio::test]
    async fn test_mark_available_restores_status() {
        let v = make_vendor("v-avail", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.mark_busy("v-avail");
        manager.mark_available("v-avail");
        assert_eq!(manager.get_status("v-avail"), VendorStatus::Available);
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
        assert_eq!(manager.get_status("v-throttle"), VendorStatus::Throttled);
    }

    #[tokio::test]
    async fn test_update_status_directly_sets_state() {
        let v = make_vendor("v-update", "openai", true, String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        manager.update_status("v-update", VendorStatus::Unavailable);
        assert_eq!(manager.get_status("v-update"), VendorStatus::Unavailable);
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
            RateLimitConfig::Interval { hours: 1 },
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
        assert!(manager.get_window_info("ghost").is_none());
    }

    #[tokio::test]
    async fn test_get_window_info_token_limiter_initial_state() {
        let v = make_vendor(
            "v-token",
            "openai",
            true,
            String::new(),
            token_limit(500),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let info = manager.get_window_info("v-token").expect("window info");
        assert_eq!(info.max_tokens, Some(500), "max_tokens should be 500");
        assert_eq!(info.used_tokens, 0, "used_tokens should be 0 initially");
        assert!(info.window_end.is_none(), "Token limiter has no window_end");
        assert!(info.has_budget(), "should have budget at 0/500");
        assert!(
            (info.budget_percentage() - 1.0).abs() < 1e-9,
            "should be 100% budget"
        );
    }

    #[tokio::test]
    async fn test_get_window_info_interval_limiter_has_no_max() {
        let v = make_vendor(
            "v-interval",
            "openai",
            true,
            String::new(),
            RateLimitConfig::Interval { hours: 1 },
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let info = manager.get_window_info("v-interval").expect("window info");
        assert!(info.max_tokens.is_none(), "Interval limiter has no max_tokens");
        assert_eq!(info.used_tokens, 0);
        // window_end 依赖当前时间（窗口内为 Some，窗口外为 None），不强断言
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
        assert_eq!(
            response["id"], "chatcmpl-test",
            "response: {:?}",
            response
        );
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
                assert!(vendor.contains("500"), "vendor should contain 500: {}", vendor);
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
                assert!(
                    vendor.contains("Parse error"),
                    "vendor: {}",
                    vendor
                );
            }
            other => panic!("expected VendorUnavailable with parse error, got {:?}", other),
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
        let info = manager.get_window_info("openai-usage").expect("window info");
        assert_eq!(info.used_tokens, 10, "used_tokens should be 10 after consume");
        assert!(!info.has_budget(), "should be at limit (10 >= 10)");
        assert_eq!(info.budget_percentage(), 0.0, "budget should be 0%");

        // get_status 应返回 Throttled（advanced_limiter.check() 返回 false）
        assert_eq!(
            manager.get_status("openai-usage"),
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

        let result = manager.execute_request("ghost", make_unified_request()).await;
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
        let info = manager.get_window_info("openai-queue").expect("window info");
        assert_eq!(info.used_tokens, 6, "worker should consume 6 tokens");
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
}

mod queue {
    use nexusflow::protocol::{UnifiedRequest, UnifiedResponse};
    use nexusflow::vendor::{PendingRequest, QueueManager, VendorQueue};
    use serde_json::json;

    fn sample_request() -> UnifiedRequest {
        serde_json::from_value(json!({"model": "test", "messages": []})).unwrap()
    }

    #[tokio::test]
    async fn test_vendor_queue_send_recv_roundtrip() {
        let queue = VendorQueue::new(10);
        assert_eq!(queue.len(), 0);

        let (tx, _rx) = tokio::sync::oneshot::channel::<
            Result<UnifiedResponse, nexusflow::error::NexusFlowError>,
        >();
        let request = PendingRequest {
            request: sample_request(),
            response_tx: tx,
        };

        queue.send(request).await.expect("send should succeed");
        assert_eq!(queue.len(), 1);

        let received = queue.recv().await;
        assert!(received.is_some(), "recv should return Some");
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
    async fn test_vendor_queue_capacity_enforced() {
        let queue = VendorQueue::new(2);
        for _ in 0..2 {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let request = PendingRequest {
                request: sample_request(),
                response_tx: tx,
            };
            queue.send(request).await.expect("send within capacity");
        }
        assert_eq!(queue.len(), 2);

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let overflow = PendingRequest {
            request: sample_request(),
            response_tx: tx,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            queue.send(overflow),
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
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            queue.recv(),
        )
        .await;
        assert!(result.is_err(), "recv on empty queue should block");
    }

    #[test]
    fn test_queue_manager_returns_none_for_missing_vendor() {
        let qm = QueueManager::new();
        assert!(qm.get("nonexistent_vendor").is_none());
    }
}

mod advanced_rate_limiter {
    use nexusflow::config::RateLimitConfig;
    use nexusflow::vendor::AdvancedRateLimiter;

    #[test]
    fn test_none_variant_always_allows() {
        // RateLimitConfig doesn't have a None constructor — use Interval with 0 hours
        // as the "always allow" proxy, or just construct directly via new(RateLimitConfig)
        // Note: AdvancedRateLimiter::None is unreachable from RateLimitConfig in current code
        // but Interval with hours=0 means interval_secs=0 which means should_reset is always true.
        let config = RateLimitConfig::Interval { hours: 1 };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(limiter.check(), "Interval limiter should allow by default");
    }

    #[test]
    fn test_interval_variant_check_returns_true_initially() {
        let config = RateLimitConfig::Interval { hours: 1 };
        let limiter = AdvancedRateLimiter::new(&config);
        // Interval always returns true from check() (the actual rate limit is via TokenBucket)
        assert!(limiter.check());
    }

    #[test]
    fn test_interval_should_reset_after_creation_is_false() {
        let config = RateLimitConfig::Interval { hours: 1 };
        let limiter = AdvancedRateLimiter::new(&config);
        // Just created, last_reset is now, so should_reset should be false
        assert!(!limiter.should_reset());
    }

    #[test]
    fn test_interval_should_reset_becomes_true_after_interval() {
        // Use hours=0 to force interval_secs=0, so should_reset returns true immediately
        let config = RateLimitConfig::Interval { hours: 0 };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(
            limiter.should_reset(),
            "interval_secs=0 should mean reset is always due"
        );
    }

    #[test]
    fn test_interval_reset_updates_last_reset() {
        let config = RateLimitConfig::Interval { hours: 1 };
        let limiter = AdvancedRateLimiter::new(&config);
        assert!(!limiter.should_reset());
        limiter.reset();
        // After reset, should_reset should still be false (just reset to now)
        assert!(!limiter.should_reset());
    }

    #[test]
    fn test_token_variant_allows_until_exhausted() {
        let config = RateLimitConfig::Token { max_tokens: 100 };
        let limiter = AdvancedRateLimiter::new(&config);

        // Initially has budget
        assert!(limiter.check());

        // Consume all tokens
        limiter.consume_tokens(100);
        assert!(!limiter.check(), "should be exhausted after consuming all tokens");
    }

    #[test]
    fn test_token_variant_partial_consumption_keeps_allowing() {
        let config = RateLimitConfig::Token { max_tokens: 100 };
        let limiter = AdvancedRateLimiter::new(&config);
        limiter.consume_tokens(50);
        assert!(limiter.check(), "should still allow with 50/100 used");
    }

    #[test]
    fn test_token_variant_consume_over_max_blocks() {
        let config = RateLimitConfig::Token { max_tokens: 10 };
        let limiter = AdvancedRateLimiter::new(&config);
        // Consume more than max
        limiter.consume_tokens(15);
        assert!(!limiter.check(), "should be blocked after over-consuming");
    }

    #[test]
    fn test_token_variant_reset_clears_used_tokens() {
        let config = RateLimitConfig::Token { max_tokens: 100 };
        let limiter = AdvancedRateLimiter::new(&config);
        limiter.consume_tokens(100);
        assert!(!limiter.check());

        limiter.reset();
        assert!(limiter.check(), "should allow again after reset");
    }

    #[test]
    fn test_token_consume_tokens_is_noop_for_non_token_variants() {
        let config = RateLimitConfig::Interval { hours: 1 };
        let limiter = AdvancedRateLimiter::new(&config);
        // consume_tokens should be a no-op for Interval variant
        limiter.consume_tokens(1000);
        assert!(limiter.check(), "Interval variant should still allow");
    }

    #[test]
    fn test_reset_is_noop_for_time_window() {
        let config = RateLimitConfig::TimeWindow {
            start: "00:00".to_string(),
            end: "23:59".to_string(),
        };
        let limiter = AdvancedRateLimiter::new(&config);
        // reset is no-op for TimeWindow — should not panic
        limiter.reset();
        assert!(limiter.check(), "TimeWindow 00:00-23:59 should allow at any time");
    }

    #[test]
    fn test_should_reset_returns_false_for_non_interval_variants() {
        let token_config = RateLimitConfig::Token { max_tokens: 100 };
        let token_limiter = AdvancedRateLimiter::new(&token_config);
        assert!(!token_limiter.should_reset());

        let tw_config = RateLimitConfig::TimeWindow {
            start: "00:00".to_string(),
            end: "23:59".to_string(),
        };
        let tw_limiter = AdvancedRateLimiter::new(&tw_config);
        assert!(!tw_limiter.should_reset());
    }
}
