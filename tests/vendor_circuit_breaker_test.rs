// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! T035a 集成测试 — CircuitBreaker 接入 VendorManager。
//!
//! 测试策略（规则9：验证有意义属性）：
//! - "5 次失败 → Open" 状态机已在 T033 单元测试覆盖，此处不重复
//! - T035a 关注**集成属性**：
//!   1. CB Open 时 `execute_request` 快速拒绝（无 HTTP 调用），错误含 "circuit open"
//!   2. CB 按 vendor_id 隔离 — vendor_a 熔断不影响 vendor_b
//! - 用 `force_circuit_open` 直接设置状态，避免
//!   `tokio::test(start_paused = true)` 与 mockito 真实 HTTP IO 冲突
//!   （auto-advance 会提前触发 execute_request 的 60s 超时）

#![cfg(feature = "resilience")]

use nexusflow::config::{
    ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
    RateLimitConfig, ServerConfig, VendorConfig,
};
use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
use nexusflow::vendor::VendorManager;
use nexusflow::vendor::resilience::State as CircuitBreakerState;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn ensure_test_api_key() {
    std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
}

fn make_vendor(id: &str, api_base: String) -> VendorConfig {
    ensure_test_api_key();
    VendorConfig {
        id: id.into(),
        name: format!("Test {}", id),
        type_: "openai".into(),
        api_key_env: "NEXUSFLOW_TEST_KEY".into(),
        api_base,
        enabled: true,
        models: vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,
        }],
        rate_limit: RateLimitConfig::Token {
            max_tokens: 1_000_000,
        },
        concurrency: ConcurrencyConfig {
            requests_per_second: 100,
        },
    }
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

/// CB Open 状态下 execute_request 应快速拒绝，错误信息含 "circuit open"，
/// 无 HTTP 调用（elapsed 远小于一次重试循环的 3s+）。
#[tokio::test]
async fn test_circuit_open_rejects_request_fast() {
    let mut server_a = mockito::Server::new_async().await;
    // mock 设置为 500 — 若 CB 失效走到 HTTP，会触发 3 次重试（每次 1s 间隔）
    // 通过 elapsed < 1s 即可证明 CB 短路生效
    let mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("upstream error")
        .expect(0) // 关键断言：CB Open 时不应有任何 HTTP 调用
        .create_async()
        .await;

    let v_a = make_vendor("vendor_a", server_a.url());
    let cm = Arc::new(ConfigManager::new(make_config(vec![v_a])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init should succeed");

    // 直接将 CB 切到 Open — 跳过"5 次真实失败"慢路径
    // 该路径已在 T033 单元测试覆盖
    manager.force_circuit_open("vendor_a");
    assert!(
        matches!(
            manager.get_circuit_state("vendor_a"),
            Some(CircuitBreakerState::Open { .. })
        ),
        "vendor_a CB should be Open after force_circuit_open"
    );

    let start = Instant::now();
    let result = manager
        .execute_request("vendor_a", make_unified_request())
        .await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "request against open circuit should fail"
    );
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("circuit open"),
        "error should mention 'circuit open', got: {}",
        err_msg
    );
    // CB Open 短路在毫秒级；HTTP+重试至少 3s。1s 阈值留充足余量
    assert!(
        elapsed < Duration::from_secs(1),
        "CB-open rejection should be fast (no HTTP), took: {:?}",
        elapsed
    );

    // 验证 mockito mock 真的没被调用
    mock_a.assert_async().await;
}

/// CB 按 vendor_id 隔离 — vendor_a 熔断不影响 vendor_b。
#[tokio::test]
async fn test_circuit_isolated_per_vendor() {
    let mut server_a = mockito::Server::new_async().await;
    let _mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("error")
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
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init should succeed");

    // 熔断 vendor_a，但不动 vendor_b
    manager.force_circuit_open("vendor_a");
    assert!(
        matches!(
            manager.get_circuit_state("vendor_a"),
            Some(CircuitBreakerState::Open { .. })
        ),
        "vendor_a CB should be Open"
    );
    assert!(
        matches!(
            manager.get_circuit_state("vendor_b"),
            Some(CircuitBreakerState::Closed)
        ),
        "vendor_b CB should remain Closed (per-vendor isolation)"
    );

    // vendor_b 请求应成功走完 HTTP
    let result = manager
        .execute_request("vendor_b", make_unified_request())
        .await;
    assert!(
        result.is_ok(),
        "vendor_b request should succeed after vendor_a circuit open: {:?}",
        result.err()
    );

    // vendor_a 状态未受影响
    assert!(
        matches!(
            manager.get_circuit_state("vendor_a"),
            Some(CircuitBreakerState::Open { .. })
        ),
        "vendor_a CB should still be Open after vendor_b success"
    );
    mock_b.assert_async().await;
}
