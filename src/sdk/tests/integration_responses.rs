// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK Responses 客户端集成测试（mockito）。

use futures::StreamExt as _;

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::{ApiError, FluxgateSdkError};
use crate::sdk::{ResponsesInputTokensRequest, ResponsesOutputItem, ResponsesRequest};

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

fn request() -> ResponsesRequest {
    ResponsesRequest {
        model: "gpt-4.1".into(),
        input: serde_json::json!("hello"),
        instructions: Some("be brief".into()),
        max_output_tokens: Some(32),
        store: Some(true),
        ..Default::default()
    }
}

fn response_body() -> &'static str {
    r#"{"id":"resp_1","object":"response","created_at":1,"status":"completed","model":"gpt-4.1",
        "output":[{"type":"message","id":"msg_1","status":"completed","role":"assistant",
            "content":[{"type":"output_text","text":"hi there","annotations":[]}]}],
        "usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5,
            "input_tokens_details":{"cached_tokens":1}}}"#
}

#[tokio::test]
async fn responses_success_parses_response() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/responses")
        .match_body(mockito::Matcher::Regex(
            r#""instructions":"be brief""#.into(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(response_body())
        .create();
    let client = build_client(&server.url());

    let response = client.responses(&request()).await.expect("响应解析");
    assert_eq!(response.id, "resp_1");
    assert_eq!(response.status, "completed");
    assert_eq!(response.usage.input_tokens, 3);
    assert_eq!(
        response
            .usage
            .input_tokens_details
            .as_ref()
            .unwrap()
            .cached_tokens,
        1
    );
    match &response.output[0] {
        ResponsesOutputItem::Message { content, .. } => {
            assert_eq!(content[0].text.as_deref(), Some("hi there"));
        }
        other => panic!("期望 message item: {other:?}"),
    }
    mock.assert();
}

#[tokio::test]
async fn responses_server_error_maps_to_sdk_error() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/responses")
        .with_status(500)
        .with_body("boom")
        .create();
    let client = build_client(&server.url());
    let error = client.responses(&request()).await.expect_err("必须报错");
    match error {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("期望 Api 错误: {other:?}"),
    }
}

#[tokio::test]
async fn responses_stream_parses_events() {
    let mut server = mockito::Server::new_async().await;
    let sse_body = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"model\":\"gpt-4.1\",\"output\":[],\"usage\":{}}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"model\":\"gpt-4.1\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":1,\"total_tokens\":3}}}\n\n"
    );
    server
        .mock("POST", "/v1/responses")
        .match_body(mockito::Matcher::Regex(r#""stream":true"#.into()))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create();
    let client = build_client(&server.url());

    let mut stream = client.responses_stream(&request()).await.expect("流式响应");
    let mut event_types = Vec::new();
    while let Some(event) = stream.next().await {
        event_types.push(event.expect("事件解析").event_type);
    }
    assert_eq!(
        event_types,
        vec![
            "response.created",
            "response.output_text.delta",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn get_and_delete_response() {
    let mut server = mockito::Server::new_async().await;
    let get = server
        .mock("GET", "/v1/responses/resp_1")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(response_body())
        .create();
    let delete = server
        .mock("DELETE", "/v1/responses/resp_1")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"resp_1","object":"response.deleted","deleted":true}"#)
        .create();
    let client = build_client(&server.url());

    let fetched = client.get_response("resp_1").await.expect("检索");
    assert_eq!(fetched.id, "resp_1");
    let deleted = client.delete_response("resp_1").await.expect("删除");
    assert_eq!(deleted["deleted"], true);
    get.assert();
    delete.assert();
}

/// 扩展 typed 字段与 extra_body 进 payload；extra_body 与 typed 冲突时
/// typed 优先（D7，spec R-sdk-client 验收：include + max_tool_calls）。
#[tokio::test]
async fn responses_payload_includes_extended_fields() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/responses")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "include": ["reasoning.encrypted_content"],
            "max_tool_calls": 3,
            "top_logprobs": 5,
            "prompt_cache_key": "pc-1",
            "prompt_cache_retention": "24h",
            "safety_identifier": "safe-1",
            "service_tier": "flex",
            "stream_options": {"include_usage": true},
            "context_management": {"truncation": "auto"},
            "moderation": {"model": "omni-moderation-latest"},
            "access_programs": {"enabled": true},
            "experimental_flag": true,
            "max_output_tokens": 32
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(response_body())
        .expect(1)
        .create_async()
        .await;

    let mut req = request();
    req.include = Some(vec!["reasoning.encrypted_content".into()]);
    req.max_tool_calls = Some(3);
    req.top_logprobs = Some(5);
    req.prompt_cache_key = Some("pc-1".into());
    req.prompt_cache_retention = Some("24h".into());
    req.safety_identifier = Some("safe-1".into());
    req.service_tier = Some("flex".into());
    req.stream_options = Some(serde_json::json!({"include_usage": true}));
    req.context_management = Some(serde_json::json!({"truncation": "auto"}));
    req.moderation = Some(serde_json::json!({"model": "omni-moderation-latest"}));
    req.access_programs = Some(serde_json::json!({"enabled": true}));
    req.extra_body
        .insert("experimental_flag".to_string(), serde_json::json!(true));
    // typed 优先：extra_body 同名键被 max_output_tokens=32 覆写
    req.extra_body
        .insert("max_output_tokens".to_string(), serde_json::json!(999));

    build_client(&server.url())
        .responses(&req)
        .await
        .expect("响应解析");
    mock.assert_async().await;
}

/// 请求级 extra_headers 注入（大小写不敏感）；routing hint 头一并生效。
#[tokio::test]
async fn responses_extra_headers_injected() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/responses")
        .match_header("x-trace", "req-1")
        .match_header("x-session-id", "sess-9")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(response_body())
        .expect(1)
        .create_async()
        .await;

    let mut req = request();
    req.extra_headers
        .insert("X-Trace".to_string(), "req-1".to_string());
    req.config = RequestConfig {
        session_id: Some("sess-9".into()),
        ..Default::default()
    };

    build_client(&server.url())
        .responses(&req)
        .await
        .expect("响应解析");
    mock.assert_async().await;
}

/// `POST /v1/responses/{id}/cancel` 路由与 cancelled 状态解析。
#[tokio::test]
async fn cancel_response_hits_cancel_route() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/responses/resp_bg/cancel")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"resp_bg","object":"response","created_at":1,"status":"cancelled","model":"gpt-4.1","output":[],"usage":{}}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let response = build_client(&server.url())
        .cancel_response("resp_bg")
        .await
        .expect("取消响应");
    assert_eq!(response.id, "resp_bg");
    assert_eq!(response.status, "cancelled");
    mock.assert_async().await;
}

/// `POST /v1/responses/input_tokens`：载荷路由 + 估算解析；空
/// model/input 在 SDK 侧拒绝（Protocol，不发请求）。
#[tokio::test]
async fn input_tokens_estimates_and_validates() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/responses/input_tokens")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "model": "gpt-4.1",
            "input": "hello"
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"object":"response.input_tokens","input_tokens":7}"#)
        .expect(1)
        .create_async()
        .await;

    let client = build_client(&server.url());
    let req = ResponsesInputTokensRequest::new("gpt-4.1", serde_json::json!("hello"));
    let estimate = client.input_tokens(&req).await.expect("估算");
    assert_eq!(estimate.object, "response.input_tokens");
    assert_eq!(estimate.input_tokens, 7);
    mock.assert_async().await;

    // 非空校验：空白 model、空字符串/空数组 input → Protocol，不发请求
    for req in [
        ResponsesInputTokensRequest::new("  ", serde_json::json!("hello")),
        ResponsesInputTokensRequest::new("gpt-4.1", serde_json::json!("")),
        ResponsesInputTokensRequest::new("gpt-4.1", serde_json::json!([])),
    ] {
        match client.input_tokens(&req).await.unwrap_err() {
            FluxgateSdkError::Protocol { message } => {
                assert!(
                    message.contains("model") || message.contains("input"),
                    "拒绝消息应定位字段: {message}"
                );
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }
}
