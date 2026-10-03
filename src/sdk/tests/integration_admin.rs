use crate::sdk::client::NexusFlowClientBuilder;
use crate::sdk::error::NexusFlowSdkError;
use std::time::Duration;

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

/// 慢响应裸 TCP 服务器（mockito 无延迟 API）：按请求路径回对应 JSON
/// body，但每个连接都先停滞 `delay` 再响应。
async fn spawn_slow_admin_server(delay: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => break,
            };
            let mut buf = [0u8; 4096];
            let mut got = Vec::new();
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
                if got.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let path = got
                .split(|b| *b == b' ')
                .nth(1)
                .unwrap_or_default()
                .to_vec();
            let body: &[u8] = if path.ends_with(b"/v1/models") {
                br#"{"data":[]}"#
            } else if path.ends_with(b"/admin/vendors") {
                br#"{"vendors":[]}"#
            } else {
                br#"{"status":"ok","version":"test","vendors":0}"#
            };
            tokio::time::sleep(delay).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// admin 请求族必须有总时长上界：服务器停滞（500ms）超过客户端总超时
///（100ms）时，三个 admin 方法都应 promptly 报 Network 超时，而不是
/// 无限悬挂。流式请求不受总超时约束（由 read_timeout 看护），admin /
/// chat / embed 非流式逐请求携带总超时。
#[tokio::test]
async fn admin_requests_bounded_by_total_timeout() {
    let base_url = spawn_slow_admin_server(Duration::from_millis(500)).await;
    let client = NexusFlowClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let models_err = tokio::time::timeout(Duration::from_secs(5), client.models())
        .await
        .expect("总超时必须切断 admin 停滞（不能拖到外层兜底）")
        .unwrap_err();
    match models_err {
        NexusFlowSdkError::Network { source } => {
            assert!(source.is_timeout(), "models 应被总超时切断: {source}");
        }
        other => panic!("models 应报 Network 超时，实际: {other:?}"),
    }

    let vendors_err = tokio::time::timeout(Duration::from_secs(5), client.vendors())
        .await
        .expect("vendors 未被总超时切断")
        .unwrap_err();
    assert!(
        matches!(vendors_err, NexusFlowSdkError::Network { .. }),
        "vendors 应报 Network 超时，实际: {vendors_err:?}"
    );

    let health_err = tokio::time::timeout(Duration::from_secs(5), client.health())
        .await
        .expect("health 未被总超时切断")
        .unwrap_err();
    assert!(
        matches!(health_err, NexusFlowSdkError::Network { .. }),
        "health 应报 Network 超时，实际: {health_err:?}"
    );
}
