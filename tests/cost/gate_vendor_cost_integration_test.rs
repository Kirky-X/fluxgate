// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "vendor-management")]

//! VendorManager × CostTracker 集成测试。
//! 计价消费 [[model_catalog]]（四档含 cache；[[pricing]] 留 SLA 因子消费）。
//!
//! 验证属性：
//! 1. mock 200 + usage 响应后 CostTracker::total_usd() > 0.0
//! 2. cost 值与目录计价公式一致（per 1M：prompt×in/1M + completion×out/1M；
//!    openai 型 Inclusive 语义——本套无 cache 档，与旧 per-1k 公式数值等价：
//!    per-1k 价 ×1000 = per-1M 价）
//! 3. 失败响应（500）不累加 cost
//! 4. by_vendor_usd 与 total_usd 一致（单 vendor 场景）
//! 5. by_model_usd 按 response.model 分桶
//! 6. Claude 计价不再失真：带 cache 档的 anthropic 响应 → cost 含
//!    cache_read/cache_creation 两档（端到端 trace，见文件尾测试）

#![cfg(feature = "cost-tracking")]

#[path = "../common/mod.rs"]
mod common;

use fluxgate::config::{Config, ConfigManager, ModelCatalogEntry, ServerConfig};
use fluxgate::protocol::{Message, MessageContent, Role, UnifiedRequest};
use fluxgate::vendor::VendorManager;
use std::sync::Arc;
use std::time::Duration;

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

/// 构造带 pricing 配置的 VendorManager + mockito server
async fn setup_cost_test(
    response_body: &str,
    status: usize,
) -> (Arc<VendorManager>, mockito::ServerGuard, mockito::Mock) {
    common::ensure_test_api_key();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(response_body)
        .create_async()
        .await;

    // 本文件语义差异，显式覆盖：展示名 "OpenAI Cost Test"、rps=100
    // （共享基座为 "Test openai-cost"、rps=1000）
    let mut v = common::base_vendor("openai-cost", format!("{}/v1", server.url()));
    v.name = "OpenAI Cost Test".into();
    v.concurrency.requests_per_second = 100;

    let config = Config {
        // 本文件语义差异，显式保留：0.0.0.0:8080（共享基座为 127.0.0.1:0）
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        // 定价面迁移——per-1k 0.03/0.06 ≡ per-1M 30.0/60.0（数值等价）
        model_catalog: vec![ModelCatalogEntry {
            model_id: "gpt-4".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 0.0,
            input_price: 30.0,
            output_price: 60.0,
            currency: fluxgate::config::PricingCurrency::Usd,
            ..Default::default()
        }],
        ..common::test_config(vec![v])
    };

    let cm = Arc::new(ConfigManager::new(config));
    let manager = Arc::new(VendorManager::new(cm));
    manager.initialize().await.expect("init should succeed");

    (manager, server, mock)
}

#[tokio::test]
async fn test_cost_tracker_accumulates_after_successful_request() {
    // usage.prompt_tokens=5, completion_tokens=1（openai Inclusive：基数=5）
    // expected = 5×30/1e6 + 1×60/1e6 = 0.00015 + 0.00006 = 0.00021 USD
    let (manager, _server, mock) = setup_cost_test(
        r#"{"id":"chatcmpl-1","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
        200,
    )
    .await;

    // 初始状态：cost = 0
    assert_eq!(
        manager.cost_tracker().total_usd(),
        0.0,
        "fresh tracker should be 0.0"
    );

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        manager.execute_request("openai-cost", make_unified_request()),
    )
    .await
    .expect("request should not timeout");
    assert!(result.is_ok(), "request should succeed: {:?}", result.err());

    // 成功后 cost_tracker 应累积
    let total = manager.cost_tracker().total_usd();
    assert!(
        total > 0.0,
        "cost_tracker total should be > 0 after success, got {}",
        total
    );
    assert!(
        (total - 0.00021).abs() < 1e-9,
        "expected 0.00021 USD (5*30/1e6 + 1*60/1e6), got {}",
        total
    );

    mock.assert_async().await;
}

#[tokio::test]
async fn test_cost_tracker_by_vendor_matches_total() {
    let (manager, _server, _mock) = setup_cost_test(
        r#"{"id":"chatcmpl-2","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#,
        200,
    )
    .await;

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        manager.execute_request("openai-cost", make_unified_request()),
    )
    .await
    .expect("no timeout")
    .expect("request ok");

    let total = manager.cost_tracker().total_usd();
    let by_vendor = manager.cost_tracker().by_vendor_usd("openai-cost");
    assert!(
        (total - by_vendor).abs() < 1e-9,
        "by_vendor should equal total in single-vendor scenario: total={}, by_vendor={}",
        total,
        by_vendor
    );
    // expected = 10×30/1e6 + 2×60/1e6 = 0.0003 + 0.00012 = 0.00042
    assert!(
        (total - 0.00042).abs() < 1e-9,
        "expected 0.00042 USD, got {}",
        total
    );
}

#[tokio::test]
async fn test_cost_tracker_by_model_buckets_by_response_model() {
    let (manager, _server, _mock) = setup_cost_test(
        r#"{"id":"chatcmpl-3","object":"chat.completion","created":1700000000,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#,
        200,
    )
    .await;

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        manager.execute_request("openai-cost", make_unified_request()),
    )
    .await
    .expect("no timeout")
    .expect("request ok");

    let by_model = manager.cost_tracker().by_model_usd("gpt-4");
    // expected = 100×30/1e6 + 50×60/1e6 = 0.003 + 0.003 = 0.006
    assert!(
        (by_model - 0.006).abs() < 1e-9,
        "by_model(gpt-4) should be 0.006 USD, got {}",
        by_model
    );
}

#[tokio::test]
async fn test_cost_tracker_not_incremented_on_error_response() {
    // 500 错误响应不应累加 cost（worker 不会进入 Ok 分支）
    let (manager, _server, _mock) = setup_cost_test("internal server error", 500).await;

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        manager.execute_request("openai-cost", make_unified_request()),
    )
    .await
    .expect("no timeout");

    assert!(result.is_err(), "500 should produce error");

    let total = manager.cost_tracker().total_usd();
    assert_eq!(
        total, 0.0,
        "cost_tracker should remain 0.0 after error response, got {}",
        total
    );
}

/// 端到端 trace：Claude 计价不再失真——带 cache 档的 anthropic 响应
/// usage（input_tokens 不含 cache 档，Exclusive 语义）经目录四档计价，
/// cost 含 cache_read/cache_creation 两档（旧 [[pricing]] 双档公式对
/// cache token 全部漏计）。
#[tokio::test]
async fn claude_cache_token_usage_priced_with_cache_tiers() {
    common::ensure_test_api_key();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "id":"msg-1","type":"message","role":"assistant","model":"claude-sonnet-4-20250514",
                "content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn",
                "usage":{"input_tokens":1000,"output_tokens":500,
                         "cache_read_input_tokens":2000,"cache_creation_input_tokens":100000}
            }"#,
        )
        .create_async()
        .await;

    let mut v = common::base_vendor("claude-cost", format!("{}/v1", server.url()));
    v.type_ = "anthropic".into();
    let config = Config {
        model_catalog: vec![ModelCatalogEntry {
            model_id: "claude-sonnet-4-20250514".into(),
            context_window: 200_000,
            rerank_price_per_1k_docs: 0.0,
            input_price: 3.0,
            output_price: 15.0,
            cache_read_price: 0.3,
            cache_creation_price: 3.75,
            long_context_input_price: 0.0,
            long_context_threshold_tokens: 0,
            currency: fluxgate::config::PricingCurrency::Usd,
            max_output_tokens: Some(8192),
            supports_tools: true,
            supports_thinking: true,
            cache_breakpoints: None,
        }],
        ..common::test_config(vec![v])
    };
    let cm = Arc::new(ConfigManager::new(config));
    let manager = Arc::new(VendorManager::new(cm));
    manager.initialize().await.expect("init");

    let mut request = make_unified_request();
    request.model = "claude-sonnet-4-20250514".into();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        manager.execute_request("claude-cost", request),
    )
    .await
    .expect("no timeout")
    .expect("request ok");

    // IR usage cache 档保真（铺设的响应面）
    assert_eq!(result.usage.cache_read_tokens, 2_000);
    assert_eq!(result.usage.cache_creation_tokens, 100_000);

    // Exclusive（anthropic input 不含 cache 档）：
    // 1000×3/1e6 + 500×15/1e6 + 2000×0.3/1e6 + 100000×3.75/1e6
    // = 0.003 + 0.0075 + 0.0006 + 0.375 = 0.3861 USD（cache 档占 0.3756）
    let total = manager.cost_tracker().total_usd();
    assert!(
        (total - 0.3861).abs() < 1e-9,
        "expected 0.3861 USD (含 cache 两档 0.3756), got {}",
        total
    );
    let by_model = manager
        .cost_tracker()
        .by_model_usd("claude-sonnet-4-20250514");
    assert!((by_model - 0.3861).abs() < 1e-9);

    mock.assert_async().await;
}
