// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 集成测试 — CircuitBreaker（limiteron::circuit）接入 VendorManager。
//!
//! recId=2 后 `force_circuit_open` 已删除（limiteron 公开面无 force API），
//! 测试改为 mockito 预置失败响应自然驱动熔断（failure_threshold=5）：
//! - 预置 5×500 + 测试配置 [retry] max_retries=0（recId=7）——500 无内部
//!   重试，每个请求恰好 1 次 HTTP，5 个请求即把失败计数推到阈值；
//! - 熔断/限流的路由判定已全面实时化于 get_status（评审 blocker 修复：
//!   持久回写会造成「路由排除→无探针→永不恢复」死锁），worker 回写仅剩
//!   观测计数（inc_error / 成功清零）。
//!
//! 集成属性：
//! 1. 熔断后 route() 不再选中该 vendor（路由过滤接线断言）
//! 2. VendorState 的 Unavailable 状态直接参与路由过滤（update_status 用例）

#![cfg(all(feature = "resilience", feature = "router"))]

use limiteron::error::CircuitState;
use nexusflow::config::{
    ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
    RateLimitConfig, RetrySection, ServerConfig, VendorConfig,
};
use nexusflow::error::NexusFlowError;
use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
use nexusflow::router::Router;
use nexusflow::vendor::VendorManager;
use nexusflow::vendor::VendorStatus;
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
        // recId=7：max_retries=0 —— 500 直接冒泡（无内部重试），
        // 每个请求恰好 1 次 HTTP，5 个请求即把 CB 失败计数推到阈值 5
        retry: RetrySection {
            max_retries: 0,
            ..Default::default()
        },
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        grpc: None,
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

const OPENAI_SUCCESS_BODY: &str = r#"{
    "id": "chatcmpl-x",
    "object": "chat.completion",
    "model": "gpt-4",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}, "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
}"#;

/// mockito 预置 5 次 Fatal 失败（4xx，无内部重试）自然驱动熔断；
/// 随后 route() 不再选中该 vendor（路由过滤接线断言），
/// 而 vendor_b 的 HTTP 路径不受影响（per-vendor 隔离）。
#[tokio::test]
async fn circuit_opens_after_five_failures_and_route_excludes_vendor() {
    let mut server_a = mockito::Server::new_async().await;
    // Fatal 4xx：execute_http_with_retry_inner 不做状态码重试，每请求至多
    // 1 次命中——expect_at_most(5) 锁上界（≥6 即存在隐藏重试，失败）。
    // 不用精确 expect(5)：全量并行下 localhost 瞬态连接失败会把失败计入
    // 熔断（VendorUnavailable）但不产生 HTTP 命中（实测：Open 断言通过而
    // mockito 收 4/5），「恰 5 次失败」语义由下方 CB 状态断言承载。
    let mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(400)
        .with_body("bad request")
        .expect_at_most(5)
        .create_async()
        .await;

    let mut server_b = mockito::Server::new_async().await;
    let mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(OPENAI_SUCCESS_BODY)
        .expect(1)
        .create_async()
        .await;

    let v_a = make_vendor("vendor_a", server_a.url());
    let v_b = make_vendor("vendor_b", server_b.url());
    let cm = Arc::new(ConfigManager::new(make_config(vec![v_a, v_b])));
    let manager = Arc::new(VendorManager::new(cm.clone()));
    manager.initialize().await.expect("init should succeed");
    let router = Router::new(cm, manager.clone());

    // 5 个请求自然驱动熔断（failure_threshold=5）
    for _ in 0..5 {
        let result = manager
            .execute_request("vendor_a", make_unified_request())
            .await;
        assert!(result.is_err(), "failing vendor should return Err");
    }

    // worker 完成分支先回写 VendorState 再 send → 此刻断言即有效
    assert_eq!(
        manager.get_circuit_state("vendor_a").await,
        Some(CircuitState::Open),
        "5 次失败后 vendor_a 熔断"
    );
    assert_eq!(
        manager.get_circuit_state("vendor_b").await,
        Some(CircuitState::Closed),
        "per-vendor 隔离：vendor_b 不受影响"
    );
    assert_eq!(
        manager.get_status("vendor_a").await,
        VendorStatus::Unavailable,
        "worker 回写让同步 get_status 感知熔断"
    );

    // 路由过滤接线：vendor_a 已 Unavailable → route() 只会选中 vendor_b
    for _ in 0..50 {
        let decision = router.route("gpt-4").await.expect("vendor_b should route");
        assert_eq!(
            decision.vendor.id, "vendor_b",
            "熔断 vendor 必须被路由过滤排除"
        );
    }

    // vendor_b 的真实 HTTP 路径不受 vendor_a 熔断影响
    let result = manager
        .execute_request("vendor_b", make_unified_request())
        .await;
    assert!(
        result.is_ok(),
        "vendor_b request should succeed: {:?}",
        result.err()
    );

    mock_a.assert_async().await;
    mock_b.assert_async().await;
}

/// CB Open 快速拒绝（无 HTTP 调用）：熔断后第 6 个请求毫秒级失败，
/// 错误为 `VendorUnavailable{... (circuit open)}`。
#[tokio::test]
async fn open_circuit_rejects_requests_fast_without_http() {
    let mut server_a = mockito::Server::new_async().await;
    let mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("upstream error")
        .expect_at_most(5) // 上界锁：无隐藏重试；恰 5 次失败由 CB Open 断言承载
        .create_async()
        .await;

    let v_a = make_vendor("vendor_a", server_a.url());
    let cm = Arc::new(ConfigManager::new(make_config(vec![v_a])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init should succeed");

    for _ in 0..5 {
        let _ = manager
            .execute_request("vendor_a", make_unified_request())
            .await;
    }
    assert_eq!(
        manager.get_circuit_state("vendor_a").await,
        Some(CircuitState::Open)
    );

    let start = std::time::Instant::now();
    let result = manager
        .execute_request("vendor_a", make_unified_request())
        .await;
    let elapsed = start.elapsed();

    let err = result.expect_err("open circuit should reject");
    match &err {
        NexusFlowError::VendorUnavailable { vendor } => {
            assert!(
                vendor.contains("circuit open"),
                "error should mention 'circuit open', got: {}",
                vendor
            );
        }
        other => panic!("expected VendorUnavailable, got {:?}", other),
    }
    // CB 短路在毫秒级；走 HTTP 至少毫秒级 mockito 往返
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "CB-open rejection should be fast (no HTTP), took: {:?}",
        elapsed
    );
    mock_a.assert_async().await;
}

/// 路由过滤直接由 VendorState 驱动：update_status(Unavailable) 后
/// route() 排除该 vendor，恢复 Available 后重新可被选中。
#[tokio::test]
async fn route_filter_follows_vendor_state_directly() {
    let mut server_a = mockito::Server::new_async().await;
    let _mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(OPENAI_SUCCESS_BODY)
        .create_async()
        .await;

    let mut server_b = mockito::Server::new_async().await;
    let _mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(OPENAI_SUCCESS_BODY)
        .create_async()
        .await;

    let v_a = make_vendor("vendor_a", server_a.url());
    let v_b = make_vendor("vendor_b", server_b.url());
    let cm = Arc::new(ConfigManager::new(make_config(vec![v_a, v_b])));
    let manager = Arc::new(VendorManager::new(cm.clone()));
    manager.initialize().await.expect("init should succeed");
    let router = Router::new(cm, manager.clone());

    // 公开 API 直接置 Unavailable → 路由过滤立即生效
    manager.update_status("vendor_a", VendorStatus::Unavailable);
    for _ in 0..50 {
        let decision = router.route("gpt-4").await.expect("vendor_b should route");
        assert_eq!(decision.vendor.id, "vendor_b");
    }

    // 恢复 Available → vendor_a 重新进入路由候选
    manager.update_status("vendor_a", VendorStatus::Available);
    let mut saw_a = false;
    for _ in 0..50 {
        let decision = router.route("gpt-4").await.expect("should route");
        if decision.vendor.id == "vendor_a" {
            saw_a = true;
        }
    }
    assert!(saw_a, "恢复 Available 后 vendor_a 应重新可被路由选中");
}

/// 评审 blocker 回归：完整恢复闭环。
/// 熔断 → 路由排除 → 真实睡眠越过 CB open_timeout(30s) → get_status 放行
/// → 直发 3 个探针成功（success_threshold=3）→ CB Closed → 路由恢复。
/// 评审探针实测：无此通路时（持久回写 Unavailable），排除是永久性的
/// （route 0/30 选中，即使 CB 内部已可半开）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn circuit_recovers_after_timeout_via_probes_and_route_resumes() {
    let mut server_a = mockito::Server::new_async().await;
    // 阶段 1 的失败 mock 限定在 scope 内：drop 后注销，探针流量改走成功 mock
    // （mockito 匹配为 LIFO，最新创建优先，双保险）
    let mock_fail;
    {
        mock_fail = server_a
            .mock("POST", "/v1/chat/completions")
            .with_status(400)
            .with_body("bad request")
            .expect_at_most(5) // 上界锁无隐藏重试；失败计数由 CB Open 断言承载
            .create_async()
            .await;
    }

    let mut server_b = mockito::Server::new_async().await;
    let _mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(OPENAI_SUCCESS_BODY)
        .create_async()
        .await;

    let v_a = make_vendor("vendor_a", server_a.url());
    let v_b = make_vendor("vendor_b", server_b.url());
    let cm = Arc::new(ConfigManager::new(make_config(vec![v_a, v_b])));
    let manager = Arc::new(VendorManager::new(cm.clone()));
    manager.initialize().await.expect("init should succeed");
    let router = Router::new(cm, manager.clone());

    // 阶段 1：5 次失败驱动熔断 → 路由排除
    for _ in 0..5 {
        let _ = manager
            .execute_request("vendor_a", make_unified_request())
            .await;
    }
    assert_eq!(
        manager.get_circuit_state("vendor_a").await,
        Some(CircuitState::Open)
    );
    assert_eq!(
        manager.get_status("vendor_a").await,
        VendorStatus::Unavailable
    );
    for _ in 0..30 {
        let decision = router.route("gpt-4").await.expect("vendor_b should route");
        assert_eq!(decision.vendor.id, "vendor_b", "熔断 vendor 被排除");
    }

    // 阶段 2：真实睡眠越过 CB open_timeout(30s)，注销失败 mock、改挂成功 mock
    drop(mock_fail);
    let mock_ok = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(OPENAI_SUCCESS_BODY)
        .expect(3)
        .create_async()
        .await;
    tokio::time::sleep(Duration::from_secs(31)).await;

    // 超时放行：get_status 不再 Unavailable（探测窗口开启）。
    //
    // 判定链为墙钟域：circuit_probe_ready 用 chrono::Utc::now() 与
    // last_failure_utc 差值比较（limiteron get_stats 仅暴露 UTC 域
    // src/error/mod.rs:176；Instant 域 last_failure_time 为私有字段
    // src/circuit/types.rs:165，无法在 manager 侧复刻库内单调钟判定）。
    // WSL2 实时钟相对 tokio 单调钟存在秒级回跳（实测：sleep(31s) 正常
    // 执行后 elapsed 仅 29.62s < 30s），瞬时断言会偶发误报。此处轮询
    // 等待窗口内放行——断言语义不变（恢复通路必须存在），仅容忍墙钟
    // 瞬时回跳；get_status 为只读 peek，轮询不消耗探针 mock（expect(3)）。
    let mut status = manager.get_status("vendor_a").await;
    for _ in 0..30 {
        if status == VendorStatus::Available {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        status = manager.get_status("vendor_a").await;
    }
    assert_eq!(
        status,
        VendorStatus::Available,
        "CB 超时后 get_status 必须放行探测流量（恢复通路）"
    );

    // 阶段 3：直发 3 个探针（绕过路由，同评审探针第二段）→ 收敛回 Closed
    for _ in 0..3 {
        let r = manager
            .execute_request("vendor_a", make_unified_request())
            .await;
        assert!(r.is_ok(), "probe should succeed: {:?}", r.err());
    }
    assert_eq!(
        manager.get_circuit_state("vendor_a").await,
        Some(CircuitState::Closed),
        "3 次探针成功后 CB 必须收敛回 Closed"
    );

    // 阶段 4：路由恢复 —— vendor_a 重新可被选中
    let mut saw_a = false;
    for _ in 0..50 {
        let decision = router.route("gpt-4").await.expect("should route");
        if decision.vendor.id == "vendor_a" {
            saw_a = true;
        }
    }
    assert!(
        saw_a,
        "熔断恢复后 vendor_a 必须重新进入路由候选（评审 blocker 回归）"
    );
    mock_ok.assert_async().await;
}
