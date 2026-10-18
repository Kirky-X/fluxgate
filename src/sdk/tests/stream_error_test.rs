// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流内错误帧测试：`event: error` 与顶层 `{"error":…}` 负载必须产出
//! `Err(Stream)`——错误帧不再静默结束流（修复前会被当作心跳丢弃）。

use bytes::Bytes;
use futures::StreamExt;

use crate::sdk::error::FluxgateSdkError;
use crate::sdk::stream::ChatStream;

fn byte_stream(
    frames: &[&str],
) -> impl futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static {
    let owned: Vec<Result<Bytes, reqwest::Error>> = frames
        .iter()
        .map(|frame| Ok(Bytes::from(frame.to_string())))
        .collect();
    futures::stream::iter(owned)
}

fn expect_stream_error_message(err: &FluxgateSdkError) -> String {
    match err {
        FluxgateSdkError::Stream { message } => message.clone(),
        other => panic!("expected Stream error, got {other:?}"),
    }
}

#[tokio::test]
async fn event_error_frame_yields_stream_error_not_silent_end() {
    let mut stream = ChatStream::from_bytes_stream(byte_stream(&[
        "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
        "event: error\ndata: {\"error\":{\"message\":\"upstream boom\"}}\n\n",
        "data: [DONE]\n\n",
    ]));

    let first = stream
        .next()
        .await
        .expect("first frame")
        .expect("content ok");
    assert_eq!(first.delta.and_then(|d| d.content).as_deref(), Some("Hi"));

    let err = stream
        .next()
        .await
        .expect("错误帧不得被静默丢弃（此前会直接结束流）")
        .unwrap_err();
    assert!(
        expect_stream_error_message(&err).contains("upstream boom"),
        "错误消息应取 error.message: {err:?}"
    );
}

#[tokio::test]
async fn top_level_error_payload_yields_stream_error() {
    let mut stream = ChatStream::from_bytes_stream(byte_stream(&[
        "data: {\"error\":{\"message\":\"rate limited\",\"type\":\"rate_limit_error\"}}\n\n",
    ]));

    let err = stream.next().await.expect("must yield error").unwrap_err();
    assert!(expect_stream_error_message(&err).contains("rate limited"));
}

#[tokio::test]
async fn error_frame_with_raw_text_payload_reflects_data() {
    let mut stream =
        ChatStream::from_bytes_stream(byte_stream(&["event: error\ndata: gateway exploded\n\n"]));

    let err = stream.next().await.expect("must yield error").unwrap_err();
    assert!(expect_stream_error_message(&err).contains("gateway exploded"));
}

#[tokio::test]
async fn null_error_field_is_not_an_error_frame() {
    // error: null 不构成错误帧；无 id/choices/usage → 心跳语义丢弃，
    // 后续 usage 帧照常解析
    let mut stream = ChatStream::from_bytes_stream(byte_stream(&[
        "data: {\"error\":null}\n\n",
        "data: {\"id\":\"u\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    ]));

    let chunk = stream.next().await.expect("usage frame").expect("ok");
    assert!(chunk.usage.is_some());
    assert!(stream.next().await.is_none());
}

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    crate::sdk::client::FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

fn responses_request() -> crate::sdk::ResponsesRequest {
    crate::sdk::ResponsesRequest {
        model: "gpt-4.1".into(),
        input: serde_json::json!("hi"),
        ..Default::default()
    }
}

const RESPONSES_CREATED: &str = "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"model\":\"gpt-4.1\",\"output\":[],\"usage\":{}}}\n\n";

#[tokio::test]
async fn responses_error_event_yields_stream_error() {
    let mut server = mockito::Server::new_async().await;
    let sse_body = format!(
        "{RESPONSES_CREATED}event: error\ndata: {{\"type\":\"error\",\"code\":\"server_error\",\"message\":\"upstream boom\"}}\n\n"
    );
    server
        .mock("POST", "/v1/responses")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse_body)
        .create();

    let client = build_client(&server.url());
    let mut stream = client
        .responses_stream(&responses_request())
        .await
        .expect("流式响应");

    let first = stream.next().await.expect("created 事件").expect("解析");
    assert_eq!(first.event_type, "response.created");

    let err = stream
        .next()
        .await
        .expect("error 事件不得被静默下发/丢弃")
        .unwrap_err();
    assert!(
        expect_stream_error_message(&err).contains("upstream boom"),
        "错误消息应取 message 字段: {err:?}"
    );
}

#[tokio::test]
async fn responses_type_error_without_event_name_yields_stream_error() {
    let mut server = mockito::Server::new_async().await;
    let sse_body =
        "data: {\"type\":\"error\",\"code\":\"rate_limit\",\"message\":\"slow down\"}\n\n";
    server
        .mock("POST", "/v1/responses")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create();

    let client = build_client(&server.url());
    let mut stream = client
        .responses_stream(&responses_request())
        .await
        .expect("流式响应");

    let err = stream.next().await.expect("must yield error").unwrap_err();
    assert!(expect_stream_error_message(&err).contains("slow down"));
}
