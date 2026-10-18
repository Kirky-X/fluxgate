// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 请求级配置（采样参数 / 全量 typed OpenAI Chat 参数 / 超时 / 路由 hints）。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// 推理强度（wire 词表与官方一致：`none/minimal/low/medium/high/xhigh/max`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// 输出详略（OpenAI `verbosity`：`low/medium/high`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    Low,
    Medium,
    High,
}

/// 流式附加选项（`stream_options`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StreamOptions {
    /// usage 尾帧（choices 空 + usage 对象）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
    /// 流式混淆字段（官方 stream obfuscation 特性）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_obfuscation: Option<bool>,
}

/// OpenAI wire 参数视图：未设置的 Option 字段直接省略（不写显式 null，
/// 严格模式上游会拒绝 null，且省略免去空字段序列化分配）。
/// `vendor_hint` / `model_hint` / `timeout` 是 header / 客户端级参数，
/// 不出现在该视图。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OpenAiParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_logprobs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) logit_bias: Option<BTreeMap<String, i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verbosity: Option<Verbosity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) safety_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_cache_retention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prediction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) web_search_options: Option<Value>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user: Option<String>,
}

/// 请求配置。字段覆盖 OpenAI Chat Completions API 全量 typed 参数，
/// 外加 `vendor_hint` / `model_hint` 用于路由 header（不进入请求 body）。
#[derive(Debug, Clone, Default)]
pub struct RequestConfig {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    /// 补全 token 上限（新名；与 `max_tokens` 双槽并存独立透传）
    pub max_completion_tokens: Option<u32>,
    /// 频率惩罚（IR SamplingParams 同名语义；-2.0 ~ 2.0）
    pub frequency_penalty: Option<f32>,
    /// 存在惩罚（IR SamplingParams 同名语义）
    pub presence_penalty: Option<f32>,
    /// 生成候选数（多 choice）
    pub n: Option<u32>,
    /// 是否返回采样 logprobs
    pub logprobs: Option<bool>,
    /// 每个 token 返回的 top logprobs 数（需 `logprobs: true`）
    pub top_logprobs: Option<u32>,
    /// 采样种子（尽力确定性）
    pub seed: Option<i64>,
    /// 结构化输出（`response_format` 原样透传）
    pub response_format: Option<Value>,
    /// token 偏置（token id → -100~100）
    pub logit_bias: Option<BTreeMap<String, i32>>,
    /// 并行工具调用开关
    pub parallel_tool_calls: Option<bool>,
    /// 推理强度（`reasoning_effort`）
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 输出详略（`verbosity`）
    pub verbosity: Option<Verbosity>,
    /// 服务层级（`auto`/`default`/`flex`/`priority` 原词透传）
    pub service_tier: Option<String>,
    /// 是否在服务端存储响应
    pub store: Option<bool>,
    /// 请求元数据（官方 ≤16 键）
    pub metadata: Option<BTreeMap<String, String>>,
    /// 安全标识（滥用检测归因）
    pub safety_identifier: Option<String>,
    /// prompt cache 键
    pub prompt_cache_key: Option<String>,
    /// prompt cache 保留策略
    pub prompt_cache_retention: Option<String>,
    /// 流式附加选项
    pub stream_options: Option<StreamOptions>,
    /// 输出模态（如 `["text", "audio"]`）
    pub modalities: Option<Vec<String>>,
    /// 音频输出参数（原样透传）
    pub audio: Option<Value>,
    /// 预测输出（latency 优化，原样透传）
    pub prediction: Option<Value>,
    /// web 搜索工具参数（原样透传）
    pub web_search_options: Option<Value>,
    /// 追加 body 键（与 `vendor_extensions` 合并，后者优先；`RESERVED_KEYS` 拒绝）
    pub extra_body: BTreeMap<String, Value>,
    /// 请求级附加头（覆盖客户端 `default_headers` 同名键，大小写不敏感；
    /// `authorization`/`host`/`content-length` 忽略并 debug）
    pub extra_headers: BTreeMap<String, String>,
    /// 请求级重试上限覆盖（None → 客户端 `RetryPolicy`）
    pub max_retries: Option<u32>,
    /// `X-Session-Id`（会话亲和）
    pub session_id: Option<String>,
    /// `X-Batch`（batch 路由标记；`true` → `X-Batch: 1`）
    pub batch: Option<bool>,
    /// `X-Weight`（请求级加权路由，有限正数）
    pub weight: Option<f64>,
    pub stream: bool,
    /// 非流式请求（chat / embed）的总超时逐请求覆盖：Some(d) 取代客户端
    /// 默认（`FluxgateClientBuilder::timeout`，缺省 600s）；对
    /// `chat_stream` 不生效（流式不设总超时，由 read_timeout 看护停滞）。
    pub timeout: Option<Duration>,
    pub vendor_hint: Option<String>,
    pub model_hint: Option<String>,
    pub stop: Option<Vec<String>>,
    pub user: Option<String>,
}

impl RequestConfig {
    /// 合并配置：`other` 的非 None 字段优先覆盖 `self`。
    /// `stream` 总是取 `other` 的值（显式覆盖）。
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            temperature: other.temperature.or(self.temperature),
            top_p: other.top_p.or(self.top_p),
            top_k: other.top_k.or(self.top_k),
            max_tokens: other.max_tokens.or(self.max_tokens),
            max_completion_tokens: other.max_completion_tokens.or(self.max_completion_tokens),
            frequency_penalty: other.frequency_penalty.or(self.frequency_penalty),
            presence_penalty: other.presence_penalty.or(self.presence_penalty),
            n: other.n.or(self.n),
            logprobs: other.logprobs.or(self.logprobs),
            top_logprobs: other.top_logprobs.or(self.top_logprobs),
            seed: other.seed.or(self.seed),
            response_format: other
                .response_format
                .clone()
                .or_else(|| self.response_format.clone()),
            logit_bias: other.logit_bias.clone().or_else(|| self.logit_bias.clone()),
            parallel_tool_calls: other.parallel_tool_calls.or(self.parallel_tool_calls),
            reasoning_effort: other.reasoning_effort.or(self.reasoning_effort),
            verbosity: other.verbosity.or(self.verbosity),
            service_tier: other
                .service_tier
                .clone()
                .or_else(|| self.service_tier.clone()),
            store: other.store.or(self.store),
            metadata: other.metadata.clone().or_else(|| self.metadata.clone()),
            safety_identifier: other
                .safety_identifier
                .clone()
                .or_else(|| self.safety_identifier.clone()),
            prompt_cache_key: other
                .prompt_cache_key
                .clone()
                .or_else(|| self.prompt_cache_key.clone()),
            prompt_cache_retention: other
                .prompt_cache_retention
                .clone()
                .or_else(|| self.prompt_cache_retention.clone()),
            stream_options: other.stream_options.or(self.stream_options),
            modalities: other.modalities.clone().or_else(|| self.modalities.clone()),
            audio: other.audio.clone().or_else(|| self.audio.clone()),
            prediction: other.prediction.clone().or_else(|| self.prediction.clone()),
            web_search_options: other
                .web_search_options
                .clone()
                .or_else(|| self.web_search_options.clone()),
            extra_body: {
                let mut extensions = self.extra_body.clone();
                extensions.extend(other.extra_body.clone());
                extensions
            },
            extra_headers: {
                let mut headers = self.extra_headers.clone();
                headers.extend(other.extra_headers.clone());
                headers
            },
            max_retries: other.max_retries.or(self.max_retries),
            session_id: other.session_id.clone().or_else(|| self.session_id.clone()),
            batch: other.batch.or(self.batch),
            weight: other.weight.or(self.weight),
            stream: other.stream,
            timeout: other.timeout.or(self.timeout),
            vendor_hint: other
                .vendor_hint
                .clone()
                .or_else(|| self.vendor_hint.clone()),
            model_hint: other.model_hint.clone().or_else(|| self.model_hint.clone()),
            stop: other.stop.clone().or_else(|| self.stop.clone()),
            user: other.user.clone().or_else(|| self.user.clone()),
        }
    }

    /// OpenAI wire 参数视图（未设字段省略；`timeout`/hints 不进 body）。
    pub(crate) fn openai_params(&self) -> OpenAiParams {
        OpenAiParams {
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_tokens: self.max_tokens,
            max_completion_tokens: self.max_completion_tokens,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            n: self.n,
            logprobs: self.logprobs,
            top_logprobs: self.top_logprobs,
            seed: self.seed,
            response_format: self.response_format.clone(),
            logit_bias: self.logit_bias.clone(),
            parallel_tool_calls: self.parallel_tool_calls,
            reasoning_effort: self.reasoning_effort,
            verbosity: self.verbosity,
            service_tier: self.service_tier.clone(),
            store: self.store,
            metadata: self.metadata.clone(),
            safety_identifier: self.safety_identifier.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            prompt_cache_retention: self.prompt_cache_retention.clone(),
            stream_options: self.stream_options,
            modalities: self.modalities.clone(),
            audio: self.audio.clone(),
            prediction: self.prediction.clone(),
            web_search_options: self.web_search_options.clone(),
            stream: self.stream,
            stop: self.stop.clone(),
            user: self.user.clone(),
        }
    }
}
