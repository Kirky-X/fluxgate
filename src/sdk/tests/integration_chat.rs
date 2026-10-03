use crate::sdk::client::NexusFlowClientBuilder;
use crate::sdk::error::NexusFlowSdkError;
use crate::sdk::ChatRequest;
use std::time::Duration;

fn build_client(base_url: &str) -> crate::sdk::NexusFlowClient {
    NexusFlowClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

#[tokio::test]
async fn chat_success() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"chatcmpl-x","model":"gpt-4","choices":[{"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2},"vendor":"openai"}"#,
        )
        .create();

    let client = build_client(&server.url());
    let resp = client
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap();

    assert_eq!(resp.content, "Hi");
    assert_eq!(resp.vendor, "openai");
    assert_eq!(resp.prompt_tokens, 5);
    assert_eq!(resp.completion_tokens, 2);
    assert_eq!(resp.finish_reason, "stop");
}

#[tokio::test]
async fn chat_401_returns_auth_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/chat/completions")
        .with_status(401)
        .create();

    let client = build_client(&server.url());
    let err = client
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap_err();

    assert!(matches!(err, NexusFlowSdkError::Auth { .. }));
}

#[tokio::test]
async fn chat_500_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create();

    let client = build_client(&server.url());
    let err = client
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap_err();

    match err {
        NexusFlowSdkError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("expected Server error, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_sends_authorization_header() {
    let mut server = mockito::Server::new_async().await;

    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"x","model":"m","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1},"vendor":"v"}"#,
        )
        .create();

    let client = build_client(&server.url());
    let _ = client.chat(&ChatRequest::new("m", "hi")).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn builder_missing_base_url_fails() {
    let err = NexusFlowClientBuilder::new()
        .api_key("key")
        .build()
        .unwrap_err();
    assert!(matches!(err, NexusFlowSdkError::Config { .. }));
}

#[tokio::test]
async fn builder_missing_api_key_fails() {
    let err = NexusFlowClientBuilder::new()
        .base_url("http://localhost:8080")
        .build()
        .unwrap_err();
    assert!(matches!(err, NexusFlowSdkError::Config { .. }));
}

/// 裸 TCP HTTP 服务器：延迟 `delay` 后回单条 JSON 响应（mockito 1.7.2
/// 无延迟 API）。返回 base_url。
async fn spawn_delayed_json_server(delay: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let mut got = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..n]);
            if got.windows(4).any(|w| w == b"\r\n\r\n") || n == 0 {
                break;
            }
        }
        tokio::time::sleep(delay).await;
        let body = r#"{"id":"x","model":"m","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1},"vendor":"v"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{addr}")
}

/// `RequestConfig.timeout` 是非流式请求的逐请求总超时覆盖：
/// 覆盖值（1s）取代客户端默认（此处 300ms），慢网关（600ms）在覆盖
/// 窗口内完成——字段未接入执行路径时该请求会在 300ms 被杀。
#[tokio::test]
async fn request_config_timeout_overrides_client_default() {
    let base_url = spawn_delayed_json_server(Duration::from_millis(600)).await;
    let client = NexusFlowClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();

    let mut req = ChatRequest::new("m", "hi");
    req.config.timeout = Some(Duration::from_secs(1));
    client
        .chat(&req)
        .await
        .expect("config.timeout override must extend the window");
}

/// 覆盖也可收紧：客户端默认 1s，config.timeout 100ms → 慢响应（250ms）
/// 被逐请求窗口杀掉（证明覆盖真实生效，而非仅放宽）。
#[tokio::test]
async fn request_config_timeout_tightens_window() {
    let base_url = spawn_delayed_json_server(Duration::from_millis(250)).await;
    let client = NexusFlowClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let mut req = ChatRequest::new("m", "hi");
    req.config.timeout = Some(Duration::from_millis(100));
    assert!(
        client.chat(&req).await.is_err(),
        "config.timeout 覆盖收紧后必须提前终止"
    );
}
