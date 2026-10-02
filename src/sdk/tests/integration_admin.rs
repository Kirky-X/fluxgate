use crate::sdk::client::NexusFlowClientBuilder;
use crate::sdk::error::NexusFlowSdkError;

fn build_client(base_url: &str) -> crate::sdk::NexusFlowClient {
    NexusFlowClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

#[tokio::test]
async fn models_returns_list() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":[{"id":"gpt-4","vendor":"openai"},{"id":"claude-3","vendor":"anthropic"}]}"#,
        )
        .create();

    let client = build_client(&server.url());
    let models = client.models().await.unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-4");
    assert_eq!(models[0].vendor, "openai");
    assert_eq!(models[1].id, "claude-3");
    assert_eq!(models[1].vendor, "anthropic");
}

#[tokio::test]
async fn vendors_returns_list() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/vendors")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"vendors":[{"id":"openai","name":"OpenAI","enabled":true},{"id":"anthropic","name":"Anthropic","enabled":false}]}"#,
        )
        .create();

    let client = build_client(&server.url());
    let vendors = client.vendors().await.unwrap();

    assert_eq!(vendors.len(), 2);
    assert_eq!(vendors[0].id, "openai");
    assert_eq!(vendors[0].name, "OpenAI");
    assert!(vendors[0].enabled);
    assert_eq!(vendors[1].id, "anthropic");
    assert_eq!(vendors[1].name, "Anthropic");
    assert!(!vendors[1].enabled);
}

#[tokio::test]
async fn health_returns_status() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/health")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","version":"0.2.0","vendors":3}"#)
        .create();

    let client = build_client(&server.url());
    let health = client.health().await.unwrap();

    assert_eq!(health.status, "ok");
    assert_eq!(health.version, "0.2.0");
    assert_eq!(health.vendors, 3);
}

#[tokio::test]
async fn models_500_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/v1/models")
        .with_status(500)
        .with_body("boom")
        .create();

    let client = build_client(&server.url());
    let err = client.models().await.unwrap_err();

    match err {
        NexusFlowSdkError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("expected Server error, got {other:?}"),
    }
}

#[tokio::test]
async fn vendors_401_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/vendors")
        .with_status(401)
        .create();

    let client = build_client(&server.url());
    let err = client.vendors().await.unwrap_err();

    // admin 端点统一返回 Server 错误（status 字段区分）
    match err {
        NexusFlowSdkError::Server { status, .. } => assert_eq!(status, 401),
        other => panic!("expected Server error, got {other:?}"),
    }
}
