// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! SSE（Server-Sent Events）编码与上游增量分帧。
//!
//! 实源头位于 protocol 模块（`vendor-management` 等 feature 不含
//! `web-server`，而 vendor 层分帧需要本能力；protocol 无 feature 门控）。
//! `api::sse` re-export 本模块保持既有引用路径。

use bytes::Bytes;
use futures::Stream;

use crate::i18n::t;

/// 编码普通 SSE 事件。
///
/// - `data` 为负载数据（必填）
/// - `event` 为可选事件类型（如 `chunk`、`error`）
///
/// 输出格式：
/// - 有 event：`event: <e>` + LF + `data: <data>` + LF + LF
/// - 无 event：`data: <data>` + LF + LF
pub fn encode_sse_event(data: &str, event: Option<&str>) -> String {
    match event {
        Some(e) => format!("event: {e}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// 编码流结束标记 `[DONE]`。
pub fn encode_sse_done() -> &'static str {
    "data: [DONE]\n\n"
}

///
/// 输出格式：`event: error` + LF + `data: <msg>` + LF + LF
pub fn encode_sse_error(msg: &str) -> String {
    format!("event: error\ndata: {msg}\n\n")
}

/// 网关侧流中错误帧 — data 为 `{"error":{"type","message"}}` JSON 信封
///（与 REST 错误体同构）：`type` 为稳定错误码字面量，`message` 由调用方
/// 传入 Fluent 文案。帧内不得携带 vendor id / api_base 等内部拓扑
///（内部错误详情只进 tracing 日志）。
pub fn encode_gateway_stream_error(message: &str) -> String {
    let payload = serde_json::json!({
        "error": {
            "type": "upstream_stream_interrupted",
            "message": message,
        }
    });
    encode_sse_error(&payload.to_string())
}

/// 解析后的 SSE 帧。
///
/// `raw` 保留帧原文（不含定界空行），透传路径逐帧原样转发时与上游
/// 字节级一致；`event`/`data` 为解析出的结构化字段，供错误检测与
/// 上游 SSE → StreamChunk 反向解析使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// 帧原文（不含定界空行，已解码为 UTF-8）
    pub raw: String,
    /// `event:` 字段值（如有）
    pub event: Option<String>,
    /// `data:` 负载（多行 data 以 `\n` 连接；无 data 行时为 `None`）
    pub data: Option<String>,
}

impl SseFrame {
    /// 帧是否携带上游错误信号：`event: error`，或 data 是含 `"error"`
    /// 对象的 JSON（Anthropic 流中错误的规范形状）。
    pub fn is_error(&self) -> bool {
        if self.event.as_deref() == Some("error") {
            return true;
        }
        match &self.data {
            Some(d) => serde_json::from_str::<serde_json::Value>(d)
                .ok()
                .and_then(|v| v.get("error").map(|e| e.is_object() || e.is_string()))
                .unwrap_or(false),
            None => false,
        }
    }
}

/// 上游 SSE 增量分帧器（阶段一）。
///
/// - **跨块缓冲**：网络块按字节 append 进内部 buffer，帧定界符自身被
///   劈在两块之间时自然跨块拼回；
/// - **字节层切帧**：帧定界只认 `\n`（0x0A）与 `\r`（0x0D）——二者均不是
///   合法 UTF-8 尾随字节（0x80-0xBF），因此在字节层扫描定界符永远不会把
///   多字节序列误判为帧边界；
/// - **增量 UTF-8 解码**：帧完整（已见定界）才解码，跨块劈开的多字节
///   字符在 buffer 中拼齐后整体解码，消除逐块 `from_utf8_lossy` 的
///   U+FFFD 劈裂；
/// - **帧内逐行解析**：`data:`/`event:` 字段与注释行（`:` 前缀）按规范
///   处理，多行 data 以 `\n` 连接。
#[derive(Debug, Default)]
pub struct SseFrameParser {
    buffer: Vec<u8>,
}

impl SseFrameParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一个网络块（字节级 append，拷贝一次整块）。
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// buffer 中是否还有未取出的字节。
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// 取出一帧。无完整帧时返回 `None`（残余字节留在 buffer 等下一块）。
    pub fn next_frame(&mut self) -> Option<SseFrame> {
        let (frame_end, consumed) = scan_frame_delimiter(&self.buffer)?;
        let raw = String::from_utf8_lossy(&self.buffer[..frame_end]).into_owned();
        self.buffer.drain(..consumed);
        Some(build_frame(raw))
    }

    /// 流结束时冲刷残余：上游省略末尾定界空行时，残余内容仍作为一帧
    /// 产出（对齐浏览器 EventSource 的 EOF 语义）。
    pub fn flush(&mut self) -> Option<SseFrame> {
        if self.buffer.is_empty() {
            return None;
        }
        let raw = String::from_utf8_lossy(&self.buffer).into_owned();
        self.buffer.clear();
        Some(build_frame(raw))
    }
}

/// 在字节 buffer 中扫描帧定界符。
///
/// 支持 `\n\n`、`\r\n\r\n` 与混合 `\n\r\n`（行尾 `\n` + 空行 `\r\n`）。
/// 返回 `(frame_end, consumed)`：帧内容止于 `frame_end`（第一个 `\n` 处，
/// 其前若是 `\r` 由行解析剥离），调用方应消费掉前 `consumed` 个字节。
fn scan_frame_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            let rest = &buf[i + 1..];
            match (rest.first(), rest.get(1)) {
                // 空行 = \n\n
                (Some(b'\n'), _) => return Some((i, i + 2)),
                // 空行 = \n\r\n（CRLF 变体）
                (Some(b'\r'), Some(b'\n')) => return Some((i, i + 3)),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 由帧原文构建 SseFrame：逐行解析 `data:`/`event:`，跳过空行与注释行。
fn build_frame(raw: String) -> SseFrame {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue; // 空行 / 注释行（SSE 规范）
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => data_lines.push(value.to_string()),
            "event" => event = Some(value.to_string()),
            _ => {} // id / retry 等字段对网关转发无用
        }
    }

    let data = if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    };
    SseFrame { raw, event, data }
}

/// 透传流适配器（阶段一）：上游字节流过分帧器后逐帧原样转发。
///
/// - 帧边界与上游一致：每帧转发 `raw + "\n\n"`（消除 openai.rs 旧实现的
///   「整块 `from_utf8_lossy` 再包一层 `data:`」嵌套包装）；
/// - 中流错误：上游 IO 错误或 `event: error` / 错误 JSON 帧
///   → 发出带语义的 SSE error 事件并提前终止，不再继续正常转发；
/// - 上游 EOF：冲刷残余帧后正常终止。
///
/// 上游流错误类型仅需 `Display`（reqwest::Error 等），输出统一为
/// `Result<Bytes, std::io::Error>` 供 `axum::body::Body::from_stream` 使用。
pub fn reframe_passthrough_stream<S, E>(
    stream: S,
) -> impl Stream<Item = Result<Bytes, std::io::Error>>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::fmt::Display,
{
    // 无观测出口：空回调被内联掉，字节与终止行为与带观测形态逐字相同。
    reframe_passthrough_stream_with_usage(stream, |_| {})
}

/// 透传流观测事件（配额结算的输入）：逐帧内容 + 终止通知。
///
/// 帧字段由 `SseFrame` 直接 move（无额外拷贝，透传热路径零分配增量）；
/// `End.completed` 在 EOF 冲刷完最后一帧后为 `true`，上游 `Err` 或错误帧
/// 提前终止时为 `false`——调用方据此决定回补或保持预扣。
pub enum ReframeObservation {
    /// 一帧已成形并即将转发（`event`/`data` 为帧内结构化字段）
    Frame {
        event: Option<String>,
        data: Option<String>,
    },
    /// 流终止（`completed = true` 为正常 EOF，`false` 为错误/提前终止）
    End { completed: bool },
}

/// 透传流适配器（带帧观测）：与 [`reframe_passthrough_stream`] 逐字节同输出，
/// 额外把每帧的 `event`/`data` 与终止事件交给 `observe`。
pub fn reframe_passthrough_stream_with_usage<S, E, F>(
    stream: S,
    observe: F,
) -> impl Stream<Item = Result<Bytes, std::io::Error>>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::fmt::Display,
    F: FnMut(ReframeObservation),
{
    // 上游流 Box::pin：S 仅保证 Stream，Unpin 由 Pin<Box<S>> 提供
    // （每流一次堆分配，可忽略）
    let stream = Box::pin(stream);
    futures::stream::unfold(
        (SseFrameParser::new(), stream, false, observe),
        |(mut parser, mut stream, done, mut observe)| async move {
            loop {
                if done {
                    return None;
                }
                // 倾倒 buffer 中已凑齐的帧（一次 unfold 一个 item，逐帧返回）
                if let Some(frame) = parser.next_frame() {
                    let terminate = frame.is_error();
                    // 字段 move 进观测事件（零拷贝），raw 继续原样转发
                    let SseFrame { raw, event, data } = frame;
                    observe(ReframeObservation::Frame { event, data });
                    if terminate {
                        observe(ReframeObservation::End { completed: false });
                    }
                    let mut out = raw;
                    out.push_str("\n\n");
                    return Some((Ok(Bytes::from(out)), (parser, stream, terminate, observe)));
                }
                // buffer 无完整帧：拉取上游下一块
                match futures::StreamExt::next(&mut stream).await {
                    Some(Ok(chunk)) => parser.feed(&chunk),
                    Some(Err(e)) => {
                        // 流中断发带语义的 error 事件，提前终止
                        let msg = t("protocol-stream-error-frame");
                        tracing::warn!(
                            error = %e,
                            "{}",
                            t("protocol-passthrough-stream-interrupted")
                        );
                        observe(ReframeObservation::End { completed: false });
                        return Some((
                            Ok(Bytes::from(encode_gateway_stream_error(&msg))),
                            (parser, stream, true, observe),
                        ));
                    }
                    None => {
                        // EOF：冲刷残余帧（此后 buffer 必空，下轮终止）
                        if let Some(frame) = parser.flush() {
                            let SseFrame { raw, event, data } = frame;
                            observe(ReframeObservation::Frame { event, data });
                            observe(ReframeObservation::End { completed: true });
                            let mut out = raw;
                            out.push_str("\n\n");
                            return Some((Ok(Bytes::from(out)), (parser, stream, true, observe)));
                        }
                        observe(ReframeObservation::End { completed: true });
                        return None;
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod reframe_observation_tests {
    use super::{
        reframe_passthrough_stream, reframe_passthrough_stream_with_usage, ReframeObservation,
    };
    use bytes::Bytes;
    use futures::StreamExt as _;
    use std::sync::{Arc, Mutex};

    /// 观测事件的测试侧结构化记录（避免比对了 `Debug` 文本格式）。
    #[derive(Debug, Clone, PartialEq)]
    enum Probe {
        Frame(Option<String>, Option<String>),
        End(bool),
    }

    fn ok_stream(texts: &[&str]) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
        futures::stream::iter(
            texts
                .iter()
                .map(|t| Ok(Bytes::from(t.to_string())))
                .collect::<Vec<_>>(),
        )
    }

    /// 抽干流并拼接转发字节（观测序列由调用方持有的 sink 读取）。
    async fn drain<S>(stream: S) -> String
    where
        S: futures::Stream<Item = Result<Bytes, std::io::Error>>,
    {
        let mut stream = Box::pin(stream);
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            out.push_str(&String::from_utf8_lossy(&item.unwrap()));
        }
        out
    }

    fn record(sink: &Arc<Mutex<Vec<Probe>>>) -> Arc<Mutex<Vec<Probe>>> {
        sink.clone()
    }

    fn observe_into(sink: &Arc<Mutex<Vec<Probe>>>) -> impl FnMut(ReframeObservation) {
        let sink = record(sink);
        move |ev| {
            let probe = match ev {
                ReframeObservation::Frame { event, data } => Probe::Frame(event, data),
                ReframeObservation::End { completed } => Probe::End(completed),
            };
            sink.lock().unwrap_or_else(|p| p.into_inner()).push(probe);
        }
    }

    /// 逐帧对应：两帧输入的观测与转发字节同序，EOF 给 `completed = true`。
    #[tokio::test]
    async fn frames_map_to_observations_and_normal_end() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let stream = reframe_passthrough_stream_with_usage(
            ok_stream(&["data: {\"a\":1}\n\ndata: {\"b\":2}\n\n"]),
            observe_into(&sink),
        );
        let bytes = drain(stream).await;
        assert_eq!(bytes, "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n", "字节保真");
        let recorded = sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded,
            vec![
                Probe::Frame(None, Some("{\"a\":1}".into())),
                Probe::Frame(None, Some("{\"b\":2}".into())),
                Probe::End(true),
            ],
            "观测需逐帧对应且正常收束"
        );
    }

    /// 错误帧：观测到该帧后紧接 `completed = false`，后续帧不再转发。
    #[tokio::test]
    async fn error_frame_reports_incomplete_end_and_drops_tail() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let stream = reframe_passthrough_stream_with_usage(
            ok_stream(&[
                "event: error\ndata: {\"error\":{\"message\":\"boom\"}}\n\ndata: {\"c\":3}\n\n",
            ]),
            observe_into(&sink),
        );
        let bytes = drain(stream).await;
        assert_eq!(
            bytes, "event: error\ndata: {\"error\":{\"message\":\"boom\"}}\n\n",
            "错误帧后不得继续转发"
        );
        let recorded = sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded,
            vec![
                Probe::Frame(
                    Some("error".into()),
                    Some("{\"error\":{\"message\":\"boom\"}}".into())
                ),
                Probe::End(false),
            ],
            "错误终止必须报 completed=false 且不补正常尾帧"
        );
    }

    /// 上游 Err：发出网关 error 帧且观测 `completed = false`（不回补的口径）。
    #[tokio::test]
    async fn upstream_error_reports_incomplete_end() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let input = futures::stream::iter(vec![
            Ok(Bytes::from("data: {\"a\":1}\n\n")),
            Err(std::io::Error::other("upstream broke")),
        ]);
        let stream = reframe_passthrough_stream_with_usage(input, observe_into(&sink));
        let bytes = drain(stream).await;
        assert!(
            bytes.starts_with("data: {\"a\":1}\n\n"),
            "已成形帧先行转发: {bytes}"
        );
        assert!(bytes.contains("event: error"), "断流补语义错误帧: {bytes}");
        let recorded = sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded,
            vec![
                Probe::Frame(None, Some("{\"a\":1}".into())),
                Probe::End(false),
            ],
            "断流不得报 completed=true"
        );
    }

    /// 无末尾定界空的残帧：EOF 冲刷为一帧 + `completed = true`。
    #[tokio::test]
    async fn eof_flush_reports_residual_frame() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let stream =
            reframe_passthrough_stream_with_usage(ok_stream(&["data: tail"]), observe_into(&sink));
        let bytes = drain(stream).await;
        assert_eq!(bytes, "data: tail\n\n", "残帧仍转发为完整帧");
        let recorded = sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded,
            vec![Probe::Frame(None, Some("tail".into())), Probe::End(true)]
        );
    }

    /// 回归门禁：无观测包装与带观测形态的转发字节逐字节一致（开关关时零行为变化）。
    #[tokio::test]
    async fn wrapper_preserves_bytes_identically_to_observed_form() {
        let payload = "data: {\"x\":1}\n\n\ndata: {\"x\":2}\n\n";
        let plain = drain(reframe_passthrough_stream(ok_stream(&[payload]))).await;
        let sink = Arc::new(Mutex::new(Vec::new()));
        let observed = drain(reframe_passthrough_stream_with_usage(
            ok_stream(&[payload]),
            observe_into(&sink),
        ))
        .await;
        assert_eq!(plain, observed, "观测不得改变字节");
    }
}
