// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK Responses 客户端集成测试（mockito）。

use futures::StreamExt as _;

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::{ResponsesOutputItem, ResponsesRequest};

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
        temperature: None,
        top_p: None,
        tools: None,
        tool_choice: None,
        text: None,
        reasoning: None,
        previous_response_id: None,
        conversation: None,
        store: Some(true),
        parallel_tool_calls: None,
        truncation: None,
        metadata: None,
        background: false,
        user: None,
        config: RequestConfig::default(),
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
        FluxgateSdkError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("期望 Server 错误: {other:?}"),
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
