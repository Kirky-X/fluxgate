// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 重试集成测试：mockito 连续响应验证 429→200、次数上限、
//! `x-should-retry` 否决、`Retry-After` 提示优先于退避。

use std::time::{Duration, Instant};

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::error::{ApiError, FluxgateSdkError};
use crate::sdk::retry::RetryPolicy;
use crate::sdk::transport::send_with_retry;
use crate::sdk::ChatRequest;

/// 网关成功响应形状（同 `integration_chat::chat_success` 子集）。
const CHAT_OK: &str = r#"{"id":"chatcmpl-retry","object":"chat.completion","model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#;

const CHAT_429: &str = r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#;

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

#[tokio::test]
async fn chat_retries_429_then_succeeds() {
    let mut server = mockito::Server::new_async().await;

    let throttled = server
        .mock("POST", "/v1/chat/completions")
        .with_status(429)
        .with_header("retry-after", "0")
        .with_body(CHAT_429)
        .expect(1)
        .create_async()
        .await;
    let success = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let resp = build_client(&server.url())
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .expect("429 后应重试并成功");

    assert_eq!(resp.content, "Hi");
    throttled.assert_async().await;
    success.assert_async().await;
}

#[tokio::test]
async fn retries_capped_by_max_retries() {
    let mut server = mockito::Server::new_async().await;

    // 默认 max_retries = 2 → 总计 3 次尝试（1 初试 + 2 重试）
    let failing = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_header("retry-after", "0")
        .with_body("boom")
        .expect(3)
        .create_async()
        .await;

    let err = build_client(&server.url())
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap_err();

    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }
    failing.assert_async().await;
}

#[tokio::test]
async fn should_retry_false_vetoes_retry() {
    let mut server = mockito::Server::new_async().await;

    // 500 本可重试，但 x-should-retry: false 显式否决 → 只打 1 次
    let failing = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_header("retry-after", "0")
        .with_header("x-should-retry", "false")
        .with_body("boom")
        .expect(1)
        .create_async()
        .await;

    let err = build_client(&server.url())
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap_err();

    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }
    failing.assert_async().await;
}

#[tokio::test]
async fn max_retries_zero_disables_retry() {
    let mut server = mockito::Server::new_async().await;

    let failing = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_header("retry-after", "0")
        .with_body("boom")
        .expect(1)
        .create_async()
        .await;

    let client = FluxgateClientBuilder::new()
        .base_url(server.url())
        .api_key("test-key")
        .max_retries(0)
        .build()
        .unwrap();
    let err = client
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap_err();

    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }
    failing.assert_async().await;
}

#[tokio::test]
async fn retry_after_hint_shortcircuits_backoff() {
    let mut server = mockito::Server::new_async().await;

    let throttled = server
        .mock("POST", "/v1/chat/completions")
        .with_status(429)
        .with_header("retry-after", "0")
        .with_body(CHAT_429)
        .expect(1)
        .create_async()
        .await;
    let success = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    // 退避初始 30s：若 Retry-After 提示未生效，本测必然超时
    let policy = RetryPolicy {
        max_retries: 1,
        initial: Duration::from_secs(30),
        max: Duration::from_secs(30),
        max_retry_after: Duration::from_secs(120),
    };
    let http = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", server.url());
    let request = http
        .post(&url)
        .json(&serde_json::json!({"model": "gpt-4", "messages": []}));
    let started = Instant::now();
    let response = send_with_retry(&request, &policy)
        .await
        .expect("Retry-After 提示应短路 30s 退避");

    assert_eq!(response.status().as_u16(), 200);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "Retry-After: 0 应即时重试（30s 退避未生效时: {:?}）",
        started.elapsed()
    );
    throttled.assert_async().await;
    success.assert_async().await;
}
