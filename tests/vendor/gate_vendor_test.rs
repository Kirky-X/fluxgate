// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(feature = "vendor-management")]

#[path = "../common/mod.rs"]
mod common;

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
    //! `VendorManager` 集成测试 — 覆盖状态方法（无 IO）和 HTTP 请求方法
    //! （mockito mock），以及端到端集成场景（execute_request 经 worker
    //! 执行完整 HTTP 流程）。

    use crate::common;
    use nexusflow::config::{Config, ConfigManager, RateLimitConfig, ServerConfig, VendorConfig};
    use nexusflow::error::GateError;
    use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
    use nexusflow::vendor::{VendorManager, VendorStatus};
    use std::sync::Arc;
    use std::time::Duration;

    /// 构造测试 vendor config：type_/enabled/rate_limit 等差异字段参数化传入，
    /// 其余走共享基座；rps=100 让 TokenBucket 立即放行（显式覆盖基座 1000）。
    fn make_vendor(
        id: &str,
        type_: &str,
        enabled: bool,
        api_base: String,
        rate_limit: RateLimitConfig,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.type_ = type_.into();
        v.enabled = enabled;
        v.rate_limit = rate_limit;
        v.concurrency.requests_per_second = 100;
        v
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
            // 本文件语义：0.0.0.0:8080（共享基座为 127.0.0.1:0，显式保留）
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vendors)
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
            ..Default::default()
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
        // Token 窗口迁移 FixedWindowLimiter 后，peek 提供 reset_secs
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
        // Interval 直接映射 FixedWindowLimiter（容量 u64::MAX =
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
            GateError::VendorUnavailable { vendor } => {
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

    /// `extra_headers` 随出站请求注入（mockito 按头匹配断言）；
    /// 与网关鉴权头冲突的配置项被跳过（出站仍为网关值）。
    #[tokio::test]
    async fn test_execute_passthrough_request_injects_extra_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("x-tenant-id", "acme")
            .match_header("authorization", "Bearer test-key-for-mockito")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"chatcmpl-hdr","object":"chat.completion","choices":[]}"#)
            .create_async()
            .await;

        let mut v = make_vendor(
            "openai-hdr",
            "openai",
            true,
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.extra_headers.insert("x-tenant-id".into(), "acme".into());
        // 冲突项：出站 Authorization 必须仍为网关值（mockito 精确匹配锁定）
        v.extra_headers
            .insert("authorization".into(), "Bearer evil".into());
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init should succeed");

        let result = manager
            .execute_passthrough_request("openai-hdr", serde_json::json!({"model":"gpt-4"}))
            .await;

        assert!(result.is_ok(), "expected success: {:?}", result.err());
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
            GateError::VendorUnavailable { vendor } => {
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
            GateError::VendorUnavailable { vendor } => {
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
            GateError::RateLimited { vendor } => {
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
            Err(GateError::VendorUnavailable { vendor }) => {
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
            GateError::VendorUnavailable { vendor } => {
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

    /// 流式 permit 生命周期：并发 3 个流请求对 max_concurrent_requests=2 的
    /// vendor，第 3 个在前两个流结束前不得进入；permit move 进返回的
    /// Stream（unfold 持有），流丢弃后才释放。
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

    /// worker 顺序回归：空闲后 rps=1 的 vendor 仍能立即提供服务，且 3 个
    /// 顺序请求因桶速 1/s 各等 ~1s——空闲期不得空烧令牌。
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

        // 空闲 10s：桶容量 100（=queue_size）。空闲期零消耗，桶保持满 →
        // 请求立即通过（空烧会耗尽令牌并强制等待回填）。
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
            Err(GateError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "ghost");
            }
            Err(other) => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    // ============ tests：metrics 覆盖全部执行路径 ============
    // 此前唯一生产记账点在透传非流式 —— 转换路径（worker）与失败路径是盲区。

    mod metrics_paths {
        use crate::common;
        use nexusflow::config::{Config, ConfigManager, ServerConfig, VendorConfig};
        use nexusflow::error::GateError;
        use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
        use nexusflow::vendor::VendorManager;
        use std::sync::Arc;
        use std::time::Duration;

        /// 本文件语义：rps=100（TokenBucket 立即放行，显式覆盖基座 1000），
        /// 其余走共享基座
        fn make_vendor(id: &str, api_base: String) -> VendorConfig {
            common::ensure_test_api_key();
            let mut v = common::base_vendor(id, api_base);
            v.concurrency.requests_per_second = 100;
            v
        }

        fn make_config(vendors: Vec<VendorConfig>) -> Config {
            Config {
                // 本文件语义：0.0.0.0:8080（共享基座为 127.0.0.1:0，显式保留）
                server: ServerConfig {
                    host: "0.0.0.0".into(),
                    port: 8080,
                },
                ..common::test_config(vendors)
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
                ..Default::default()
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
                GateError::VendorUnavailable { .. } => {}
                other => panic!("expected VendorUnavailable, got {:?}", other),
            }
        }
    }

    // ======== TimeWindow 时段门控（get_window_info 判定委托） ========

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

    // ======== 出站 HTTP 超时治理 ========
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
            Err(GateError::Timeout { vendor }) => assert_eq!(vendor, "v-ot"),
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
            matches!(items.last(), Some(Err(GateError::Timeout { vendor })) if vendor == "v-idle-t"),
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
            Err(GateError::Timeout { vendor }) => assert_eq!(vendor, "v-ttft"),
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
        let (tx, _rx) =
            tokio::sync::oneshot::channel::<Result<UnifiedResponse, nexusflow::error::GateError>>();
        PendingRequest {
            request: sample_request(),
            response_tx: tx,
            enqueued_at: std::time::Instant::now(),
            // 出队时刻由 VendorQueue::recv 盖戳，构造方恒 None
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

    // QueueManager 死件已删除（全库无 insert 方，queue_factor 恒 1.0
    // 的根因）；原 test_queue_manager_returns_none_for_missing_vendor 随之退役。
}

mod advanced_rate_limiter {
    //! AdvancedRateLimiter 新 API 测试。
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
        // CAS 全有全无 —— 剩余 10 请求扣 15 → 整体拒绝、分文未扣
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

    /// 用例①（tests）：Token 窗口耗尽 → check false → 假时钟推进过
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

    /// 用例②（tests）：100 并发任务对 max=50 窗口各扣 1 → 成功数恰 50。
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

// ======== TimeWindow 时段门控跨午夜判定（纯函数） ========

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

// ============ Token/Interval 滑动窗口（ShardedSlidingWindowLimiter）============

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
        // 评审修复：空窗（零消费）以窗口全长上报——
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

// ============ token 加权并发票据 ============

#[cfg(test)]
mod token_weighted_tickets {
    //! 估算双形态的 clamp 边界与量级一致性、acquire_many 语义（60×2 挤出
    //! 第三张）、全链路挤压（32k 长会话独占容量）。

    use crate::common;
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
        assert!((1.0..=1000.0).contains(&a) && (1.0..=1000.0).contains(&b));
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
        common::ensure_test_api_key();
        let config = nexusflow::config::Config {
            rate_limit: Default::default(),
            server: nexusflow::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: nexusflow::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![nexusflow::config::VendorConfig {
                timeout_ms: 2_000,
                stream_ttft_ms: 2_000,
                stream_idle_ms: 2_000,
                id: "v1".to_string(),
                name: "V1".into(),
                type_: "openai".into(),
                api_key_env: "NEXUSFLOW_TEST_KEY".into(),
                api_key_envs: Vec::new(),
                extra_headers: Default::default(),
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
            resilience: nexusflow::config::ResilienceSection::default(),
            docs: Default::default(),
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
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, RateLimitConfig, ServerConfig};
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    let mut v = common::base_vendor("v-slide", "");
    // 本文件语义差异，显式覆盖：展示名 V、空模型表、空窗 sliding 小额度窗口、
    // rps=100（基座为单 gpt-4 模型、Token 1_000_000 非滑动、rps=1000）
    v.name = "V".into();
    v.models = vec![];
    v.rate_limit = RateLimitConfig::Token {
        max_tokens: 10,
        window_hours: Some(1),
        sliding: true,
    };
    v.concurrency.requests_per_second = 100;
    let config = Config {
        // 本文件语义：0.0.0.0:8080（共享基座为 127.0.0.1:0，显式保留）
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        ..common::test_config(vec![v])
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

// ============ manager.rs 执行路径补全（错误分支 / 流分帧边界 / 预算与热更新） ============
//
// 每个（组）用例对应一条此前未覆盖的执行路径，断言可观测行为而非"调用不报错"：
// worker 票据加权与配置热移除、token 记账 CAS 拒绝、未知 vendor type 三条路径、
// 默认 api_base 解析与 api key 缺失、出站连接拒绝、Retry-After 解析、重试时间
// 预算放弃、body 读取超时、流式限流/错误帧/IO 中断/EOF 冲刷/注释与 role-only 帧跳过、
// TimeWindow 窗口内 window_end 计算。

mod manager_paths {
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, RateLimitConfig, ServerConfig, VendorConfig};
    use nexusflow::error::GateError;
    use nexusflow::protocol::{Message, MessageContent, Role, StreamChunk, UnifiedRequest};
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    use std::time::Duration;

    /// tracing 宏在进程无订阅者时整体短路：warn! 的事件字段表达式（含 i18n
    /// 文案构造）不会求值，对应分支在行覆盖率上恒为 0。测试进程内安装一次性
    /// 全局订阅者（no-op，事件仅吞掉，零输出噪声），使这些分支真实可测。
    /// 不用 tracing-subscriber：它在 Cargo.toml 中是 optional 且无 feature
    /// 引用，对集成测试不可见。
    fn init_tracing() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            struct SinkSubscriber;
            impl tracing::Subscriber for SinkSubscriber {
                fn enabled(&self, _meta: &tracing::Metadata<'_>) -> bool {
                    true
                }
                fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                    tracing::span::Id::from_u64(1)
                }
                fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
                fn record_follows_from(
                    &self,
                    _span: &tracing::span::Id,
                    _follows: &tracing::span::Id,
                ) {
                }
                fn event(&self, _event: &tracing::Event<'_>) {}
                fn enter(&self, _span: &tracing::span::Id) {}
                fn exit(&self, _span: &tracing::span::Id) {}
            }
            let _ = tracing::subscriber::set_global_default(SinkSubscriber);
        });
    }

    /// 参数化字段（type_/rate_limit）按调用点传入，其余走共享基座；
    /// rps=100 让 TokenBucket 立即放行（本文件语义，显式覆盖基座 1000）
    fn make_vendor(
        id: &str,
        type_: &str,
        api_base: String,
        rate_limit: RateLimitConfig,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.type_ = type_.into();
        v.rate_limit = rate_limit;
        v.concurrency.requests_per_second = 100;
        v
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
            // 本文件语义：0.0.0.0:8080（共享基座为 127.0.0.1:0，显式保留）
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vendors)
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
            ..Default::default()
        }
    }

    const OPENAI_OK_BODY: &str = r#"{"id":"chatcmpl-1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#;

    // ============ 实例生命周期与观测访问器 ============

    /// initialize 二次调用命中 get_or_create_instance 的已存在分支：
    /// 不重建实例、不重复 spawn worker（get_all_status 仍只有一份）。
    #[tokio::test]
    async fn initialize_twice_reuses_existing_instance() {
        let v = make_vendor("v-init2", "openai", String::new(), token_limit(1000));
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("first init");
        manager.initialize().await.expect("second init");
        assert_eq!(
            manager.get_all_status().len(),
            1,
            "二次 initialize 不得重复建实例"
        );
    }

    /// get_circuit_state 对未初始化 vendor 返回 None（CB map 未注册该 id）。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn get_circuit_state_unknown_vendor_returns_none() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        assert!(manager.get_circuit_state("ghost").await.is_none());
    }

    /// 无实例（未 initialize）时 fleet 均值必须返回 0.0 退化值，
    /// 不得除零或 panic（消费方 relative 公式依赖该兜底）。
    #[tokio::test]
    async fn fleet_mean_in_flight_zero_before_initialize() {
        let cm = Arc::new(ConfigManager::new(make_config(vec![])));
        let manager = VendorManager::new(cm);
        assert_eq!(manager.fleet_mean_in_flight(), 0.0);
    }

    // ============ worker 转换路径（queue → worker → HTTP）============

    /// token_weighted 开启时 worker 按 estimate_cost_unified 计费取票
    ///（acquire_weighted 分支），请求成功且 permit 随完成释放。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_ticket_weighted_path_acquires_and_releases() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_OK_BODY)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-weighted-worker",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.concurrency.token_weighted = true;
        v.concurrency.max_concurrent_requests = Some(8);
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-weighted-worker", make_unified_request()),
        )
        .await
        .expect("no timeout")
        .expect("weighted worker request should succeed");
        assert_eq!(result.id, "chatcmpl-1");

        // permit 已随完成释放：在途归零，容量 = 显式并发上限
        assert_eq!(
            manager.fleet_concurrency(),
            (0, 8),
            "worker 完成后加权票据必须释放"
        );
        mock.assert_async().await;
    }

    /// 热更新移除 vendor 后，已初始化实例的在途请求必须以 VendorUnavailable
    /// 拒绝（worker 热读 fresh_config 失败分支），错误文案携带 vendor id。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_rejects_when_vendor_removed_from_hot_config() {
        let dir = std::env::temp_dir().join("nexusflow-vendor-removed-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let v = make_vendor(
            "v-removed",
            "openai",
            "http://127.0.0.1:1/v1".into(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        cm.set_path(path.clone());
        let manager = Arc::new(VendorManager::new(cm.clone()));
        manager.initialize().await.expect("init");

        // 热更新：vendor 从配置中移除（落盘 + 内存切换）
        cm.update_config(make_config(vec![])).expect("hot update");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-removed", make_unified_request()),
        )
        .await
        .expect("no timeout");
        match result {
            Ok(_) => panic!("removed vendor must be rejected"),
            Err(GateError::VendorUnavailable { vendor }) => {
                assert!(
                    vendor.contains("v-removed"),
                    "错误文案必须携带 vendor id: {}",
                    vendor
                );
            }
            Err(other) => panic!("expected VendorUnavailable, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// worker 成功分支的 token 记账 CAS 拒绝：响应 usage 超过窗口剩余时
    /// consume_tokens 整笔拒绝，used_tokens 保持不变（不超卖）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_success_token_underflow_skips_accounting() {
        init_tracing();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":20,"total_tokens":50}}"#,
            )
            .create_async()
            .await;

        // 窗口 10 < 响应 usage 50 → 记账必然被 CAS 拒绝（quota-underflow 分支）
        let v = make_vendor(
            "v-underflow-worker",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(10),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-underflow-worker", make_unified_request()),
        )
        .await
        .expect("no timeout")
        .expect("response itself must succeed");
        assert_eq!(result.usage.total_tokens, 50);

        // CAS 全有全无：拒绝记账后窗口用量保持 0
        let info = manager
            .get_window_info("v-underflow-worker")
            .await
            .expect("window info");
        assert_eq!(info.used_tokens, 0, "被拒绝的记账不得部分扣减（CAS 语义）");
        mock.assert_async().await;
    }

    // ============ vendor type 与 api_base/api_key 解析 ============

    /// 未知 vendor type 走转换路径时在 converter 分派处失败（Config 错误），
    /// 不得触达任何 HTTP。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn conversion_unknown_vendor_type_fails_fast() {
        let v = make_vendor(
            "v-bad-type",
            "crystal-ball",
            "http://127.0.0.1:1/v1".into(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-bad-type", make_unified_request()),
        )
        .await
        .expect("no timeout");
        match result {
            Err(GateError::Config { .. }) => {}
            other => panic!("expected Config error, got {:?}", other.map(|_| ())),
        }
    }

    /// 透传路径 + 空 api_base + 未知 type：URL 构建期失败（无法推导默认 base）。
    #[tokio::test]
    async fn passthrough_unknown_vendor_type_empty_base_fails_at_url_build() {
        let v = make_vendor(
            "v-bad-type-base",
            "crystal-ball",
            String::new(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = manager
            .execute_passthrough_request("v-bad-type-base", serde_json::json!({}))
            .await;
        match result {
            Err(GateError::Config { .. }) => {}
            other => panic!("expected Config error, got {:?}", other.map(|_| ())),
        }
    }

    /// 流式路径 + 未知 type + 合法 api_base：URL 构建成功后在 converter
    /// 分派处失败（与转换/透传路径不同的第三处分派点）。
    #[tokio::test]
    async fn stream_unknown_vendor_type_fails_at_converter() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .create_async()
            .await;
        let v = make_vendor(
            "v-bad-type-stream",
            "crystal-ball",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = manager
            .execute_stream_request("v-bad-type-stream", make_unified_request())
            .await;
        match result {
            Ok(_) => panic!("unknown type must fail"),
            Err(GateError::Config { .. }) => {}
            Err(other) => panic!("expected Config error, got {:?}", other),
        }
    }

    /// 空 api_base 的默认域名解析（anthropic/openai 两个分支），随后在
    /// api key 环境变量缺失处失败——全程无网络请求。
    #[tokio::test]
    async fn default_api_base_resolution_then_missing_api_key_fails() {
        const MISSING_ANTHROPIC: &str = "NEXUSFLOW_UNSET_KEY_ANTHROPIC_Q7";
        const MISSING_OPENAI: &str = "NEXUSFLOW_UNSET_KEY_OPENAI_Q7";
        std::env::remove_var(MISSING_ANTHROPIC);
        std::env::remove_var(MISSING_OPENAI);

        let anthropic = {
            let mut v = make_vendor(
                "v-no-base-anthropic",
                "anthropic",
                String::new(),
                token_limit(1000),
            );
            v.api_key_env = MISSING_ANTHROPIC.into();
            v
        };
        let openai = {
            let mut v = make_vendor(
                "v-no-base-openai",
                "openai",
                String::new(),
                token_limit(1000),
            );
            v.api_key_env = MISSING_OPENAI.into();
            v
        };
        let cm = Arc::new(ConfigManager::new(make_config(vec![anthropic, openai])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        for (id, env) in [
            ("v-no-base-anthropic", MISSING_ANTHROPIC),
            ("v-no-base-openai", MISSING_OPENAI),
        ] {
            let result = manager
                .execute_passthrough_request(id, serde_json::json!({}))
                .await;
            match result {
                Err(GateError::Config { message }) => {
                    assert!(
                        message.contains(env),
                        "错误文案必须携带缺失的环境变量名 {}: {}",
                        env,
                        message
                    );
                }
                other => panic!(
                    "expected Config error for {}, got {:?}",
                    id,
                    other.map(|_| ())
                ),
            }
        }
    }

    /// 透传出站连接拒绝（无监听端口）映射 VendorUnavailable——
    /// outbound_send_error 的非超时分支。
    #[tokio::test]
    async fn passthrough_connect_refused_is_vendor_unavailable() {
        let v = make_vendor(
            "v-conn-refused",
            "openai",
            "http://127.0.0.1:1/v1".into(),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = manager
            .execute_passthrough_request("v-conn-refused", serde_json::json!({}))
            .await;
        match result {
            Err(GateError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "v-conn-refused");
            }
            other => panic!("expected VendorUnavailable, got {:?}", other.map(|_| ())),
        }
    }

    // ============ 重试循环（Retry-After / 时间预算 / body 超时）============

    /// 上游 429 携带 Retry-After 头：秒数形式必须被解析（后续退避以其为
    /// base）；max_retries=0 下不重试，单次命中后以 429 语义失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn conversion_429_retry_after_header_is_parsed() {
        init_tracing();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(429)
            .with_header("retry-after", "7")
            .with_body("slow down")
            .create_async()
            .await;

        let v = make_vendor(
            "v-retry-after",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let mut config = make_config(vec![v]);
        config.retry.max_retries = 0;
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-retry-after", make_unified_request()),
        )
        .await
        .expect("no timeout");
        match result {
            Err(GateError::VendorUnavailable { vendor }) => {
                assert!(vendor.contains("429"), "错误须携带状态码: {}", vendor);
            }
            other => panic!("expected VendorUnavailable, got {:?}", other.map(|_| ())),
        }
        mock.assert_async().await;
    }

    /// 网络错误的 RetryableShort 重试受 retry_budget_secs 约束：
    /// 「已耗时 + 下次等待」超预算时立即放弃（不再 sleep），返回最后错误。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_budget_aborts_network_retry_loop() {
        let v = make_vendor(
            "v-budget-net",
            "openai",
            "http://127.0.0.1:1/v1".into(),
            token_limit(1000),
        );
        let mut config = make_config(vec![v]);
        config.retry.retry_budget_secs = 1;
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-budget-net", make_unified_request()),
        )
        .await
        .expect("no timeout");
        match result {
            Err(GateError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "v-budget-net");
            }
            other => panic!("expected VendorUnavailable, got {:?}", other.map(|_| ())),
        }
        // 第二次尝试的等待（1000ms）被预算截断：总耗时应明显小于
        // 500ms + 1000ms 的完整退避序列
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "预算超限必须提前放弃，实际耗时 {:?}",
            start.elapsed()
        );
    }

    /// 5xx 重试同样受 retry_budget_secs 约束：首次重试等待前即判超限
    ///（spread 展宽后 delay ≥ 1s ≥ 预算），保留 5xx 语义失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retry_budget_aborts_5xx_retry_loop() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .expect_at_most(2)
            .create_async()
            .await;

        let v = make_vendor(
            "v-budget-5xx",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let mut config = make_config(vec![v]);
        config.retry.retry_budget_secs = 1;
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-budget-5xx", make_unified_request()),
        )
        .await
        .expect("no timeout");
        match result {
            Err(GateError::VendorUnavailable { vendor }) => {
                assert!(
                    vendor.contains("500"),
                    "预算放弃必须保留最后错误的 5xx 语义: {}",
                    vendor
                );
            }
            other => panic!("expected VendorUnavailable, got {:?}", other.map(|_| ())),
        }
        mock.assert_async().await;
    }

    /// 转换路径（worker）body 读取阶段超时映射 Timeout——与透传路径
    ///（既有用例）同语义，覆盖重试内层 json() 的 is_timeout 分支。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn conversion_body_read_timeout_maps_to_timeout() {
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
            "v-body-timeout",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.timeout_ms = 200;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            manager.execute_request("v-body-timeout", make_unified_request()),
        )
        .await
        .expect("test-level watchdog must not fire");
        match result {
            Err(GateError::Timeout { vendor }) => assert_eq!(vendor, "v-body-timeout"),
            other => panic!("expected Timeout, got {:?}", other.map(|_| ())),
        }
    }

    /// timeout_ms=0（不限时）时 execute_request 外层只加排队预算——
    /// 成功路径锁定「零值不退化为立即超时」语义。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_request_zero_timeout_uses_queue_budget() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OPENAI_OK_BODY)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-zero-timeout",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.timeout_ms = 0;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.execute_request("v-zero-timeout", make_unified_request()),
        )
        .await
        .expect("no timeout")
        .expect("timeout_ms=0 must not fail the request");
        assert_eq!(result.id, "chatcmpl-1");
        mock.assert_async().await;
    }

    // ============ 流式：限流 / 派发错误 / 分帧边界 ============

    /// Token 窗口耗尽后转换流式在 advanced limiter peek 处被拒
    ///（RateLimited，无 HTTP 命中）。
    #[tokio::test]
    async fn stream_rate_limited_when_token_window_exhausted() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":0,"total_tokens":5}}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let v = make_vendor(
            "v-stream-throttle",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(5),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        // 透传一次消耗满 5/5 窗口
        let r = manager
            .execute_passthrough_request("v-stream-throttle", serde_json::json!({}))
            .await;
        assert!(r.is_ok(), "priming request should succeed: {:?}", r.err());

        let result = manager
            .execute_stream_request("v-stream-throttle", make_unified_request())
            .await;
        match result {
            Ok(_) => panic!("exhausted window must reject streams"),
            Err(GateError::RateLimited { vendor }) => {
                assert_eq!(vendor, "v-stream-throttle");
            }
            Err(other) => panic!("expected RateLimited, got {:?}", other),
        }
        // 第二次请求未触达上游
        mock.assert_async().await;
    }

    /// token_weighted 流式按预估 token 计费取票（acquire_weighted 分支），
    /// 流正常建立并产出 chunk。
    #[tokio::test]
    async fn stream_ticket_weighted_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
                );
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-weighted-stream",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.concurrency.token_weighted = true;
        v.concurrency.max_concurrent_requests = Some(8);
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_stream_request("v-weighted-stream", make_unified_request())
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut chunks = 0;
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            assert!(item.is_ok(), "chunk should be ok: {:?}", item.err());
            chunks += 1;
        }
        assert_eq!(chunks, 1, "应恰好产出一个内容 chunk");
        mock.assert_async().await;
    }

    /// anthropic 协议的流式 converter 分派 + 反向解析（content_block_delta
    /// → IR Content，message_stop 生命周期帧跳过）。
    #[tokio::test]
    async fn anthropic_stream_success_uses_anthropic_converter() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"bon\"}}\n\n",
                );
                let _ = w.write_all(b"data: {\"type\":\"message_stop\"}\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let v = make_vendor(
            "v-anthropic-stream",
            "anthropic",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_stream_request("v-anthropic-stream", make_unified_request())
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut texts = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            match item.expect("chunk ok") {
                StreamChunk::Content {
                    delta: MessageContent::Text(t),
                    ..
                } => texts.push(t),
                other => panic!("unexpected chunk: {:?}", other),
            }
        }
        assert_eq!(texts, vec!["bon".to_string()], "生命周期帧必须被跳过");
        mock.assert_async().await;
    }

    /// stream_ttft_ms=0（TTFT 不限时）分支：出站 send 失败（连接拒绝）
    /// 映射 VendorUnavailable——不经 outbound_send_error 的直连映射臂。
    #[tokio::test]
    async fn stream_send_error_without_ttft_budget_maps_to_vendor_unavailable() {
        let v = make_vendor(
            "v-stream-dead",
            "openai",
            "http://127.0.0.1:1/v1".into(),
            token_limit(1000),
        );
        let mut config = make_config(vec![v]);
        config.vendors[0].stream_ttft_ms = 0;
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let result = manager
            .execute_stream_request("v-stream-dead", make_unified_request())
            .await;
        match result {
            Ok(_) => panic!("dead endpoint must fail"),
            Err(GateError::VendorUnavailable { vendor }) => {
                assert_eq!(vendor, "v-stream-dead");
            }
            Err(other) => panic!("expected VendorUnavailable, got {:?}", other),
        }
    }

    /// 上游 429 的流式派发：状态校验映射 RateLimited（不再以 200 正文
    /// 伪装 SSE 流），且失败计入 metrics。
    #[tokio::test]
    async fn stream_429_maps_to_rate_limited() {
        init_tracing();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(429)
            .with_body("rate limited")
            .create_async()
            .await;

        let v = make_vendor(
            "v-stream-429",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("init");

        let result = manager
            .execute_stream_request("v-stream-429", make_unified_request())
            .await;
        match result {
            Ok(_) => panic!("429 must fail the stream"),
            Err(GateError::RateLimited { vendor }) => {
                assert_eq!(vendor, "v-stream-429");
            }
            Err(other) => panic!("expected RateLimited, got {:?}", other),
        }

        let vendor_metrics = manager
            .metrics()
            .snapshot()
            .vendors
            .into_iter()
            .find(|v| v.vendor == "v-stream-429")
            .expect("failure must be recorded");
        assert_eq!(vendor_metrics.failures, 1, "流式失败必须记账 metrics");
        mock.assert_async().await;
    }

    /// 分帧边界组合：注释帧与 role-only 帧跳过、内容帧映射 IR、
    /// 上游省略末尾定界空行时 EOF 冲刷仍产出尾帧；
    /// idle=0 走无超时直读分支，流以 None 正常收束（permit 释放）。
    #[tokio::test]
    async fn conversion_stream_zero_idle_parses_comment_role_and_eof_flush() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                // 注释帧（data 缺失 → 跳过）
                let _ = w.write_all(b": keepalive\n\n");
                // role-only 首帧（无内容无终止 → 不产出空事件）
                let _ = w.write_all(
                    b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                );
                // 内容帧
                let _ = w.write_all(
                    b"data: {\"id\":\"2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                );
                // 尾帧故意不带定界空行 → EOF 冲刷路径产出
                let _ = w.write_all(
                    b"data: {\"id\":\"3\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"}}]}",
                );
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-stream-frames",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.stream_ttft_ms = 0;
        v.stream_idle_ms = 0;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_stream_request("v-stream-frames", make_unified_request())
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut texts = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            match item.expect("chunk ok") {
                StreamChunk::Content {
                    delta: MessageContent::Text(t),
                    ..
                } => texts.push(t),
                other => panic!("unexpected chunk: {:?}", other),
            }
        }
        assert_eq!(
            texts,
            vec!["hi".to_string(), "!".to_string()],
            "注释/role-only 帧必须跳过，EOF 冲刷必须产出尾帧"
        );
        mock.assert_async().await;
    }

    /// 上游中流 IO 错误（声明 content-length 提前断开）：字节流错误统一为
    /// GateError 后，分帧层以 VendorUnavailable（stream-interrupted
    /// 语义）提前终止流。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn conversion_stream_io_error_wraps_as_vendor_unavailable() {
        init_tracing();
        // 手写 TCP 桩：声明 200 字节只发 17 字节即断开 → hyper 产生
        // 「连接提前关闭」IO 错误（mockito 无此原语）
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                // 先读掉入站请求再写响应：带着未读数据关连接会触发 RST
                //（客户端在响应头前就见到连接重置），读净后关连接才是 FIN
                let mut buf = vec![0u8; 16_384];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 200\r\n\r\ndata: {\"partial\":",
                    )
                    .await;
                let _ = sock.flush().await;
                // drop 即关闭：声明 200 字节只发 17 字节，客户端在 body
                // 读完前见到 EOF → hyper 产生 IO 错误
            }
        });

        let v = make_vendor(
            "v-stream-ioerr",
            "openai",
            format!("http://{addr}/v1"),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_stream_request("v-stream-ioerr", make_unified_request())
            .await
            .expect("headers arrived, stream established");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut last = None;
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            last = Some(item);
        }
        match last {
            Some(Err(GateError::VendorUnavailable { vendor })) => {
                assert!(
                    vendor.contains("v-stream-ioerr"),
                    "中断错误必须携带 vendor 与 detail: {}",
                    vendor
                );
            }
            other => panic!("expected Err(VendorUnavailable), got {:?}", other),
        }
    }

    /// 上游中流错误帧（event: error）→ 语义化 Err 提前终止，
    /// detail 取自帧 data，不得被当作普通 chunk 吞掉。
    #[tokio::test]
    async fn conversion_stream_error_frame_terminates_as_vendor_unavailable() {
        init_tracing();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(
                    b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
                );
                let _ = w.write_all(b"event: error\ndata: upstream exploded\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let v = make_vendor(
            "v-stream-errframe",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_stream_request("v-stream-errframe", make_unified_request())
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut items = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            items.push(item);
        }
        assert_eq!(items.len(), 2, "内容帧 + 错误帧各产出一条: {:?}", items);
        assert!(items[0].is_ok(), "首帧应为内容 chunk");
        match &items[1] {
            Err(GateError::VendorUnavailable { vendor }) => {
                assert!(
                    vendor.contains("upstream exploded"),
                    "错误 detail 必须携带上游消息: {}",
                    vendor
                );
            }
            other => panic!("expected VendorUnavailable, got {:?}", other),
        }
        mock.assert_async().await;
    }

    // ============ 透传流式：idle 超时 / 零 idle / IO 错误 / 限流 / 票据加权 ============

    /// 透传流式 idle 超时：chunk 间隔超过 stream_idle_ms → 字节流以
    /// Err(Timeout) 产出（与转换流式同口径）。
    #[tokio::test]
    async fn passthrough_stream_idle_timeout_yields_timeout_item() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                let _ = w.write_all(b"data: {\"first\":true}\n\n");
                let _ = w.flush();
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let _ = w.write_all(b"data: {\"late\":true}\n\n");
                Ok::<(), std::io::Error>(())
            })
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-pta-idle",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.stream_idle_ms = 300;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_passthrough_stream_request("v-pta-idle", serde_json::json!({}))
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut items = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            items.push(item);
            if items.len() > 4 {
                break;
            }
        }
        assert!(
            items.iter().any(|i| i.is_ok()),
            "首个 chunk 应在停顿前到达: {:?}",
            items
        );
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Err(GateError::Timeout { vendor }) if vendor == "v-pta-idle")),
            "idle 超时必须以 Err(Timeout) 产出: {:?}",
            items
        );
    }

    /// 透传流式 idle=0（不限时）：直读分支读到 EOF，内容完整收束。
    #[tokio::test]
    async fn passthrough_stream_zero_idle_reads_to_eof() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"pta-zero"}"#)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-pta-zero",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.stream_idle_ms = 0;
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_passthrough_stream_request("v-pta-zero", serde_json::json!({}))
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut collected = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            collected.extend_from_slice(&item.expect("chunk ok"));
        }
        let text = String::from_utf8(collected).expect("utf8");
        assert!(
            text.contains("pta-zero"),
            "idle=0 必须完整读到 EOF: {}",
            text
        );
        mock.assert_async().await;
    }

    /// 透传流式中流 IO 错误：以 Err(VendorUnavailable) 产出（与转换流式
    /// 同一 outbound_send_error 统一口径）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn passthrough_stream_io_error_yields_vendor_unavailable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                // 同上：读净请求再写响应，避免未读数据触发的 RST
                let mut buf = vec![0u8; 16_384];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 200\r\n\r\ndata: {\"partial\":",
                    )
                    .await;
                let _ = sock.flush().await;
            }
        });

        let v = make_vendor(
            "v-pta-ioerr",
            "openai",
            format!("http://{addr}/v1"),
            token_limit(1000),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_passthrough_stream_request("v-pta-ioerr", serde_json::json!({}))
            .await
            .expect("stream established");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut last = None;
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            last = Some(item);
        }
        match last {
            Some(Err(GateError::VendorUnavailable { vendor })) => {
                assert_eq!(vendor, "v-pta-ioerr");
            }
            other => panic!("expected Err(VendorUnavailable), got {:?}", other),
        }
    }

    /// Token 窗口耗尽后透传流式在 advanced limiter peek 处被拒。
    #[tokio::test]
    async fn passthrough_stream_rate_limited_when_token_window_exhausted() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":0,"total_tokens":5}}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let v = make_vendor(
            "v-pta-throttle",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(5),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let r = manager
            .execute_passthrough_request("v-pta-throttle", serde_json::json!({}))
            .await;
        assert!(r.is_ok(), "priming request should succeed: {:?}", r.err());

        let result = manager
            .execute_passthrough_stream_request("v-pta-throttle", serde_json::json!({}))
            .await;
        match result {
            Ok(_) => panic!("exhausted window must reject streams"),
            Err(GateError::RateLimited { vendor }) => {
                assert_eq!(vendor, "v-pta-throttle");
            }
            Err(other) => panic!("expected RateLimited, got {:?}", other),
        }
        mock.assert_async().await;
    }

    /// token_weighted 透传流式按 estimate_cost_json 计费取票，
    /// 流正常建立并读至 EOF。
    #[tokio::test]
    async fn passthrough_stream_ticket_weighted_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"pta-weighted"}"#)
            .create_async()
            .await;

        let mut v = make_vendor(
            "v-pta-weighted",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(1000),
        );
        v.concurrency.token_weighted = true;
        v.concurrency.max_concurrent_requests = Some(8);
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let stream = manager
            .execute_passthrough_stream_request("v-pta-weighted", serde_json::json!({}))
            .await
            .expect("stream should start");
        use futures_util::StreamExt;
        let mut stream = stream;
        let mut collected = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("no hang")
        {
            collected.extend_from_slice(&item.expect("chunk ok"));
        }
        assert!(
            String::from_utf8(collected)
                .unwrap()
                .contains("pta-weighted"),
            "加权票据不得影响透传流内容"
        );
        mock.assert_async().await;
    }

    // ============ 透传 token 记账 CAS 拒绝 / TimeWindow 窗口内 ============

    /// 透传成功响应的 usage 超过窗口剩余：consume_tokens CAS 整笔拒绝，
    /// 窗口用量保持 0（quota-underflow 分支不产生误导性扣减）。
    #[tokio::test]
    async fn passthrough_token_underflow_skips_accounting() {
        init_tracing();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":30,"completion_tokens":20,"total_tokens":50}}"#,
            )
            .create_async()
            .await;

        let v = make_vendor(
            "v-underflow-pta",
            "openai",
            format!("{}/v1", server.url()),
            token_limit(10),
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let r = manager
            .execute_passthrough_request("v-underflow-pta", serde_json::json!({}))
            .await;
        assert!(r.is_ok(), "response itself must succeed: {:?}", r.err());

        let info = manager
            .get_window_info("v-underflow-pta")
            .await
            .expect("window info");
        assert_eq!(info.used_tokens, 0, "CAS 拒绝后窗口用量不得变化");
        mock.assert_async().await;
    }

    /// TimeWindow 窗口内 get_window_info 返回精确 window_end（当日 end
    /// 时刻内插分支）。窗口取 now±2min，同日/跨午夜构造下均恒在窗口内。
    #[tokio::test]
    async fn get_window_info_time_window_inside_reports_window_end() {
        use chrono::{Duration as ChronoDuration, Local};

        let now = Local::now().time();
        let start = (now - ChronoDuration::minutes(2))
            .format("%H:%M")
            .to_string();
        let end = (now + ChronoDuration::minutes(2))
            .format("%H:%M")
            .to_string();
        let v = make_vendor(
            "v-timewindow-inside",
            "openai",
            String::new(),
            RateLimitConfig::TimeWindow { start, end },
        );
        let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let info = manager
            .get_window_info("v-timewindow-inside")
            .await
            .expect("window info");
        assert_eq!(info.max_tokens, None, "TimeWindow 无 token 上限");
        assert_eq!(info.used_tokens, 0);
        assert!(
            info.window_end.is_some(),
            "窗口内必须内插出 end 时刻（window_end）"
        );
        // 不断言 is_ending_soon：跨午夜构造下 end_datetime 退化为 now 是
        // 既有已知语义（manager.rs 注释如实声明），断言会随运行时刻翻flaky
    }
}
