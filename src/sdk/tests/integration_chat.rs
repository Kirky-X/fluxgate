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

/// 网关真实响应形状——无 vendor 字段（核实：response_to_external
/// 不产出），finish_reason 结构化，usage 含 prompt_tokens_details.cached_tokens
#[tokio::test]
async fn chat_success() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"chatcmpl-x","object":"chat.completion","model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7,"prompt_tokens_details":{"cached_tokens":3}}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let resp = client
        .chat(&ChatRequest::new("gpt-4", "hello"))
        .await
        .unwrap();

    assert_eq!(resp.content, "Hi");
    assert_eq!(resp.usage.prompt_tokens, 5);
    assert_eq!(resp.usage.completion_tokens, 2);
    assert_eq!(resp.usage.total_tokens, 7);
    assert_eq!(
        resp.usage.cache_read_tokens, 3,
        "cache_read 摘自 cached_tokens"
    );
    assert_eq!(
        resp.finish_reason,
        crate::sdk::ChatFinishReason::Standard(crate::sdk::StandardFinishReason::Stop)
    );
}

/// 模型发起工具环：非流式 message.tool_calls 解析（对齐网关
/// response_to_external 产物：无 index，function.name/arguments）
#[tokio::test]
async fn chat_parses_response_tool_calls() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"chatcmpl-t","model":"gpt-4","choices":[{"index":0,
                "message":{"role":"assistant","content":"",
                  "tool_calls":[{"id":"call_1","type":"function",
                                 "function":{"name":"get_weather","arguments":"{\"city\":\"SF\"}"}}]},
                "finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":20}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let resp = client
        .chat(&ChatRequest::new("gpt-4", "weather?"))
        .await
        .unwrap();

    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_1");
    assert_eq!(resp.tool_calls[0].function.name, "get_weather");
    assert!(resp.tool_calls[0].function.arguments.contains("SF"));
    assert_eq!(
        resp.finish_reason,
        crate::sdk::ChatFinishReason::Standard(crate::sdk::StandardFinishReason::ToolCalls)
    );
}

/// 工具面 wire：tools/tool_choice/vendor_extensions 进请求体（扩全）
#[tokio::test]
async fn chat_sends_tools_and_vendor_extensions() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "tools": [{"function": {"name": "get_weather"}}],
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
            "thinking_mode": "off"
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"x","model":"gpt-4","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let request = ChatRequest::new("gpt-4", "hello")
        .with_tools(
            vec![crate::sdk::ChatTool {
                function: crate::sdk::ChatToolDefinition {
                    name: "get_weather".into(),
                    description: None,
                    parameters: serde_json::json!({"type":"object"}),
                },
            }],
            Some(crate::sdk::ChatToolChoice::specific("get_weather")),
        )
        .with_vendor_extension("thinking_mode", serde_json::json!("off"))
        .unwrap();
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.content, "ok");

    mock.assert_async().await;
}

/// 多模态 content：Parts 数组 wire（图片 image_url 形状）
#[tokio::test]
async fn chat_sends_multimodal_parts() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
                ]
            }]
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"x","model":"gpt-4o","choices":[{"message":{"role":"assistant","content":"a cat"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let mut request = ChatRequest::new("gpt-4o", "");
    request.messages = vec![crate::sdk::ChatMessage::user(
        crate::sdk::ChatContent::Parts(vec![
            crate::sdk::ChatContentPart::text("what is this?"),
            crate::sdk::ChatContentPart::image_url("https://example.com/a.png"),
        ]),
    )];
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.content, "a cat");

    mock.assert_async().await;
}

/// tool 结果消息 wire：role=tool + tool_call_id
#[tokio::test]
async fn chat_sends_tool_result_message() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "messages": [
                {"role": "assistant", "tool_calls": [{"id": "call_1",
                    "function": {"name": "get_weather", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
            ]
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"x","model":"gpt-4","choices":[{"message":{"role":"assistant","content":"sunny!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let mut request = ChatRequest::new("gpt-4", "");
    request.messages = vec![
        crate::sdk::ChatMessage::assistant_with_tool_calls(
            crate::sdk::ChatContent::Text(String::new()),
            vec![crate::sdk::ChatToolCall {
                id: "call_1".into(),
                function: crate::sdk::ChatToolFunction {
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                },
            }],
        ),
        crate::sdk::ChatMessage::tool("call_1", "sunny"),
    ];
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.content, "sunny!");

    mock.assert_async().await;
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
