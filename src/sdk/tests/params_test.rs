// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 请求参数面测试：头注入（default_headers / extra_headers / X-Session-Id /
//! X-Batch / X-Weight）、受限头过滤、`extra_body` 合并与 RESERVED_KEYS
//! 拒绝、请求级 `max_retries` 覆盖。

use std::collections::BTreeMap;

use serde_json::json;

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::ChatRequest;

const CHAT_OK: &str = r#"{"id":"chatcmpl-p","object":"chat.completion","model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

fn request_with_config(config: RequestConfig) -> ChatRequest {
    let mut req = ChatRequest::new("gpt-4", "hello");
    req.config = config;
    req
}

/// X-Session-Id / X-Batch / X-Weight / X-Vendor / X-Model 从 typed 字段
/// 统一注入（网关 routing_hints 口径：batch 值为 "1"）。
#[tokio::test]
async fn routing_hint_headers_injected() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("x-vendor", "anthropic")
        .match_header("x-model", "claude-x")
        .match_header("x-session-id", "sess-42")
        .match_header("x-batch", "1")
        .match_header("x-weight", "0.5")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let config = RequestConfig {
        vendor_hint: Some("anthropic".into()),
        model_hint: Some("claude-x".into()),
        session_id: Some("sess-42".into()),
        batch: Some(true),
        weight: Some(0.5),
        ..Default::default()
    };
    build_client(&server.url())
        .chat(&request_with_config(config))
        .await
        .expect("chat");

    mock.assert_async().await;
}

/// 路由头只能经 typed 字段设置：`extra_headers` 里的同名键被忽略并 debug
///（reqwest `header()` 为追加语义，扩展头先插入会以首个值压过 typed 注入）。
#[tokio::test]
async fn routing_headers_in_extra_headers_are_rejected() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("x-vendor", mockito::Matcher::Missing)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let config = RequestConfig {
        extra_headers: BTreeMap::from([("X-Vendor".to_string(), "spoofed".to_string())]),
        ..Default::default()
    };
    build_client(&server.url())
        .chat(&request_with_config(config))
        .await
        .expect("chat");

    mock.assert_async().await;
}

/// 客户端 default_headers 与请求级 extra_headers：请求级覆盖同名键，
/// 覆盖比较大小写不敏感。
#[tokio::test]
async fn default_headers_merge_with_case_insensitive_request_override() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("x-trace", "override")
        .match_header("x-tenant", "acme")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let client = FluxgateClientBuilder::new()
        .base_url(server.url())
        .api_key("test-key")
        .default_headers(BTreeMap::from([
            ("x-trace".to_string(), "default".to_string()),
            ("x-tenant".to_string(), "acme".to_string()),
        ]))
        .build()
        .unwrap();

    let config = RequestConfig {
        // 大小写不同仍覆盖 default 的 x-trace
        extra_headers: BTreeMap::from([("X-Trace".to_string(), "override".to_string())]),
        ..Default::default()
    };
    client
        .chat(&request_with_config(config))
        .await
        .expect("chat");

    mock.assert_async().await;
}

/// 受限头（authorization/host/content-length）忽略：真实凭证未被配置面
/// 覆写（若被覆写，match_header 不中 → 501 → chat 失败）。
#[tokio::test]
async fn restricted_headers_ignored() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let config = RequestConfig {
        extra_headers: BTreeMap::from([
            ("Authorization".to_string(), "Bearer hacked".to_string()),
            ("Host".to_string(), "evil.example".to_string()),
            ("Content-Length".to_string(), "0".to_string()),
        ]),
        ..Default::default()
    };
    build_client(&server.url())
        .chat(&request_with_config(config))
        .await
        .expect("chat（受限头被忽略，凭证保真）");

    mock.assert_async().await;
}

/// extra_body 先合并、vendor_extensions 后合并（后者优先）。
#[tokio::test]
async fn extra_body_merges_and_vendor_extensions_win() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::PartialJson(json!({
            "example_flag": true,
            "conflict_key": 2
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CHAT_OK)
        .expect(1)
        .create_async()
        .await;

    let config = RequestConfig {
        extra_body: BTreeMap::from([
            ("example_flag".to_string(), json!(true)),
            ("conflict_key".to_string(), json!(1)),
        ]),
        ..Default::default()
    };
    let mut req = request_with_config(config);
    req.vendor_extensions
        .insert("conflict_key".to_string(), json!(2));

    build_client(&server.url()).chat(&req).await.expect("chat");

    mock.assert_async().await;
}

/// RESERVED_KEYS 拒绝：extra_body 与 vendor_extensions 均不得覆写
/// typed 请求骨架（不发请求，直接 Protocol 错误）。
#[tokio::test]
async fn reserved_keys_rejected_for_extra_body_and_vendor_extensions() {
    let server = mockito::Server::new_async().await;
    let client = build_client(&server.url());

    let config = RequestConfig {
        extra_body: BTreeMap::from([("temperature".to_string(), json!(0.5))]),
        ..Default::default()
    };
    let err = client.chat(&request_with_config(config)).await.unwrap_err();
    match err {
        FluxgateSdkError::Protocol { message } => {
            assert!(
                message.contains("extra_body") && message.contains("temperature"),
                "拒绝消息应定位来源与键: {message}"
            );
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    // vendor_extensions 直接字段写入同样被拒（不依赖 with_vendor_extension）
    let mut req = ChatRequest::new("gpt-4", "hello");
    req.vendor_extensions
        .insert("model".to_string(), json!("evil"));
    let err = client.chat(&req).await.unwrap_err();
    match err {
        FluxgateSdkError::Protocol { message } => {
            assert!(
                message.contains("vendor extension") && message.contains("model"),
                "拒绝消息应定位来源与键: {message}"
            );
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
}

/// 请求级 `max_retries` 覆盖客户端默认：0 表示不重试（500 只打 1 次）。
#[tokio::test]
async fn request_level_max_retries_override() {
    let mut server = mockito::Server::new_async().await;
    let failing = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_header("retry-after", "0")
        .with_body("boom")
        .expect(1)
        .create_async()
        .await;

    let config = RequestConfig {
        max_retries: Some(0),
        ..Default::default()
    };
    let err = build_client(&server.url())
        .chat(&request_with_config(config))
        .await
        .unwrap_err();
    match err {
        FluxgateSdkError::Api(crate::sdk::ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }

    failing.assert_async().await;
}

/// config.merge：extra_body/extra_headers 逐键合并（other 优先），
/// max_retries/session_id/batch/weight 为 Option 覆盖语义。
#[test]
fn merge_extended_fields_semantics() {
    let base = RequestConfig {
        extra_body: BTreeMap::from([("a".to_string(), json!(1)), ("b".to_string(), json!(1))]),
        extra_headers: BTreeMap::from([("x-a".to_string(), "1".to_string())]),
        max_retries: Some(3),
        session_id: Some("s1".into()),
        batch: Some(false),
        weight: Some(0.5),
        ..Default::default()
    };
    let override_cfg = RequestConfig {
        extra_body: BTreeMap::from([("b".to_string(), json!(2))]),
        extra_headers: BTreeMap::from([("x-b".to_string(), "2".to_string())]),
        max_retries: Some(0),
        session_id: Some("s2".into()),
        ..Default::default()
    };
    let merged = base.merge(&override_cfg);

    assert_eq!(merged.extra_body["a"], json!(1), "未冲突键保留");
    assert_eq!(merged.extra_body["b"], json!(2), "other 覆盖");
    assert_eq!(merged.extra_headers["x-a"], "1");
    assert_eq!(merged.extra_headers["x-b"], "2");
    assert_eq!(merged.max_retries, Some(0));
    assert_eq!(merged.session_id.as_deref(), Some("s2"));
    assert_eq!(merged.batch, Some(false), "other 未设 → 保留 self");
    assert_eq!(merged.weight, Some(0.5));
}
