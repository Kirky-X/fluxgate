// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Bench 共享 fixture 基座（benches/ 各文件为独立编译单元，经 `mod common;`
//! 引入；未被某二进制消费的导出项以 `#![allow(dead_code)]` 放行——与
//! tests/common 同纪律）。
//!
//! 覆盖三组基准的共享构造：协议转换 fixture（chunk / 多轮请求 / 流块序列）、
//! 两协议 SSE 帧样例、路由固定 5 vendor 加权表。fixture 为手写测试专用构造
//! （仓库无 mock 库惯例，不引入 mockall）；与生产结构体字段耦合，协议结构
//! 演进时需同步维护。

#![allow(dead_code)]

use fluxgate::config::model_catalog::CacheBreakpointPolicy;
use fluxgate::config::{
    ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
    RateLimitConfig, ServerConfig, VendorConfig,
};
use fluxgate::protocol::{
    AnthropicConverter, MessageContent, ProtocolConverter, SseFrameParser, StreamChunk,
    UnifiedRequest,
};
use fluxgate::router::Router;
use fluxgate::vendor::VendorManager;
use serde_json::{json, Value};
use std::sync::Arc;

/// OpenAI 流式 chunk 代表性样例（chat.completion.chunk）：携带 id/created
/// 使 normalize 走重写路径，delta.content 为续块常态形态。
pub fn openai_chunk_value() -> Value {
    json!({
        "id": "chatcmpl-bench-0001",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000_u64,
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "delta": { "content": "Hello" },
            "finish_reason": null
        }]
    })
}

/// Anthropic 多轮请求（system + 6 轮对话 + 2 个工具定义）→ IR UnifiedRequest。
/// 走生产入站解析路径（`AnthropicConverter::to_unified`）构造，保证 fixture
/// 与真实请求同形态。
pub fn anthropic_unified_request() -> UnifiedRequest {
    let body = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 4096,
        "stream": true,
        "temperature": 0.7,
        "system": "You are a careful assistant. Follow the repository conventions strictly and keep replies concise.",
        "messages": [
            {"role": "user", "content": "帮我梳理一下这个模块的依赖关系。"},
            {"role": "assistant", "content": "这个模块依赖三个上游组件：配置管理、协议转换与存储边界。"},
            {"role": "user", "content": "协议转换里最热的路径是哪一段？"},
            {"role": "assistant", "content": "流式 chunk 的归一化与出站编码是逐帧执行的，属热点路径。"},
            {"role": "user", "content": "那缓存断点注入呢？"},
            {"role": "assistant", "content": "它在出站前做一次纯函数式的块标记，随请求体规模线性增长。"},
            {"role": "user", "content": [{"type": "text", "text": "把 weather 与 docs 两个工具都接入，然后给我一个最终建议。"}]}
        ],
        "tools": [
            {
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            },
            {
                "name": "search_docs",
                "description": "Search project documentation",
                "input_schema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }
            }
        ]
    });
    AnthropicConverter::new()
        .to_unified(body)
        .expect("fixture 入站解析必须成功")
}

/// tool_use 流块序列：首片段（id/name）→ 参数增量分片 → 终止文本块
/// （触发工具缓冲刷写）——`encode_chunk` 的典型工具调用尾段。
pub fn tool_use_chunk_sequence() -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCall {
            index: 0,
            id: "toolu_bench_01".into(),
            name: Some("get_weather".into()),
            arguments: None,
        },
        StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("{\"city\":".into()),
        },
        StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("\"Paris\"}".into()),
        },
        StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("Done.".into()),
            finish_reason: Some("tool_use".into()),
        },
    ]
}

/// 长多轮 Anthropic 出站请求体（`inject_cache_control` 输入形态）：tools 数组 +
/// system 字符串 + 8 轮对话（末条 user 内容为块数组）——tools / system /
/// last_user 三锚点齐备。
pub fn long_anthropic_body() -> Value {
    json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 4096,
        "system": "You are a careful assistant. Follow the repository conventions strictly and keep replies concise.",
        "tools": [
            {
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
            },
            {
                "name": "search_docs",
                "description": "Search project documentation",
                "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}
            }
        ],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "先概述这个仓库的分层结构。"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "api / middleware / router / protocol / database 五层。"}]},
            {"role": "user", "content": [{"type": "text", "text": "协议的入口在哪个模块？"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "src/protocol 下的转换器族。"}]},
            {"role": "user", "content": [{"type": "text", "text": "缓存断点的注入时机？"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "出站编码之后、发送之前。"}]},
            {"role": "user", "content": [{"type": "text", "text": "长上下文场景有什么注意点？"}]},
            {"role": "user", "content": [{"type": "text", "text": "最后一个问题：把工具说明也纳入缓存断点规划。"}]}
        ]
    })
}

/// 缓存断点策略 fixture：三锚点齐备、上限 4（覆盖锚点规划 + 应用全路径）。
pub fn cache_policy() -> CacheBreakpointPolicy {
    CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: 4,
        anchors: vec!["tools".into(), "system".into(), "last_user".into()],
    }
}

/// OpenAI 上游 SSE 尾段原文（含 data: 前缀与 [DONE] 终止帧；空格连接流内容）。
pub const OPENAI_SSE_TAIL: &str = concat!(
    "data: {\"id\":\"chatcmpl-bench\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-bench\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-bench\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\", world\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-bench\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// Anthropic 上游 SSE 尾段原文（含 event:/data: 前缀，message_delta 尾帧携带
/// usage + stop_reason，message_stop 收束）。
pub const ANTHROPIC_SSE_TAIL: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_bench\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3-5-sonnet-20241022\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\", world\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":12}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 经生产分帧器（[`SseFrameParser`]）从帧原文剥出 `data:` 负载——bench 输入
/// 形态与运行期一致（data:/event: 前缀与空行定界留在分帧环节）。
pub fn data_payloads(raw: &str) -> Vec<String> {
    let mut parser = SseFrameParser::new();
    parser.feed(raw.as_bytes());
    let mut payloads = Vec::new();
    while let Some(frame) = parser.next_frame() {
        if let Some(data) = frame.data {
            payloads.push(data);
        }
    }
    payloads
}

/// 5 vendor × 多权重 weighted 路由 fixture：每 vendor 一个 `gpt-4` 模型、
/// 权重各异；可选层（亲和 / bandit / SLA / LPS）全部保持默认关闭。
pub fn routing_config() -> Config {
    let weights = [1.0_f64, 0.5, 2.0, 0.75, 1.25];
    let vendors = weights
        .iter()
        .enumerate()
        .map(|(i, weight)| VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            supports_batch: false,
            chat_path: None,
            id: format!("bench-vendor-{i}"),
            name: format!("Bench Vendor {i}"),
            type_: "openai".into(),
            api_key_env: "FLUXGATE_BENCH_KEY".into(), // pragma: allowlist secret（仅 fixture 环境变量名，非凭证）
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: format!("http://127.0.0.1:{}", 9100 + i),
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "gpt-4".into(),
                abstract_id: Some("gpt-4".into()),
                weight: *weight,
                enabled: true,
                dimensions: None,
                compat_group: None,
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 1000,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        })
        .collect();

    Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors,
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![],
        model_catalog: Vec::new(),
        auth: None,
        cors: None,
        retry: Default::default(),
        routing: Default::default(),
        cache: Default::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: Default::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
    }
}

/// 路由 fixture：ConfigManager + VendorManager 装配（与路由集成测试同构，
/// 不依赖网络与外部服务）。
pub fn router_fixture() -> Router {
    let config_manager = Arc::new(ConfigManager::new(routing_config()));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    Router::new(config_manager, vendor_manager)
}
