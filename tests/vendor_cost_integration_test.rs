#![cfg(feature = "vendor-management")]

//! VendorManager × CostTracker 集成测试。
//!
//! 验证属性（规则9）：
//! 1. mock 200 + usage 响应后 CostTracker::total_usd() > 0.0
//! 2. cost 值与 CostCalculator 公式一致：`(prompt/1000)*in + (completion/1000)*out`
//! 3. 失败响应（500）不累加 cost
//! 4. by_vendor_usd 与 total_usd 一致（单 vendor 场景）
//! 5. by_model_usd 按 response.model 分桶

#![cfg(feature = "cost-tracking")]

use nexusflow::config::{
    ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
    PricingConfig, RateLimitConfig, ServerConfig, VendorConfig,
};
use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
use nexusflow::vendor::VendorManager;
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
    }
}

/// 构造带 pricing 配置的 VendorManager + mockito server
async fn setup_cost_test(
    response_body: &str,
    status: usize,
) -> (Arc<VendorManager>, mockito::ServerGuard, mockito::Mock) {
    std::env::set_var("NEXUSFLOW_TEST_KEY", "test-key-for-mockito");
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(response_body)
        .create_async()
        .await;

    let v = VendorConfig {
        timeout_ms: 60_000,
        stream_ttft_ms: 60_000,
        stream_idle_ms: 30_000,
        id: "openai-cost".into(),
        name: "OpenAI Cost Test".into(),
        type_: "openai".into(),
        api_key_env: "NEXUSFLOW_TEST_KEY".into(),
        api_base: format!("{}/v1", server.url()),
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
    };

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
        vendors: vec![v],
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![PricingConfig {
            vendor_id: "openai-cost".into(),
            model_id: "gpt-4".into(),
            input_per_1k_tokens: 0.03,
            output_per_1k_tokens: 0.06,
        }],
        auth: None,
        cors: None,
        retry: nexusflow::config::RetrySection::default(),
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        resilience: nexusflow::config::ResilienceSection::default(),
    };

    let cm = Arc::new(ConfigManager::new(config));
    let manager = Arc::new(VendorManager::new(cm));
    manager.initialize().await.expect("init should succeed");

    (manager, server, mock)
}

#[tokio::test]
async fn test_cost_tracker_accumulates_after_successful_request() {
    // usage.prompt_tokens=5, completion_tokens=1
    // expected = (5/1000)*0.03 + (1/1000)*0.06 = 0.00015 + 0.00006 = 0.00021 USD
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
        "expected 0.00021 USD (5*0.03/1000 + 1*0.06/1000), got {}",
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
    // expected = (10/1000)*0.03 + (2/1000)*0.06 = 0.0003 + 0.00012 = 0.00042
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
    // expected = (100/1000)*0.03 + (50/1000)*0.06 = 0.003 + 0.003 = 0.006
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
