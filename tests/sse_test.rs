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
