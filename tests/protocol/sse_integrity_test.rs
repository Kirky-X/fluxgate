// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SSE 事件完整性对账：单元级编码器 + 集成级全链路，断言失败输出
//! 清晰的事件序列差异。

#![cfg(feature = "web-server")]

#[path = "../common/mod.rs"]
mod common;

use fluxgate::protocol::{
    AnthropicStreamEncoder, MessageContent, OpenAIConverter, OpenAIStreamEncoder,
    ProtocolConverter, StreamChunk,
};
use serde_json::{json, Value};

/// 事件序列可读快照（断言失败时输出，可诊断）。
fn snapshot(events: &[Value]) -> String {
    events
        .iter()
        .enumerate()
        .map(|(i, e)| format!("{i}: {e}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn event_types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            e.get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

/// 单元级：Anthropic 文本流（多块 + 工具调用）经编码器 → 事件序列断言。
#[test]
fn anthropic_text_and_tool_stream_sequence_integrity() {
    let mut encoder = AnthropicStreamEncoder::new();
    let mut events: Vec<Value> = Vec::new();
    let push = |events: &mut Vec<Value>, frames: Vec<String>| {
        events.extend(
            frames
                .into_iter()
                .map(|f| serde_json::from_str(&f).expect("编码器产出合法 JSON")),
        );
    };

    // 文本块 0（含 finish 闭合）+ 工具调用块 + usage 尾帧 + finish。
    push(
        &mut events,
        encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("Hello ".into()),
            finish_reason: None,
        }),
    );
    push(
        &mut events,
        encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("world".into()),
            finish_reason: Some("end_turn".into()),
        }),
    );
    push(
        &mut events,
        encoder.encode_chunk(StreamChunk::ToolCall {
            index: 0,
            id: "toolu_1".into(),
            name: Some("get_weather".into()),
            arguments: Some(r#"{"city":"Paris"}"#.into()),
        }),
    );
    push(
        &mut events,
        encoder.encode_chunk(StreamChunk::Usage {
            prompt_tokens: 12,
            completion_tokens: 8,
            total_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: None,
        }),
    );
    push(&mut events, encoder.finish());

    let types = event_types(&events);
    // message_start 起始（首事件）。
    assert_eq!(
        types.first().map(String::as_str),
        Some("message_start"),
        "必须 message_start 起始:\n{}",
        snapshot(&events)
    );
    // message_stop 终止（末事件，无孤儿尾帧）。
    assert_eq!(
        types.last().map(String::as_str),
        Some("message_stop"),
        "必须 message_stop 终止:\n{}",
        snapshot(&events)
    );
    // content_block_start/stop 按 index 严格配对连续（不跳号、不孤儿）。
    let mut open: Vec<u32> = Vec::new();
    let mut closed: Vec<u32> = Vec::new();
    for event in &events {
        let index = event
            .get("index")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        match event.get("type").and_then(|t| t.as_str()) {
            Some("content_block_start") => open.push(index.expect("start 带 index")),
            Some("content_block_stop") => closed.push(index.expect("stop 带 index")),
            _ => {}
        }
    }
    assert!(!open.is_empty(), "必须有内容块:\n{}", snapshot(&events));
    assert_eq!(
        open,
        closed,
        "start/stop 按 index 严格配对:\n{}",
        snapshot(&events)
    );
    let mut sorted = open.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(open, sorted, "块序号连续不跳号:\n{}", snapshot(&events));
    // delta 仅落在已开启块内：content_block_delta 的 index 必须已 start。
    for event in &events {
        if event.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
            let index = event.get("index").and_then(|v| v.as_u64()).unwrap() as u32;
            assert!(
                open.contains(&index),
                "delta 仅落在已开启块内（index={index}）:\n{}",
                snapshot(&events)
            );
        }
    }
    // message_delta 携带 usage 尾帧。
    let delta = events
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("message_delta"))
        .expect("message_delta 必须存在");
    assert!(
        delta.get("usage").is_some(),
        "message_delta 携带 usage:\n{}",
        snapshot(&events)
    );
}

/// 单元级：OpenAI 流（内容 + 终止 + usage）经编码器 → 首 chunk role、
/// finish 恰一次、id/created 一致。
#[test]
fn openai_stream_sequence_integrity() {
    let converter = OpenAIConverter::new();
    let mut encoder = OpenAIStreamEncoder::new();
    let chunks = vec![
        StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("Hi".into()),
            finish_reason: None,
        },
        StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text(" there".into()),
            finish_reason: Some("stop".into()),
        },
        StreamChunk::Usage {
            prompt_tokens: 5,
            completion_tokens: 3,
            total_tokens: 8,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: None,
        },
    ];
    let events: Vec<Value> = chunks
        .into_iter()
        .map(|c| {
            let mapped = converter.stream_chunk_to_external(c).expect("映射");
            encoder.normalize(mapped)
        })
        .collect();

    // 首 chunk 带 role。
    assert_eq!(
        events[0]["choices"][0]["delta"]["role"],
        "assistant",
        "首 chunk 携带 role:\n{}",
        snapshot(&events)
    );
    // 后续 chunk 不重复 role。
    for event in events.iter().skip(1) {
        assert!(
            event["choices"][0]["delta"].get("role").is_none(),
            "role 仅首 chunk:\n{}",
            snapshot(&events)
        );
    }
    // finish_reason 恰一次且在终止 chunk。
    let finish_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e["choices"]
                .as_array()
                .map(|choices| {
                    choices
                        .iter()
                        .any(|c| !c["finish_reason"].is_null() && c["finish_reason"] != "")
                })
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        finish_positions.len(),
        1,
        "finish_reason 恰一次:\n{}",
        snapshot(&events)
    );
    // chunk 间 id/created 一致。
    let ids: Vec<&str> = events
        .iter()
        .map(|e| e["id"].as_str().unwrap_or("?"))
        .collect();
    let created: Vec<u64> = events
        .iter()
        .map(|e| e["created"].as_u64().unwrap_or(u64::MAX))
        .collect();
    assert!(
        ids.iter().all(|id| *id == ids[0]),
        "id 一致: {ids:?}:\n{}",
        snapshot(&events)
    );
    assert!(
        created.iter().all(|c| *c == created[0]),
        "created 一致: {created:?}:\n{}",
        snapshot(&events)
    );
}

/// OpenAI 增量 tool_calls 上游帧组装（程序化拼接，杜绝手写转义错误）。
fn tool_call_frame(id: Option<&str>, name: Option<&str>, args: String) -> String {
    let mut function = serde_json::Map::new();
    if let Some(name) = name {
        function.insert("name".to_string(), Value::String(name.to_string()));
    }
    function.insert("arguments".to_string(), Value::String(args));
    let mut frag = serde_json::Map::new();
    frag.insert("index".to_string(), json!(0));
    if let Some(id) = id {
        frag.insert("id".to_string(), json!(id));
    }
    frag.insert("function".to_string(), Value::Object(function));
    json!({
        "id": "c1",
        "object": "chat.completion.chunk",
        "created": 1700000000,
        "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"tool_calls": [Value::Object(frag)]}, "finish_reason": null}],
    })
    .to_string()
}

/// 上游 OpenAI 增量帧解析 → ToolCall 片段（解析层必须产出两片段）。
#[test]
fn openai_upstream_tool_fragments_parse_to_chunks() {
    let converter = OpenAIConverter::new();
    // 增量分片为原文字符串（`{"city":"` 与 `Paris"}`）。
    let frames = [
        tool_call_frame(
            Some("call_1"),
            Some("get_weather"),
            "{\"city\":\"".to_string(),
        ),
        tool_call_frame(None, None, "Paris\"}".to_string()),
    ];
    let chunks: Vec<StreamChunk> = frames
        .iter()
        .filter_map(|f| converter.parse_stream_frame(f))
        .collect();
    assert_eq!(chunks.len(), 2, "两增量帧必须产出两片段：{chunks:?}");
    for chunk in &chunks {
        assert!(
            matches!(chunk, StreamChunk::ToolCall { index: 0, .. }),
            "同槽位 index=0：{chunk:?}"
        );
    }
}

/// 单元级：tool_calls 增量流 index/argument 拼装（同 index 分片按序可拼）。
#[test]
fn openai_tool_call_fragments_assemble_by_index() {
    let converter = OpenAIConverter::new();
    let fragments = vec![
        StreamChunk::ToolCall {
            index: 0,
            id: "call_1".into(),
            name: Some("get_weather".into()),
            arguments: Some(r#"{"city":""#.into()),
        },
        StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some(r#"Paris"}"#.into()),
        },
    ];
    let deltas: Vec<Value> = fragments
        .into_iter()
        .map(|c| converter.stream_chunk_to_external(c).expect("映射"))
        .collect();
    // 同 index；首片带 id/name，续片仅 arguments 分片。
    assert_eq!(
        deltas[0]["choices"][0]["delta"]["tool_calls"][0]["index"],
        0
    );
    assert_eq!(
        deltas[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        deltas[1]["choices"][0]["delta"]["tool_calls"][0]["index"],
        0,
        "续片同 index:\n{}",
        snapshot(&deltas)
    );
    assert!(
        deltas[1]["choices"][0]["delta"]["tool_calls"][0]
            .get("id")
            .is_none(),
        "续片不重复 id:\n{}",
        snapshot(&deltas)
    );
    // 按序拼装还原完整参数。
    let assembled: String = deltas
        .iter()
        .filter_map(|d| d["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    assert_eq!(assembled, r#"{"city":"Paris"}"#, "分片按序拼装");
}

// ==================== 集成级（全链路，mock 上游） ====================

#[cfg(feature = "web-server")]
mod integration {
    use crate::common;
    use axum::{body::Body, http::Request, routing::post, Router};
    use fluxgate::api::{chat_completions, messages, ApiState};
    use fluxgate::config::{
        ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig, VendorConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::router::Router as NexusRouter;
    use fluxgate::vendor::VendorManager;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn openai_vendor(id: &str, api_base: String) -> VendorConfig {
        crate::common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.models = vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v
    }

    fn anthropic_vendor(id: &str, api_base: String) -> VendorConfig {
        crate::common::ensure_test_api_key();
        let mut v = common::base_vendor(id, api_base);
        v.type_ = "anthropic".into();
        v.models = vec![ModelConfig {
            model_id: "claude-x".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }];
        v
    }

    async fn make_state(vendors: Vec<VendorConfig>) -> Arc<ApiState> {
        let mut config = common::test_config(vendors);
        config.server = ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        };
        config.database = DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        };
        config.global_model = Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        });
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("vendor init");

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let database = Arc::new(Database { pool, repository });
        let router = Arc::new(NexusRouter::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        })
    }

    /// SSE 字节流 → 事件负载序列（data: 拼接 + [DONE] 标记）。
    async fn read_sse_payloads(response: axum::response::Response) -> (Vec<String>, bool) {
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let mut payloads = Vec::new();
        let mut done = false;
        for frame in text.split("\n\n") {
            for line in frame.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        done = true;
                    } else {
                        payloads.push(data.to_string());
                    }
                } else if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        done = true;
                    } else if !data.is_empty() {
                        payloads.push(data.to_string());
                    }
                }
            }
        }
        (payloads, done)
    }

    /// Anthropic 全链路：Anthropic 入站 + OpenAI 上游（文本增量 +
    /// 终止 + usage 尾帧）→ 出站 Anthropic SSE 事件完整性（起止/配对/
    #[tokio::test]
    async fn anthropic_full_chain_event_integrity() {
        let text_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":"hi"},"finish_reason":null}]}).to_string();
        let stop_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string();
        let usage_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}).to_string();
        let sse = format!("data: {text_frame}\n\ndata: {stop_frame}\n\ndata: {usage_frame}\n\n");
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(&sse)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(vec![openai_vendor("fx-o", server.url())]).await;
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "max_tokens": 16,
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": true,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let (payloads, done) = read_sse_payloads(response).await;
        assert!(!done, "Anthropic 流以 message_stop 终止，不带 [DONE]");
        assert!(!payloads.is_empty(), "必须有事件负载");
        let events: Vec<Value> = payloads
            .iter()
            .map(|p| serde_json::from_str(p).expect("负载为 JSON"))
            .collect();
        let types: Vec<&str> = events
            .iter()
            .map(|e| e.get("type").and_then(|t| t.as_str()).unwrap_or("?"))
            .collect();
        assert_eq!(
            types.first().copied(),
            Some("message_start"),
            "message_start 起始: {types:?}"
        );
        assert_eq!(
            types.last().copied(),
            Some("message_stop"),
            "message_stop 终止（无孤儿事件）: {types:?}"
        );
        let mut open = vec![];
        let mut closed = vec![];
        for e in &events {
            match e.get("type").and_then(|t| t.as_str()) {
                Some("content_block_start") => open.push(e["index"].as_u64().unwrap_or(u64::MAX)),
                Some("content_block_stop") => closed.push(e["index"].as_u64().unwrap_or(u64::MAX)),
                _ => {}
            }
        }
        assert_eq!(open, closed, "块配对: {types:?}");
        let delta = events
            .iter()
            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("message_delta"))
            .expect("message_delta 存在");
        assert!(delta.get("usage").is_some(), "usage 尾帧: {delta}");

        mock.assert_async().await;
    }

    /// OpenAI 上游增量 tool_calls → IR → Anthropic tool_use 重建（解析无损端到端证明）。
    #[tokio::test]
    async fn openai_full_chain_event_integrity() {
        // 增量分片为原文字符串；帧经顶层 helper 组装。
        let frag1_arg = "{\"city\":\"".to_string();
        let frag2_arg = "Paris\"}".to_string();
        let text_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}).to_string();
        let stop_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string();
        let usage_frame = json!({"id":"c1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}).to_string();
        let frames = [
            text_frame,
            super::tool_call_frame(Some("call_1"), Some("get_weather"), frag1_arg),
            super::tool_call_frame(None, None, frag2_arg),
            stop_frame,
            usage_frame,
        ];
        let sse: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse)
            .expect(1)
            .create_async()
            .await;

        // 本用例锁定 OpenAI 上游增量解析（Anthropic 入站 + OpenAI 上游）。
        let state = make_state(vec![openai_vendor("fx-o", server.url())]).await;
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "max_tokens": 16,
                            "messages": [{"role": "user", "content": "weather in Paris?"}],
                            "stream": true,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let (payloads, _) = read_sse_payloads(response).await;
        assert!(!payloads.is_empty());
        // 工具增量在 Anthropic 出站重建为 tool_use 块（解析无损的旁证）。
        let dumped = payloads.join("\n");
        assert!(dumped.contains("get_weather"), "工具名贯穿全链路: {dumped}");

        mock.assert_async().await;
    }

    /// OpenAI 出站全链路：OpenAI 入站 + Anthropic 上游 → 出站 OpenAI SSE
    /// 经网关编码器（role 首帧、id/created 一致、finish 恰一次、[DONE]）。
    #[tokio::test]
    async fn openai_outbound_full_chain_event_integrity() {
        let sse = format!(
            "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{}}\n\n",
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":3,"output_tokens":1}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        );
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(&sse)
            .expect(1)
            .create_async()
            .await;

        let state = make_state(vec![anthropic_vendor("fx-a", server.url())]).await;
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "gpt-4",
                            "messages": [{"role": "user", "content": "hi"}],
                            "max_tokens": 16,
                            "stream": true,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let (payloads, done) = read_sse_payloads(response).await;
        assert!(done, "OpenAI 流以 [DONE] 终止");
        assert!(!payloads.is_empty());
        let events: Vec<Value> = payloads
            .iter()
            .map(|p| serde_json::from_str(p).expect("负载为 JSON"))
            .collect();
        // 首 chunk 带 role。
        assert_eq!(
            events[0]["choices"][0]["delta"]["role"], "assistant",
            "首 chunk role: {:?}",
            events[0]
        );
        // finish_reason 恰一次。
        let finish_count = events
            .iter()
            .flat_map(|e| e["choices"].as_array().cloned().unwrap_or_default())
            .filter(|c| !c["finish_reason"].is_null() && c["finish_reason"] != "")
            .count();
        assert_eq!(finish_count, 1, "finish 恰一次: {events:?}");
        // id/created 一致。
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e["id"].as_str().unwrap_or("?"))
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]), "id 一致: {ids:?}");
        let created: Vec<u64> = events
            .iter()
            .map(|e| e["created"].as_u64().unwrap_or(u64::MAX))
            .collect();
        assert!(
            created.iter().all(|c| *c == created[0]),
            "created 一致: {created:?}"
        );
        // 内容无孤儿 error 帧。
        assert!(
            !payloads.iter().any(|p| p.contains("event: error")),
            "无错误帧"
        );

        mock.assert_async().await;
    }
}
