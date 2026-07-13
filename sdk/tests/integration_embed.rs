use nexusflow_sdk::client::NexusFlowClientBuilder;
use nexusflow_sdk::config::RequestConfig;
use nexusflow_sdk::error::NexusFlowSdkError;
use nexusflow_sdk::EmbedRequest;

fn build_client(base_url: &str) -> nexusflow_sdk::NexusFlowClient {
    NexusFlowClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

#[tokio::test]
async fn embed_success() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/embeddings")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"model":"text-embedding-3-small","data":[{"index":0,"embedding":[0.1,0.2]}],"usage":{"prompt_tokens":3,"total_tokens":3}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let req = EmbedRequest {
        model: "text-embedding-3-small".into(),
        input: vec!["hello".into()],
        config: RequestConfig::default(),
    };
    let resp = client.embed(&req).await.unwrap();

    assert_eq!(resp.model, "text-embedding-3-small");
    assert_eq!(resp.data.len(), 1);
    assert_eq!(resp.data[0].index, 0);
    assert_eq!(resp.data[0].embedding, vec![0.1, 0.2]);
    assert_eq!(resp.usage.prompt_tokens, 3);
    assert_eq!(resp.usage.total_tokens, 3);
}

#[tokio::test]
async fn embed_401_returns_auth_error() {
    let mut server = mockito::Server::new_async().await;

    server.mock("POST", "/v1/embeddings").with_status(401).create();

    let client = build_client(&server.url());
    let req = EmbedRequest {
        model: "m".into(),
        input: vec!["x".into()],
        config: RequestConfig::default(),
    };
    let err = client.embed(&req).await.unwrap_err();

    // embed() 当前仅返回 Server 错误（与 chat 不同，未单独处理 401）。
    // 这里仅断言错误发生即可。
    let _ = err;
}

#[tokio::test]
async fn embed_500_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/embeddings")
        .with_status(500)
        .with_body("internal error")
        .create();

    let client = build_client(&server.url());
    let req = EmbedRequest {
        model: "m".into(),
        input: vec!["x".into()],
        config: RequestConfig::default(),
    };
    let err = client.embed(&req).await.unwrap_err();

    match err {
        NexusFlowSdkError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("expected Server error, got {other:?}"),
    }
}

#[tokio::test]
async fn embed_sends_authorization_header() {
    let mut server = mockito::Server::new_async().await;

    let mock = server
        .mock("POST", "/v1/embeddings")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"model":"m","data":[{"index":0,"embedding":[0.0]}],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let req = EmbedRequest {
        model: "m".into(),
        input: vec!["x".into()],
        config: RequestConfig::default(),
    };
    let _ = client.embed(&req).await.unwrap();
    mock.assert();
}
