#![cfg(feature = "web-server")]

// SSE 编码器与增量分帧器单元测试（recId=3 阶段一）

use futures::StreamExt;
use nexusflow::api::sse::{
    encode_sse_done, encode_sse_error, encode_sse_event, reframe_passthrough_stream, SseFrame,
    SseFrameParser,
};
use std::time::Duration;

// ==================== 编码器（既有行为回归） ====================

#[test]
fn encodes_plain_data_without_event() {
    let out = encode_sse_event("hello", None);
    assert_eq!(out, "data: hello\n\n");
}

#[test]
fn encodes_typed_event_with_chunk() {
    let out = encode_sse_event("{\"x\":1}", Some("chunk"));
    assert_eq!(out, "event: chunk\ndata: {\"x\":1}\n\n");
}

#[test]
fn encodes_done_marker() {
    assert_eq!(encode_sse_done(), "data: [DONE]\n\n");
}

#[test]
fn encodes_error_event_contains_event_and_data() {
    let out = encode_sse_error("boom");
    assert!(out.contains("event: error"));
    assert!(out.contains("data: boom"));
}

#[test]
fn encodes_error_event_terminates_with_double_newline() {
    let out = encode_sse_error("oops");
    assert!(out.ends_with("\n\n"));
}

// ==================== SseFrameParser（recId=3 阶段一） ====================

/// ① `data: {"a":1}` 劈两半喂入 → 恰出一完整帧。
///
/// 上游网络块边界劈在 JSON 字面量中间：分帧器跨块缓冲，凑齐定界符
/// `\n\n` 才产出，帧内容完整无缺。
#[test]
fn frame_split_across_feed_chunks_yields_one_complete_frame() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: {\"a\":");
    // 第一半无定界符 → 不产出帧
    assert!(parser.next_frame().is_none());

    parser.feed(b"1}\n\n");
    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1, "劈两半喂入应恰出一帧");
    assert_eq!(frames[0].data.as_deref(), Some("{\"a\":1}"));
    assert_eq!(frames[0].raw, "data: {\"a\":1}");
    assert_eq!(frames[0].event, None);
}

/// ② 四字节 UTF-8 字符（😀 = F0 9F 98 80）劈在块边界 → 还原原文无 U+FFFD。
///
/// 旧实现逐块 `from_utf8_lossy` 会把劈开的两个半截各变成 U+FFFD；
/// 分帧器在帧完整后才整体解码，跨块多字节序列自然拼回。
#[test]
fn multibyte_utf8_split_at_chunk_boundary_no_replacement_char() {
    let full = "data: hello \u{1F600} world\n\n".to_string();
    let bytes = full.as_bytes();
    // 劈在四字节序列中间：F0 9F | 98 80
    let emoji_pos = full.find('\u{1F600}').unwrap();
    let split = emoji_pos + 1; // 字符首字节之后

    let mut parser = SseFrameParser::new();
    parser.feed(&bytes[..split]);
    parser.feed(&bytes[split..]);

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0].data.as_deref(),
        Some("hello \u{1F600} world"),
        "跨块 UTF-8 序列应完整还原"
    );
    assert!(
        !frames[0].raw.contains('\u{FFFD}'),
        "不得出现 U+FFFD 替换符: {:?}",
        frames[0].raw
    );
}

/// ③ 单块多事件（`data: x\n\ndata: y\n\n`）→ 两帧。
#[test]
fn single_chunk_multiple_events_yield_multiple_frames() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: x\n\ndata: y\n\n");

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].data.as_deref(), Some("x"));
    assert_eq!(frames[1].data.as_deref(), Some("y"));
    assert!(parser.is_empty(), "消费完后 buffer 应为空");
}

/// ④ CRLF 变体：`\r\n` 行尾 + `\r\n\r\n` 帧定界。
#[test]
fn crlf_line_endings_and_delimiters() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: a\r\n\r\ndata: b\r\n\r\n");

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 2, "CRLF 定界应产出两帧");
    assert_eq!(frames[0].data.as_deref(), Some("a"));
    assert_eq!(frames[1].data.as_deref(), Some("b"));
}

/// ④ 补充：CRLF 定界符本身劈在两块之间（`\r\n` | `\r\n`）。
#[test]
fn crlf_delimiter_split_across_chunks() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: z\r\n");
    assert!(parser.next_frame().is_none(), "定界未完整不应产出");
    parser.feed(b"\r\n");

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data.as_deref(), Some("z"));
}

/// event 字段与 data 同时解析；注释行与 id/retry 字段跳过。
#[test]
fn event_field_parsed_and_comments_skipped() {
    let mut parser = SseFrameParser::new();
    parser.feed(b": keepalive comment\nid: 42\nevent: chunk\ndata: payload\nretry: 100\n\n");

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].event.as_deref(), Some("chunk"));
    assert_eq!(frames[0].data.as_deref(), Some("payload"));
}

/// 多行 data 以 \n 连接（SSE 规范）。
#[test]
fn multiple_data_lines_joined_with_newline() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: line1\ndata: line2\n\n");

    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data.as_deref(), Some("line1\nline2"));
}

/// 无尾随定界的残余帧由 flush 冲出（对齐 EventSource EOF 语义）。
#[test]
fn flush_emits_trailing_incomplete_frame() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: tail");
    assert!(parser.next_frame().is_none());
    assert!(parser.flush().is_some(), "EOF 残余应由 flush 产出");
    assert!(parser.flush().is_none(), "flush 后应为空");
}

/// 空 data 行帧（`: comment` 仅注释）→ raw 保留但 data 为 None。
#[test]
fn comment_only_frame_has_no_data() {
    let mut parser = SseFrameParser::new();
    parser.feed(b": ping\n\n");
    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, None);
}

// ==================== 错误帧检测（recId=14② 依赖） ====================

#[test]
fn error_event_frame_detected() {
    let mut parser = SseFrameParser::new();
    parser.feed(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n",
    );
    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].is_error());
}

#[test]
fn error_data_frame_without_event_field_detected() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: {\"error\":{\"message\":\"boom\"}}\n\n");
    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].is_error());
}

#[test]
fn normal_data_frame_not_error() {
    let mut parser = SseFrameParser::new();
    parser.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
    let frames = collect_frames(&mut parser);
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].is_error());
}

#[test]
fn plain_text_error_event_detected() {
    let frame = SseFrame {
        raw: "event: error\ndata: upstream blew up".into(),
        event: Some("error".into()),
        data: Some("upstream blew up".into()),
    };
    assert!(frame.is_error());
}

// ==================== reframe_passthrough_stream 集成 ====================

fn collect_frames(parser: &mut SseFrameParser) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    while let Some(frame) = parser.next_frame() {
        frames.push(frame);
    }
    frames
}

fn sse_bytes_stream(
    chunks: Vec<Result<&'static [u8], std::io::Error>>,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    futures::stream::iter(
        chunks
            .into_iter()
            .map(|r| r.map(bytes::Bytes::from_static))
            .collect::<Vec<_>>(),
    )
}

async fn drain_reframed(chunks: Vec<Result<&'static [u8], std::io::Error>>) -> String {
    let mut out = String::new();
    let mut stream = Box::pin(reframe_passthrough_stream(sse_bytes_stream(chunks)));
    while let Some(item) = stream.next().await {
        let bytes = item.expect("reframed chunk should be Ok");
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

/// 透传重帧：劈块喂入的帧逐帧原样转发，帧边界与上游一致（消除旧实现的
/// 整块 `data:` 嵌套包装）。
#[tokio::test]
async fn reframe_passthrough_forwards_frames_verbatim() {
    let out = drain_reframed(vec![
        Ok(b"data: {\"id\":1}\n\nda"),
        Ok(b"ta: {\"id\":2}\n\n"),
    ])
    .await;

    assert_eq!(out, "data: {\"id\":1}\n\ndata: {\"id\":2}\n\n");
}

/// 透传重帧：UTF-8 多字节劈块不产生 U+FFFD。
#[tokio::test]
async fn reframe_passthrough_utf8_safe() {
    let frame = "data: weather \u{1F325}\n\n".to_string();
    let bytes = bytes::Bytes::from(frame.clone());
    let split = 8;

    let mut out = String::new();
    let upstream = futures::stream::iter(vec![
        Ok::<_, std::io::Error>(bytes.slice(..split)),
        Ok(bytes.slice(split..)),
    ]);
    let mut stream = Box::pin(reframe_passthrough_stream(upstream));
    while let Some(item) = stream.next().await {
        out.push_str(&String::from_utf8_lossy(&item.unwrap()));
    }
    assert_eq!(out, frame, "透传应与上游逐字节一致");
    assert!(!out.contains('\u{FFFD}'));
}

/// recId=14②：上游 `event: error` 帧 → 该帧转发后流提前终止，
/// 后续帧不再透传。
#[tokio::test]
async fn reframe_passthrough_terminates_on_error_frame() {
    let out = drain_reframed(vec![
        Ok(b"data: {\"delta\":{\"content\":\"hi\"}}\n\n"),
        Ok(b"event: error\ndata: {\"error\":{\"message\":\"overloaded\"}}\n\n"),
        Ok(b"data: {\"delta\":{\"content\":\"never\"}}\n\n"),
    ])
    .await;

    assert!(out.contains("overloaded"), "错误帧应转发");
    assert!(!out.contains("never"), "错误帧之后的帧不得透传");
    assert!(!out.contains("[DONE]"), "错误终止不得补正常结束标记");
}

/// recId=14②：上游 IO 中断 → 带语义的 SSE error 事件，而非静默截断。
#[tokio::test]
async fn reframe_passthrough_converts_io_error_to_sse_error_event() {
    let out = drain_reframed(vec![
        Ok(b"data: {\"ok\":true}\n\n"),
        Err(std::io::Error::other("connection reset")),
    ])
    .await;

    assert!(out.contains("event: error"), "应产出 error 事件: {out}");
    assert!(out.contains("connection reset"));
    assert!(!out.contains("[DONE]"));
}

/// 上游省略末尾定界空行时，残余帧仍被冲出转发。
#[tokio::test]
async fn reframe_passthrough_flushes_trailing_frame() {
    let out = drain_reframed(vec![Ok(b"data: last-frame-without-delimiter")]).await;
    assert_eq!(out, "data: last-frame-without-delimiter\n\n");
}

/// 上游超时不挂死：EOF 空流立即正常结束。
#[tokio::test]
async fn reframe_passthrough_empty_stream_ends_promptly() {
    let started = std::time::Instant::now();
    let out = drain_reframed(vec![]).await;
    assert_eq!(out, "");
    assert!(started.elapsed() < Duration::from_secs(1), "空流应立即结束");
}

// ==================== 覆盖补缺：SDK 客户端（chat / chat_stream / admin / builder） ====================
//
// src/sdk/tests/ 下的单元测试不走集成目标，这里从 tests/ 集成侧用 mockito
// 锁定同一批 wire 行为：响应解析（含缺失 choices 的协议错误）、路由 hint
// header、强制 stream=true、URL join 失败、read_timeout 停滞拦截与
// Debug 脱敏。

#[cfg(all(test, feature = "sdk"))]
mod sdk_client_coverage {
    use futures::StreamExt;
    use nexusflow::sdk::{
        ChatRequest, ChatRole, NexusFlowClient, NexusFlowClientBuilder, NexusFlowSdkError,
        RequestConfig,
    };
    use std::time::Duration;

    fn build_client(base_url: &str) -> NexusFlowClient {
        NexusFlowClientBuilder::new()
            .base_url(base_url)
            .api_key("test-key")
            .build()
            .unwrap()
    }

    // T008：网关真实形状——无 vendor 键（response_to_external 不产出）
    const CHAT_OK_BODY: &str = r#"{"id":"chatcmpl-1","model":"gpt-4","choices":[{"message":{"role":"assistant","content":"Hi there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;

    // -------- builder 构造与 Debug 脱敏 --------

    /// 缺 base_url / URL 非法 / 缺 api_key → Config 错误显性化，不 panic。
    #[test]
    fn builder_rejects_incomplete_or_invalid_configuration() {
        let err = NexusFlowClientBuilder::new()
            .api_key("k")
            .build()
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowSdkError::Config { .. }),
            "缺 base_url"
        );

        let err = NexusFlowClientBuilder::new()
            .base_url("::not a url::")
            .api_key("k")
            .build()
            .unwrap_err();
        match err {
            NexusFlowSdkError::Config { message } => {
                assert!(
                    message.contains("invalid base_url"),
                    "非法 URL 须指明: {message}"
                );
            }
            other => panic!("expected Config error, got {other:?}"),
        }

        let err = NexusFlowClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .build()
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowSdkError::Config { .. }),
            "缺 api_key"
        );
    }

    /// api_key 是 Bearer 凭证：Debug 输出必须脱敏为 [REDACTED]。
    #[test]
    fn debug_output_redacts_api_key() {
        let client = build_client_with("http://127.0.0.1:8080", "super-secret-bearer-key");
        let rendered = format!("{client:?}");
        assert!(
            !rendered.contains("super-secret-bearer-key"),
            "Debug 泄露 api_key: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "应显式标注脱敏: {rendered}"
        );
    }

    fn build_client_with(base_url: &str, key: &str) -> NexusFlowClient {
        NexusFlowClientBuilder::new()
            .base_url(base_url)
            .api_key(key)
            .build()
            .unwrap()
    }

    // -------- chat 非流式 --------

    /// chat 成功：字段逐一映射（id/model/content/usage/vendor/finish_reason）。
    #[tokio::test]
    async fn chat_success_maps_all_response_fields() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(CHAT_OK_BODY)
            .create_async()
            .await;

        let resp = build_client(&server.url())
            .chat(&ChatRequest::new("gpt-4", "hello"))
            .await
            .unwrap();
        assert_eq!(resp.id, "chatcmpl-1");
        assert_eq!(resp.model, "gpt-4");
        assert_eq!(resp.content, "Hi there");
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        // T008：网关响应无 vendor 键（旧 SDK vendor 字段恒空误导，已移除）
        assert_eq!(
            resp.finish_reason,
            nexusflow::sdk::ChatFinishReason::Standard(nexusflow::sdk::StandardFinishReason::Stop)
        );
    }

    /// 路由 hint 进 header（X-Vendor / X-Model）、Bearer 凭证与
    /// default_config 合并语义：请求级 max_tokens 覆盖、默认级 user 补缺。
    #[tokio::test]
    async fn chat_sends_hints_auth_and_merged_config() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer test-key")
            .match_header("x-vendor", "anthropic_main")
            .match_header("x-model", "claude-3")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "gpt-4",
                "max_tokens": 50,
                "user": "sdk-user"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(CHAT_OK_BODY)
            .create_async()
            .await;

        let client = NexusFlowClientBuilder::new()
            .base_url(server.url())
            .api_key("test-key")
            .default_config(RequestConfig {
                max_tokens: Some(999),
                user: Some("sdk-user".into()),
                ..Default::default()
            })
            .build()
            .unwrap();
        let req = ChatRequest {
            model: "gpt-4".into(),
            messages: vec![nexusflow::sdk::ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                tool_calls: None,
            }],
            config: RequestConfig {
                max_tokens: Some(50),
                vendor_hint: Some("anthropic_main".into()),
                model_hint: Some("claude-3".into()),
                ..Default::default()
            },
            tools: Vec::new(),
            tool_choice: None,
            vendor_extensions: Default::default(),
        };
        client.chat(&req).await.unwrap();
        mock.assert_async().await;
    }

    /// 401 → Auth 错误；其他非 2xx → Server{status, 响应原文}。
    #[tokio::test]
    async fn chat_maps_error_statuses_to_typed_errors() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .create_async()
            .await;
        let err = build_client(&server.url())
            .chat(&ChatRequest::new("gpt-4", "hi"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowSdkError::Auth { .. }),
            "401 必须映射 Auth"
        );

        server
            .mock("POST", "/v1/chat/completions")
            .with_status(503)
            .with_body("upstream overloaded")
            .create_async()
            .await;
        let err = build_client(&server.url())
            .chat(&ChatRequest::new("gpt-4", "hi"))
            .await
            .unwrap_err();
        match err {
            NexusFlowSdkError::Server { status, message } => {
                assert_eq!(status, 503);
                assert_eq!(message, "upstream overloaded", "响应原文必须透传");
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    /// 协议错误：缺 choices 数组 / 空 choices 数组 → Protocol 错误，
    /// 不得静默产出空响应。
    #[tokio::test]
    async fn chat_reports_protocol_errors_on_malformed_payload() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"x","model":"m"}"#)
            .create_async()
            .await;
        let err = build_client(&server.url())
            .chat(&ChatRequest::new("m", "hi"))
            .await
            .unwrap_err();
        match err {
            NexusFlowSdkError::Protocol { message } => {
                assert_eq!(message, "missing choices array");
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }

        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"x","choices":[]}"#)
            .create_async()
            .await;
        let err = build_client(&server.url())
            .chat(&ChatRequest::new("m", "hi"))
            .await
            .unwrap_err();
        match err {
            NexusFlowSdkError::Protocol { message } => {
                assert_eq!(message, "empty choices array");
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    /// cannot-be-a-base 的 base_url（mailto:）在 join 请求路径时失败 →
    /// Config 错误显性化（构造期 URL 合法、拼接期失败的分界）。
    #[tokio::test]
    async fn chat_join_failure_on_non_base_url_is_config_error() {
        let client = build_client("mailto:user@example.com");
        let err = client.chat(&ChatRequest::new("m", "hi")).await.unwrap_err();
        match err {
            NexusFlowSdkError::Config { message } => {
                assert!(
                    message.contains("invalid URL"),
                    "须指明 URL 拼接失败: {message}"
                );
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    // -------- chat_stream 流式 --------

    /// chat_stream 成功：SSE 分帧解码为 chunk、[DONE] 过滤、wire body 强制
    /// stream=true（请求未设也强制）、hint header 照带。
    #[tokio::test]
    async fn chat_stream_decodes_chunks_until_done_marker() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("x-vendor", "openai_main")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({ "stream": true }),
            ))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"He\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"llo\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ))
            .create_async()
            .await;

        let client = build_client(&server.url());
        let req = ChatRequest {
            model: "gpt-4".into(),
            messages: vec![nexusflow::sdk::ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                tool_calls: None,
            }],
            config: RequestConfig {
                vendor_hint: Some("openai_main".into()),
                ..Default::default()
            },
            tools: Vec::new(),
            tool_choice: None,
            vendor_extensions: Default::default(),
        };
        let mut stream = client.chat_stream(&req).await.unwrap();

        let first = stream.next().await.expect("首 chunk").unwrap();
        assert_eq!(first.id, "c1");
        let delta = first.delta.expect("首 chunk 带 delta");
        assert_eq!(delta.role, Some(ChatRole::Assistant));
        assert_eq!(delta.content.as_deref(), Some("He"));
        assert!(first.finish_reason.is_none());

        let second = stream.next().await.expect("次 chunk").unwrap();
        assert_eq!(
            second.delta.and_then(|d| d.content),
            Some("llo".to_string())
        );
        assert_eq!(
            second.finish_reason,
            Some(nexusflow::sdk::ChatFinishReason::Standard(
                nexusflow::sdk::StandardFinishReason::Stop
            ))
        );

        assert!(stream.next().await.is_none(), "[DONE] 之后的流必须正常结束");
        mock.assert_async().await;
    }

    /// chat_stream 错误路径：401 → Auth；500 → Server；mailto base → Config。
    /// （ChatStream 无 Debug，错误分支用 match 而非 unwrap_err。）
    #[tokio::test]
    async fn chat_stream_maps_error_paths() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .create_async()
            .await;
        match build_client(&server.url())
            .chat_stream(&ChatRequest::new("m", "hi"))
            .await
        {
            Err(NexusFlowSdkError::Auth { .. }) => {}
            Err(other) => panic!("expected Auth error, got {other:?}"),
            Ok(_) => panic!("401 必须报错而非返回流"),
        }

        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;
        match build_client(&server.url())
            .chat_stream(&ChatRequest::new("m", "hi"))
            .await
        {
            Err(NexusFlowSdkError::Server { status, message }) => {
                assert_eq!(status, 500);
                assert_eq!(message, "boom");
            }
            Err(other) => panic!("expected Server error, got {other:?}"),
            Ok(_) => panic!("500 必须报错而非返回流"),
        }

        match build_client("mailto:user@example.com")
            .chat_stream(&ChatRequest::new("m", "hi"))
            .await
        {
            Err(NexusFlowSdkError::Config { .. }) => {}
            Err(other) => panic!("expected Config error, got {other:?}"),
            Ok(_) => panic!("非法 base_url 必须报错而非返回流"),
        }
    }

    // -------- admin / models / health --------

    /// models / vendors / health 成功路径：响应结构逐一反序列化。
    #[tokio::test]
    async fn admin_endpoints_parse_success_payloads() {
        let mut server = mockito::Server::new_async().await;

        // T008 失配修复：GET /admin/models（目录九字段，原 /v1/models 无路由）
        server
            .mock("GET", "/admin/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"model_id":"gpt-4","context_window":128000,"input_price":30.0,
                    "output_price":60.0,"currency":"usd","supports_tools":true}]"#,
            )
            .create_async()
            .await;
        let models = build_client(&server.url()).models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "gpt-4");
        assert_eq!(models[0].context_window, 128_000);

        // 真实形状：{id,status,queue_length,key_pool?}
        server
            .mock("GET", "/admin/vendors")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"id":"v1","status":"Available","queue_length":0},
                    {"id":"v2","status":"Throttled","queue_length":2}]"#,
            )
            .create_async()
            .await;
        let vendors = build_client(&server.url()).vendors().await.unwrap();
        assert_eq!(vendors.len(), 2);
        assert_eq!(vendors[0].status, "Available");
        assert_eq!(vendors[1].status, "Throttled");
        assert_eq!(vendors[1].queue_length, 2);

        // T008 失配修复：GET /api/v1/health（admin/health 不存在）
        server
            .mock("GET", "/api/v1/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"status":"ok","version":"0.1.0","timestamp":"2026-10-05T00:00:00+00:00"}"#,
            )
            .create_async()
            .await;
        let health = build_client(&server.url()).health().await.unwrap();
        assert_eq!(health.status, "ok");
        assert_eq!(health.version, "0.1.0");
        assert_eq!(health.timestamp, "2026-10-05T00:00:00+00:00");
    }

    /// admin 端点非 2xx → Server{status, 响应原文}（models / health 代表）。
    #[tokio::test]
    async fn admin_endpoints_map_error_statuses() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/admin/models")
            .with_status(503)
            .with_body("unavailable")
            .create_async()
            .await;
        let err = build_client(&server.url()).models().await.unwrap_err();
        match err {
            NexusFlowSdkError::Server { status, message } => {
                assert_eq!(status, 503);
                assert_eq!(message, "unavailable");
            }
            other => panic!("expected Server error, got {other:?}"),
        }

        server
            .mock("GET", "/api/v1/health")
            .with_status(500)
            .with_body("db down")
            .create_async()
            .await;
        let err = build_client(&server.url()).health().await.unwrap_err();
        match err {
            NexusFlowSdkError::Server { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "db down");
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    /// admin 端点在 cannot-be-a-base URL 上 join 失败 → Config 错误
    ///（models / vendors / health 三处拼接点）。
    #[tokio::test]
    async fn admin_join_failure_on_non_base_url_is_config_error() {
        let client = build_client("mailto:user@example.com");
        for err in [
            client.models().await.unwrap_err(),
            client.vendors().await.unwrap_err(),
            client.health().await.unwrap_err(),
        ] {
            match err {
                NexusFlowSdkError::Config { message } => {
                    assert!(message.contains("invalid URL"), "{message}");
                }
                other => panic!("expected Config error, got {other:?}"),
            }
        }
    }

    // -------- 读超时（连接停滞拦截） --------

    /// 裸 TCP 服务器：accept 后延迟 delay 才回（半）响应。返回 base_url。
    async fn spawn_delayed_server(delay: Duration) -> String {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(delay).await;
            let body = "{}";
            let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
            stream.write_all(head.as_bytes()).await.ok();
            stream.write_all(body.as_bytes()).await.ok();
        });
        format!("http://{addr}")
    }

    /// read_timeout 是「按读停滞」看护：服务端迟迟不回字节，超过客户端
    /// read_timeout 即 Network 错误（而非无限悬挂），也不受默认 30s 拖累。
    #[tokio::test]
    async fn read_timeout_intercepts_stalled_connection() {
        let base = spawn_delayed_server(Duration::from_millis(1_500)).await;
        let client = NexusFlowClientBuilder::new()
            .base_url(&base)
            .api_key("k")
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap();

        let started = std::time::Instant::now();
        let err = client.chat(&ChatRequest::new("m", "hi")).await.unwrap_err();
        match err {
            NexusFlowSdkError::Network { .. } => {}
            other => panic!("expected Network error, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "停滞必须在 read_timeout 量级被拦截: {:?}",
            started.elapsed()
        );
    }
}
