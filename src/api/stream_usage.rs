// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流式请求用量记账：占位日志行的流终止补写与 keyed 分账。
//!
//! 流式派发在响应头阶段只落 `{"stream":true}` 占位行（usage 帧只在流中
//! 到达）——本记录器有两种观测源：① 透传流的分帧观测（逐帧 JSON 提取）；
//! ② 转换流的 IR chunk（`StreamChunk::Usage` 已结构化，零解析）。流终止点
//! （EOF/错误帧/客户端中断经 [`Drop`] 兜底）补写日志三列并写 keyed 分账。
//! 无 usage 帧（OpenAI 透传未带 `include_usage`）保持占位行原样、不计账。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::api::state::ApiState;

/// 流式协议：决定 usage 帧的提取位置与补写日志行的 usage 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamProtocol {
    /// Anthropic messages SSE：input 于 `message_start`、output 于
    /// `message_delta` 增量到达（各字段取跨帧最大值即为累计值）
    Anthropic,
    /// OpenAI chat completions SSE：`include_usage` 尾帧一次给全
    /// （choices 为空 + usage），取末帧
    OpenAI,
    /// Gemini `streamGenerateContent` SSE：`usageMetadata` 逐帧携带
    /// （candidatesTokenCount 累计，取跨帧最大值）
    Gemini,
    /// OpenAI Responses 原生 SSE：`response.completed` 事件携带
    /// `response.usage`，取末帧
    Responses,
}

/// 单帧提取出的用量（token 口径与 request_logs 三列同源）
#[derive(Debug, Clone, Copy, Default)]
struct FrameUsage {
    prompt: u64,
    completion: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl FrameUsage {
    fn merge_max(self, other: Self) -> Self {
        Self {
            prompt: self.prompt.max(other.prompt),
            completion: self.completion.max(other.completion),
            cache_read: self.cache_read.max(other.cache_read),
            cache_creation: self.cache_creation.max(other.cache_creation),
        }
    }
}

/// 从一帧 SSE data 提取 usage（按协议形态；无 usage 返回 None）。
///
/// - OpenAI：顶层 `usage`（尾帧形态，`choices` 为空）。
/// - Responses：`response.usage`（`response.completed` 事件）。
/// - Anthropic：`message_delta.usage` / `message_start.message.usage` /
///   顶层 `usage`（兼容三种到达形态）。
/// - Gemini：顶层 `usageMetadata`；`completion = candidates + thoughts`
///   （思考 token 按输出计费，与 [`crate::protocol::parse_usage_detailed`]
///   同口径）。
fn extract_frame_usage(protocol: StreamProtocol, value: &Value) -> Option<FrameUsage> {
    fn u64_field(value: &Value, key: &str) -> u64 {
        value.get(key).and_then(Value::as_u64).unwrap_or(0)
    }

    let usage = match protocol {
        StreamProtocol::OpenAI => value.get("usage")?,
        StreamProtocol::Responses => value.get("response").and_then(|r| r.get("usage"))?,
        StreamProtocol::Anthropic => value
            .get("message_delta")
            .and_then(|delta| delta.get("usage"))
            .or_else(|| {
                value
                    .get("message_start")
                    .and_then(|start| start.get("message"))
                    .and_then(|message| message.get("usage"))
            })
            .or_else(|| value.get("usage"))?,
        StreamProtocol::Gemini => value.get("usageMetadata")?,
    };
    if !usage.is_object() {
        return None;
    }
    Some(match protocol {
        StreamProtocol::OpenAI => FrameUsage {
            prompt: u64_field(usage, "prompt_tokens"),
            completion: u64_field(usage, "completion_tokens"),
            cache_read: usage
                .get("prompt_tokens_details")
                .map(|details| u64_field(details, "cached_tokens"))
                .unwrap_or_default(),
            cache_creation: 0,
        },
        StreamProtocol::Anthropic => FrameUsage {
            prompt: u64_field(usage, "input_tokens"),
            completion: u64_field(usage, "output_tokens"),
            cache_read: u64_field(usage, "cache_read_input_tokens"),
            cache_creation: u64_field(usage, "cache_creation_input_tokens"),
        },
        // Responses 用 `input_tokens_details.cached_tokens`（真实 wire 形
        // 状），兼容 Anthropic 形状（历史/混合代理）；无 cache_creation 概念。
        StreamProtocol::Responses => FrameUsage {
            prompt: u64_field(usage, "input_tokens"),
            completion: u64_field(usage, "output_tokens"),
            cache_read: usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| usage.get("cache_read_input_tokens").and_then(Value::as_u64))
                .unwrap_or_default(),
            cache_creation: 0,
        },
        // 思考 token 按输出计费：与 parse_usage_detailed 的
        // `candidates + thoughts` 口径一致；形状拼装不再写 thoughts 字段。
        StreamProtocol::Gemini => FrameUsage {
            prompt: u64_field(usage, "promptTokenCount"),
            completion: u64_field(usage, "candidatesTokenCount")
                .saturating_add(u64_field(usage, "thoughtsTokenCount")),
            cache_read: u64_field(usage, "cachedContentTokenCount"),
            cache_creation: 0,
        },
    })
}

/// 按协议构造补写日志行的 usage 响应体（形状与 [`crate::protocol::parse_usage_detailed`]
/// 的双格式解析对齐，分账走同一份数据）。
fn usage_response_value(protocol: StreamProtocol, usage: &FrameUsage) -> Value {
    match protocol {
        StreamProtocol::OpenAI => json!({
            "usage": {
                "prompt_tokens": usage.prompt,
                "completion_tokens": usage.completion,
                "total_tokens": usage.prompt + usage.completion,
                "prompt_tokens_details": { "cached_tokens": usage.cache_read },
            }
        }),
        StreamProtocol::Anthropic => json!({
            "usage": {
                "input_tokens": usage.prompt,
                "output_tokens": usage.completion,
                "cache_read_input_tokens": usage.cache_read,
                "cache_creation_input_tokens": usage.cache_creation,
            }
        }),
        // Responses 形状用 input_tokens_details.cached_tokens（与其真实
        // wire 一致），parse_usage_detailed 双形态均可解析。
        StreamProtocol::Responses => json!({
            "usage": {
                "input_tokens": usage.prompt,
                "output_tokens": usage.completion,
                "total_tokens": usage.prompt + usage.completion,
                "input_tokens_details": { "cached_tokens": usage.cache_read },
            }
        }),
        // Gemini：completion 已折入 thoughts（见 extract_frame_usage 注
        // 释），此处只写 candidatesTokenCount，避免分账解析二次相加。
        StreamProtocol::Gemini => json!({
            "usageMetadata": {
                "promptTokenCount": usage.prompt,
                "candidatesTokenCount": usage.completion,
                "totalTokenCount": usage.prompt + usage.completion,
                "cachedContentTokenCount": usage.cache_read,
            }
        }),
    }
}

/// 流式用量记录器：Clone 分发给观测回调，末次收尾生效。
#[derive(Clone)]
pub(crate) struct StreamUsageRecorder {
    inner: Arc<Inner>,
}

struct Inner {
    state: Arc<ApiState>,
    request_id: String,
    protocol: StreamProtocol,
    vendor_id: String,
    vendor_type: String,
    model_id: String,
    api_key_hash: String,
    usage: Mutex<Option<FrameUsage>>,
    settled: AtomicBool,
}

impl StreamUsageRecorder {
    /// `vendor_id`/`vendor_type`/`model_id` 取自路由裁决与请求体（分账
    /// 维度）；`request_id` 用于定位派发阶段落的占位日志行。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        state: Arc<ApiState>,
        request_id: String,
        protocol: StreamProtocol,
        vendor_id: String,
        vendor_type: String,
        model_id: String,
        api_key_hash: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state,
                request_id,
                protocol,
                vendor_id,
                vendor_type,
                model_id,
                api_key_hash,
                usage: Mutex::new(None),
                settled: AtomicBool::new(false),
            }),
        }
    }

    /// 逐帧观测（透传 SSE）：只留含 usage 的帧；Anthropic 增量形态取
    /// 跨帧最大值（即累计值），OpenAI/Gemini 尾帧取替换。
    pub(crate) fn observe_frame(&self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let Some(frame) = extract_frame_usage(self.inner.protocol, &value) else {
            return;
        };
        self.observe_usage(
            frame.prompt,
            frame.completion,
            frame.cache_read,
            frame.cache_creation,
        );
    }

    /// 结构化观测（转换流 IR chunk）：usage 已由协议解析器聚合，本层零
    /// JSON 解析；多帧仍按最大值收敛（Anthropic `message_start` 给 input、
    /// `message_delta` 给 output）。
    pub(crate) fn observe_usage(
        &self,
        prompt: u64,
        completion: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let frame = FrameUsage {
            prompt,
            completion,
            cache_read,
            cache_creation,
        };
        let mut guard = self
            .inner
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(match *guard {
            Some(prev) => prev.merge_max(frame),
            None => frame,
        });
    }

    /// 流终止收尾：补写占位日志行（response 列 usage + token 三列）并写
    /// keyed 分账；无 usage 帧则什么都不做。幂等（Drop 兜底与显式调用
    /// 只生效一次）；落库失败显性 warn，不影响已转发给客户端的字节。
    pub(crate) fn finish(&self) {
        if self.inner.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        // 无日志/分账接线（SDK 嵌入或测试降级）时零收尾开销。
        if self.inner.state.logger.is_none() && self.inner.state.pre_aggregator.is_none() {
            return;
        }
        let usage = self
            .inner
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(usage) = usage else {
            return;
        };
        let inner = self.inner.clone();
        // Shutdown 期 Drop 可能落在无运行时的线程：记 warn 而非 panic。
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                request_id = %inner.request_id,
                "stream usage settlement skipped: no tokio runtime"
            );
            return;
        };
        handle.spawn(async move {
            let response = usage_response_value(inner.protocol, &usage);
            if let Some(logger) = inner.state.logger.as_ref() {
                if let Err(e) = logger
                    .update_stream_usage(
                        &inner.request_id,
                        &response.to_string(),
                        usage.prompt as i64,
                        usage.completion as i64,
                        (usage.prompt + usage.completion) as i64,
                    )
                    .await
                {
                    tracing::warn!(
                        request_id = %inner.request_id,
                        error = %e,
                        "stream usage log update failed"
                    );
                }
            }
            crate::api::record_keyed_usage(
                &inner.state,
                &inner.vendor_id,
                &inner.vendor_type,
                &inner.model_id,
                &inner.api_key_hash,
                &response,
            )
            .await;
        });
    }
}

impl Drop for StreamUsageRecorder {
    fn drop(&mut self) {
        // Drop 兜底仅对「最后一个存活副本」生效：handler 作用域持有的原始
        // 副本会先于流终止析构，若它也触发 finish 会把 settled 置位、令
        // 观测闭包的真正收尾被幂等挡掉。
        if Arc::strong_count(&self.inner) == 1 {
            self.finish();
        }
    }
}

/// 转换流观测包装：IR chunk 流原样转发，命中的 [`crate::protocol::StreamChunk::Usage`]
/// 写进同一记录器（零 JSON 解析）；流终止（含客户端断流经生成器析构）触发
/// 与透传链完全一致的占位日志补写与 keyed 分账。
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_converted_stream<S>(
    state: Arc<ApiState>,
    request_id: String,
    protocol: StreamProtocol,
    vendor_id: String,
    vendor_type: String,
    model_id: String,
    api_key_hash: String,
    stream: S,
) -> impl futures::Stream<Item = Result<crate::protocol::StreamChunk, crate::error::GateError>>
where
    S: futures::Stream<Item = Result<crate::protocol::StreamChunk, crate::error::GateError>>
        + Send
        + 'static,
{
    let recorder = StreamUsageRecorder::new(
        state,
        request_id,
        protocol,
        vendor_id,
        vendor_type,
        model_id,
        api_key_hash,
    );
    async_stream::stream! {
        let mut stream = Box::pin(stream);
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            if let Ok(crate::protocol::StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                ..
            }) = &item
            {
                recorder.observe_usage(
                    u64::from(*prompt_tokens),
                    u64::from(*completion_tokens),
                    u64::from(*cache_read_tokens),
                    u64::from(*cache_creation_tokens),
                );
            }
            yield item;
        }
        // 正常 EOF 显式收尾；客户端断流由 recorder 的 Drop 兜底。
        recorder.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_tail_frame_extracts_usage() {
        let value = json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "total_tokens": 18,
                "prompt_tokens_details": { "cached_tokens": 3 }
            }
        });
        let usage = extract_frame_usage(StreamProtocol::OpenAI, &value).expect("usage");
        assert_eq!(usage.prompt, 11);
        assert_eq!(usage.completion, 7);
        assert_eq!(usage.cache_read, 3);
        assert_eq!(usage.cache_creation, 0);
    }

    #[test]
    fn anthropic_incremental_frames_merge_to_totals() {
        let start = json!({
            "message_start": { "message": { "usage": {
                "input_tokens": 12,
                "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 2,
                "output_tokens": 1
            }}}
        });
        let delta = json!({
            "message_delta": { "usage": { "output_tokens": 5 } }
        });
        let first = extract_frame_usage(StreamProtocol::Anthropic, &start).expect("start");
        let second = extract_frame_usage(StreamProtocol::Anthropic, &delta).expect("delta");
        let merged = first.merge_max(second);
        assert_eq!(merged.prompt, 12);
        assert_eq!(merged.completion, 5);
        assert_eq!(merged.cache_read, 4);
        assert_eq!(merged.cache_creation, 2);
    }

    #[test]
    fn responses_frame_reads_cached_tokens_detail() {
        let value = json!({
            "type": "response.completed",
            "response": { "usage": {
                "input_tokens": 9,
                "output_tokens": 4,
                "total_tokens": 13,
                "input_tokens_details": { "cached_tokens": 3 }
            }}
        });
        let usage = extract_frame_usage(StreamProtocol::Responses, &value).expect("usage");
        assert_eq!(usage.prompt, 9);
        assert_eq!(usage.completion, 4);
        assert_eq!(usage.cache_read, 3);
    }

    #[test]
    fn gemini_frame_folds_thoughts_into_completion() {
        let value = json!({
            "usageMetadata": {
                "promptTokenCount": 6,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 2,
                "cachedContentTokenCount": 1,
                "totalTokenCount": 13
            }
        });
        let usage = extract_frame_usage(StreamProtocol::Gemini, &value).expect("usage");
        assert_eq!(usage.prompt, 6);
        assert_eq!(usage.completion, 7, "completion = candidates + thoughts");
        assert_eq!(usage.cache_read, 1);
    }

    #[test]
    fn usage_shape_roundtrips_through_parse_usage_detailed() {
        let frame = FrameUsage {
            prompt: 10,
            completion: 6,
            cache_read: 3,
            cache_creation: 2,
        };
        for protocol in [
            StreamProtocol::OpenAI,
            StreamProtocol::Anthropic,
            StreamProtocol::Responses,
            StreamProtocol::Gemini,
        ] {
            let value = usage_response_value(protocol, &frame);
            let parsed = crate::protocol::parse_usage_detailed(&value)
                .unwrap_or_else(|| panic!("形状必须可被分账解析: {protocol:?} {value}"));
            // 仅 Anthropic 有 cache_creation 概念（其余协议形状不携带该键）。
            let expected_creation = match protocol {
                StreamProtocol::Anthropic => frame.cache_creation as u32,
                _ => 0,
            };
            assert_eq!(
                parsed,
                (
                    frame.prompt as u32,
                    frame.completion as u32,
                    frame.cache_read as u32,
                    expected_creation
                ),
                "协议形状回环失真: {protocol:?} {value}"
            );
        }
    }
}
