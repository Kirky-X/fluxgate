use crate::sdk::client::NexusFlowClientBuilder;
use crate::sdk::error::NexusFlowSdkError;
use crate::sdk::ChatRequest;

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
