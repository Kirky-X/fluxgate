// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流式请求用量记账：占位日志行的流终止补写与 keyed 分账。
//!
//! 流式派发在响应头阶段只落 `{"stream":true}` 占位行（usage 帧只在流中
//! 到达）——本记录器挂在透传流的分帧观测上，逐帧提取 usage，流终止点
//! （EOF/错误帧/客户端中断经 [`Drop`] 兜底）补写日志三列并写 keyed 分账。
//! 无 usage 帧（客户端未请求 `include_usage`）保持占位行原样、不计账。

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
/// - Anthropic：`message_delta.usage` / `message_start.message.usage` /
///   顶层 `usage`（兼容三种到达形态）。
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
        StreamProtocol::Anthropic | StreamProtocol::Responses => FrameUsage {
            prompt: u64_field(usage, "input_tokens"),
            completion: u64_field(usage, "output_tokens"),
            cache_read: u64_field(usage, "cache_read_input_tokens"),
            cache_creation: u64_field(usage, "cache_creation_input_tokens"),
        },
        StreamProtocol::Gemini => FrameUsage {
            prompt: u64_field(usage, "promptTokenCount"),
            completion: u64_field(usage, "candidatesTokenCount"),
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
        StreamProtocol::Anthropic | StreamProtocol::Responses => json!({
            "usage": {
                "input_tokens": usage.prompt,
                "output_tokens": usage.completion,
                "cache_read_input_tokens": usage.cache_read,
                "cache_creation_input_tokens": usage.cache_creation,
            }
        }),
        StreamProtocol::Gemini => json!({
            "usageMetadata": {
                "promptTokenCount": usage.prompt,
                "candidatesTokenCount": usage.completion,
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

    /// 逐帧观测：只留含 usage 的帧。OpenAI 尾帧唯一取替换；Anthropic
    /// 增量形态取跨帧最大值（即累计值）。
    pub(crate) fn observe_frame(&self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let Some(frame) = extract_frame_usage(self.inner.protocol, &value) else {
            return;
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
        tokio::spawn(async move {
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
