use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::stream::ChatStream;
use crate::sdk::ChatRequest;
use bytes::Bytes;
use futures::StreamExt;
use std::time::Duration;

#[tokio::test]
async fn stream_decodes_chunks_and_filters_done() {
    let frame1 = Bytes::from(
        "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n".to_string(),
    );
    let frame2 = Bytes::from(
        "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\" there\"}}]}\n\n".to_string(),
    );
    let frame3 = Bytes::from("data: [DONE]\n\n".to_string());

    let byte_stream = futures::stream::iter(vec![
        Ok::<bytes::Bytes, reqwest::Error>(frame1),
        Ok::<bytes::Bytes, reqwest::Error>(frame2),
        Ok::<bytes::Bytes, reqwest::Error>(frame3),
    ]);

    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    let mut collected = Vec::new();
    while let Some(chunk) = chat_stream.next().await {
        collected.push(chunk.unwrap());
    }

    assert_eq!(collected.len(), 2, "[DONE] frame should be filtered");
    assert_eq!(collected[0].id, "1");
    assert_eq!(
        collected[0].delta.as_ref().unwrap().content.as_deref(),
        Some("Hi")
    );
    assert_eq!(
        collected[1].delta.as_ref().unwrap().content.as_deref(),
        Some(" there")
    );

    let combined: String = collected
        .iter()
        .filter_map(|c| c.delta.as_ref())
        .filter_map(|d| d.content.clone())
        .collect();
    assert_eq!(combined, "Hi there");
}

#[tokio::test]
async fn stream_propagates_parse_error() {
    let bad_frame = Bytes::from("data: not valid json\n\n".to_string());

    let byte_stream = futures::stream::iter(vec![Ok::<bytes::Bytes, reqwest::Error>(bad_frame)]);

    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    let result = chat_stream.next().await.unwrap();

    assert!(result.is_err());
}

#[tokio::test]
async fn stream_empty_input_yields_nothing() {
    let byte_stream = futures::stream::iter(Vec::<Result<bytes::Bytes, reqwest::Error>>::new());
    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    assert!(chat_stream.next().await.is_none());
}

/// 裸 TCP SSE 服务器（mockito 1.7.2 无分块延迟 API）：两帧 SSE，
/// 帧间隔 `gap`。返回 base_url。
async fn spawn_chunked_sse_server(gap: Duration) -> String {
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
        sock.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
        let frames: [&[u8]; 2] = [
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n",
        ];
        for (i, frame) in frames.iter().enumerate() {
            if i == 1 {
                tokio::time::sleep(gap).await;
            }
            sock.write_all(format!("{:x}\r\n", frame.len()).as_bytes())
                .await
                .unwrap();
            sock.write_all(frame).await.unwrap();
            sock.write_all(b"\r\n").await.unwrap();
        }
        sock.write_all(b"0\r\n\r\n").await.unwrap();
    });
    format!("http://{addr}")
}

/// 流式主路径不得被客户端总超时切断：帧间隔（300ms）远大于 builder
/// 总超时（100ms），SSE 长流必须继续收帧（总超时若作用于流式 body
/// 读取，LLM 长回复会在默认 30s 处被截断——reqwest 总超时按整个
/// body 计且不按 chunk 重置）。stall 由 read_timeout 看护。
#[tokio::test]
async fn stream_survives_beyond_total_timeout() {
    let base_url = spawn_chunked_sse_server(Duration::from_millis(300)).await;
    let client = FluxgateClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let mut stream = client
        .chat_stream(&ChatRequest::new("gpt-4", "hi"))
        .await
        .expect("streaming must not be bound by the total request timeout");

    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.push(chunk.expect("chunk should decode"));
    }
    let text: String = collected
        .iter()
        .filter_map(|c| c.delta.as_ref())
        .filter_map(|d| d.content.clone())
        .collect();
    assert_eq!(text, "ab", "两帧都必须在总超时之外送达");
}

/// read_timeout 停滞看护的负向锁定：帧间停滞（1s）远超 read_timeout
///（200ms）时，流必须报错终止而不是无限悬挂。总超时不约束流式（上一
/// 条测试），「连接停滞被切断」的承诺由 read_timeout 承担。
#[tokio::test]
async fn stream_errors_when_stall_exceeds_read_timeout() {
    let base_url = spawn_chunked_sse_server(Duration::from_secs(1)).await;
    let client = FluxgateClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .read_timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    let mut stream = client
        .chat_stream(&ChatRequest::new("gpt-4", "hi"))
        .await
        .expect("首帧前无停滞，响应应正常建立");

    let first = stream
        .next()
        .await
        .expect("流不应在首帧前终止")
        .expect("首帧应在停滞前正常解码");
    assert_eq!(first.delta.as_ref().unwrap().content.as_deref(), Some("a"));

    // 第二帧前服务器停滞 1s > read_timeout 200ms：必须及时报错。
    // 外层 5s 兜底仅防测试悬挂，断言目标是 read_timeout 切断本身。
    let second = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("read_timeout 必须切断停滞，不能拖到外层兜底");
    let err = second.expect("停滞切断后流应以错误帧收尾").unwrap_err();
    assert!(
        matches!(err, FluxgateSdkError::Stream { .. }),
        "停滞切断应以上游读取错误呈现，实际: {err:?}"
    );
}

/// 审查 LOW：网关心跳帧（`{":":""}` 注释帧）不产出空 ChatChunk
#[tokio::test]
async fn heartbeat_frame_is_filtered() {
    // 心跳帧 = 注释块 `{":":""}`（json! 构造防手写转义歧义）
    let heartbeat = Bytes::from(format!("data: {}\n\n", serde_json::json!({":": ""})).to_string());
    let content = Bytes::from(
        "data: {\"id\":\"chunk-0\",\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n".to_string(),
    );
    let byte_stream = futures::stream::iter(vec![
        Ok::<bytes::Bytes, reqwest::Error>(heartbeat),
        Ok::<bytes::Bytes, reqwest::Error>(content),
    ]);
    let mut stream = ChatStream::from_bytes_stream(byte_stream);
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(
        first.delta.as_ref().unwrap().content.as_deref(),
        Some("x"),
        "心跳帧被过滤，内容帧正常产出"
    );
    assert!(stream.next().await.is_none());
}
