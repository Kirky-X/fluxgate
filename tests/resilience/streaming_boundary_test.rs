// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流式失败语义边界测试。边界 =「尚未向客户端写出任何字节」：
//! - 首字节前上游失败（429 / 连接错误 / 5xx）→ 走换路链重派，客户端无感
//!   （5xx 形态已由 fallback_test::stream_fails_over_before_first_byte 锁定）；
//! - 首字节后上游断流 → 禁止换路，客户端恰收一个网关错误帧
//!   （data: {"error":{...}} JSON 信封，错误码走 i18n、不泄内部拓扑）、
//!   无 [DONE]、内容不重放，且该 vendor 进入冷却（Throttled，TTL 到期
//!   惰性恢复——复用 [resilience].circuit_open_timeout_ms 窗口）。
//!
//! 断流桩为裸 TCP 上游：content-length 虚高 + 写出一帧后半途 drop，
//! 客户端 hyper 侧得到 incomplete message（真实断流形态；mockito 无法
//! 表达中流断开）。模式参照 sse_test::spawn_delayed_server。

#![cfg(all(feature = "resilience", feature = "web-server"))]

#[path = "../common/mod.rs"]
mod common;

use axum::{body::Body, http::Request, routing::post, Router};
use dbnexus::{DbConfig, DbPool};
use fluxgate::api::{chat_completions, messages, ApiState};
use fluxgate::config::{
    Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ResilienceSection,
    ServerConfig, VendorConfig,
};
use fluxgate::database::{Database, Repository};
use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
use fluxgate::router::Router as NexusRouter;
use fluxgate::vendor::{VendorManager, VendorStatus};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// openai 型 vendor（透传路径：入站 OpenAI == 出站 OpenAI）。
/// api_base 不带 /v1 —— manager 会拼 `{base}/v1/chat/completions`。
fn make_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
    crate::common::ensure_test_api_key();
    VendorConfig {
        timeout_ms: 60_000,
        stream_ttft_ms: 60_000,
        stream_idle_ms: 30_000,
        id: id.into(),
        supports_batch: false,
        chat_path: None,
        api_style: None,
        name: format!("Test {id}"),
        type_: "openai".into(),
        api_key_env: "FLUXGATE_TEST_KEY".into(),
        api_key_envs: Vec::new(),
        extra_headers: Default::default(),
        api_base: api_base.into(),
        enabled: true,
        priority: 1,
        models: vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }],
        rate_limit: fluxgate::config::RateLimitConfig::Token {
            max_tokens: 1_000_000,
            window_hours: None,
            sliding: false,
        },
        concurrency: fluxgate::config::ConcurrencyConfig {
            requests_per_second: 1000,
            max_concurrent_requests: None,
            token_weighted: false,
            token_ticket_min: 1,
            token_ticket_max: 16384,
            token_ticket_default: 4096,
        },
        anthropic_version: None,
        forward_beta_headers: false,
        send_nonstandard_params: false,
    }
}

/// anthropic 型 vendor（转换路径：入站 OpenAI/出站 Anthropic 双向经
/// 网关转换器重建）。api_base 不带版本段——manager 按类型拼
/// `{base}/v1/messages`。
fn make_anthropic_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
    crate::common::ensure_test_api_key();
    VendorConfig {
        type_: "anthropic".into(),
        ..make_vendor(id, api_base)
    }
}

fn make_config(vendors: Vec<VendorConfig>, circuit_open_timeout_ms: u64) -> Config {
    Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
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
        retry: fluxgate::config::RetrySection::default(),
        routing: fluxgate::config::RoutingSection::default(),
        cache: fluxgate::config::CacheSection::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: ResilienceSection {
            cover_all_paths: true,
            circuit_open_timeout_ms,
            ..Default::default()
        },
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
    }
}

async fn make_state(config: Config) -> Arc<ApiState> {
    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await.expect("vendor init");

    let pool = DbPool::with_config(DbConfig {
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
    let repository = Arc::new(Repository::new(pool.clone()));
    repository.initialize().await.expect("schema init");
    let database = Arc::new(Database {
        pool,
        repository: repository.clone(),
    });
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
        responses: None,
    })
}

fn app(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .with_state(state)
}

async fn post_chat(
    state: Arc<ApiState>,
    vendor_hint: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(v) = vendor_hint {
        builder = builder.header("x-vendor", v);
    }
    app(state)
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_messages(state: Arc<ApiState>, body: Value) -> axum::response::Response {
    app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn chat_body(stream: bool) -> Value {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 10,
        "stream": stream
    })
}

async fn read_body(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn request_content_length(head: &str) -> usize {
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                return v.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// 裸 TCP 流式上游：前 `broken_conns` 个连接在发出 `broken_body` 后突断
///（content-length 虚高 + 半途 drop → 客户端 hyper 侧报 incomplete
/// message），此后每个连接返回 `ok_body`。返回 (base_url, 连接计数)。
/// 请求体按 content-length 读满后再写响应，防半途 drop 触发 RST 吞掉
/// 已写出的响应字节。帧内容参数化：透传路径与两条转换路径（Anthropic
/// 出站 / OpenAI 出站）各自需要协议形似的断流前帧。
async fn spawn_stream_upstream_with(
    broken_conns: usize,
    broken_body: &str,
    ok_body: &str,
) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (broken_body, ok_body) = (broken_body.to_string(), ok_body.to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    let served_task = served.clone();
    tokio::spawn(async move {
        let mut idx = 0usize;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let conn = idx;
            idx += 1;
            served_task.fetch_add(1, Ordering::SeqCst);

            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = socket.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head_end = pos + 4;
                let declared = request_content_length(&String::from_utf8_lossy(&buf[..head_end]));
                if buf.len() >= head_end + declared {
                    break;
                }
            }

            let body = if conn < broken_conns {
                broken_body.to_string()
            } else {
                ok_body.to_string()
            };
            let declared_len = if conn < broken_conns {
                4096
            } else {
                body.len()
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {declared_len}\r\n\r\n"
            );
            socket.write_all(head.as_bytes()).await.ok();
            socket.write_all(body.as_bytes()).await.ok();
            socket.flush().await.ok();
            // 突断连接持有一瞬再 drop，确保客户端先收到已写字节
            if conn < broken_conns {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            drop(socket);
        }
    });
    (format!("http://{addr}"), served)
}

async fn spawn_stream_upstream(broken_conns: usize) -> (String, Arc<AtomicUsize>) {
    spawn_stream_upstream_with(
        broken_conns,
        "data: {\"id\":\"partial-1\"}\n\n",
        "data: {\"id\":\"recovered\"}\n\ndata: [DONE]\n\n",
    )
    .await
}

///：首跳上游 429（首字节前失败）→ 换路
/// 第二 vendor 建流，客户端拿到完整正常 SSE，响应不含任何错误帧。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_hop_429_fails_over_stream_before_first_byte() {
    let mut server_a = mockito::Server::new_async().await;
    let mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(429)
        .with_body("rate limited")
        .expect(1)
        .create_async()
        .await;
    let mut server_b = mockito::Server::new_async().await;
    let mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"b-sse\"}\n\ndata: [DONE]\n\n")
        .expect(1)
        .create_async()
        .await;

    let state = make_state(make_config(
        vec![
            make_vendor("rl-a", server_a.url()),
            make_vendor("rl-b", server_b.url()),
        ],
        30_000,
    ))
    .await;

    let response = post_chat(state, Some("rl-a"), chat_body(true)).await;
    assert_eq!(response.status(), 200, "首跳 429 必须换路到 B 成功");
    let body = read_body(response).await;
    assert!(body.contains("b-sse"), "客户端必须拿到 B 的流内容: {body}");
    assert!(
        !body.contains("event: error"),
        "换路成功不得出现错误帧: {body}"
    );

    mock_a.assert_async().await;
    mock_b.assert_async().await;
}

///：首跳连接错误（连接拒绝，首字节前）→
/// 换路第二 vendor，客户端无感。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_hop_connection_error_fails_over_stream_before_first_byte() {
    let mut server_b = mockito::Server::new_async().await;
    let mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"b-sse\"}\n\ndata: [DONE]\n\n")
        .expect(1)
        .create_async()
        .await;

    let state = make_state(make_config(
        vec![
            // 127.0.0.1:1 连接拒绝（既有测试同款死端口形态）
            make_vendor("dead-a", "http://127.0.0.1:1"),
            make_vendor("live-b", server_b.url()),
        ],
        30_000,
    ))
    .await;

    let response = post_chat(state, Some("dead-a"), chat_body(true)).await;
    assert_eq!(response.status(), 200, "首跳连接错误必须换路到 B 成功");
    let body = read_body(response).await;
    assert!(body.contains("b-sse"), "客户端必须拿到 B 的流内容: {body}");
    assert!(
        !body.contains("event: error"),
        "换路成功不得出现错误帧: {body}"
    );

    mock_b.assert_async().await;
}

///：流建立后上游断流 → 客户端恰收
/// 1 个网关错误帧（data 为 {"error":{type,message}} JSON 信封、无
/// vendor id/api_base 内部字段）、0 个 [DONE]、已收内容不重放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mid_stream_break_single_json_error_frame_no_done_no_replay() {
    let (base_a, _served) = spawn_stream_upstream(1).await;
    let state = make_state(make_config(vec![make_vendor("brk-a", base_a)], 30_000)).await;

    let response = post_chat(state, None, chat_body(true)).await;
    assert_eq!(response.status(), 200, "流已建立 → 200 SSE");
    let body = read_body(response).await;

    assert_eq!(
        body.matches("event: error").count(),
        1,
        "必须恰有一个错误帧: {body}"
    );
    assert!(!body.contains("[DONE]"), "错误终止不得补 [DONE]: {body}");
    assert_eq!(
        body.matches("partial-1").count(),
        1,
        "已收内容不得重放: {body}"
    );

    // 错误帧 data 为 JSON 信封，与 REST 错误体同构；message 为 i18n
    // 文案，帧内无内部拓扑（vendor id / api_base）
    let payload = body
        .split_once("event: error\ndata: ")
        .map(|(_, rest)| rest.split("\n\n").next().unwrap_or_default())
        .unwrap_or_default();
    let frame: Value = serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("错误帧 data 必须是 JSON: {e}: {payload}"));
    assert_eq!(
        frame["error"]["type"], "upstream_stream_interrupted",
        "错误码为稳定字面量: {payload}"
    );
    assert!(
        frame["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "message 走 i18n 文案: {payload}"
    );
    assert!(
        !body.contains("brk-a") && !body.contains("127.0.0.1"),
        "错误帧不得泄漏 vendor id / api_base: {body}"
    );
}

///：断流触发该 vendor 冷却 —— 后续请求
/// 立即可观测 Throttled（不再触达该上游）；冷却 TTL（此处取
/// circuit_open_timeout_ms=300 短窗）到期后惰性恢复 Available，流量
/// 重新可达。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mid_stream_break_cooldowns_vendor_until_ttl_expiry() {
    let (base_a, served) = spawn_stream_upstream(1).await;
    let state = make_state(make_config(vec![make_vendor("cd-a", base_a)], 300)).await;

    let response = post_chat(state.clone(), None, chat_body(true)).await;
    assert_eq!(response.status(), 200);
    let body = read_body(response).await;
    assert!(body.contains("event: error"), "断流以错误帧终止: {body}");
    assert_eq!(served.load(Ordering::SeqCst), 1);

    assert_eq!(
        state.vendor_manager.get_status("cd-a").await,
        VendorStatus::Throttled,
        "断流后该 vendor 必须进入冷却（Throttled）"
    );

    // 冷却期内的后续请求：不再触达该上游（连接计数停在 1）
    let response = post_chat(state.clone(), None, chat_body(true)).await;
    assert!(
        !response.status().is_success(),
        "冷却期内后续请求不得放行触达已冷却 vendor"
    );
    assert_eq!(
        served.load(Ordering::SeqCst),
        1,
        "冷却期内上游不得被再次触达"
    );

    // TTL 到期惰性恢复 → 流量重新可达（连接 2 返回完整响应）
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        state.vendor_manager.get_status("cd-a").await,
        VendorStatus::Available,
        "冷却 TTL 到期必须恢复 Available（避免永久排除死锁）"
    );
    let response = post_chat(state, None, chat_body(true)).await;
    assert_eq!(response.status(), 200);
    let body = read_body(response).await;
    assert!(
        body.contains("recovered") && !body.contains("event: error"),
        "恢复后的流完整正常: {body}"
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

/// 错误帧 data 断言公共段：JSON 信封、稳定错误码、i18n message。
fn assert_gateway_error_envelope(payload: &str) -> Value {
    let frame: Value = serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("错误帧 data 必须是 JSON: {e}: {payload}"));
    assert_eq!(
        frame["error"]["type"], "upstream_stream_interrupted",
        "错误码为稳定字面量: {payload}"
    );
    assert!(
        frame["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "message 走 i18n 文案: {payload}"
    );
    frame
}

fn error_frame_payload(body: &str) -> &str {
    body.split_once("event: error\ndata: ")
        .map(|(_, rest)| rest.split("\n\n").next().unwrap_or_default())
        .unwrap_or_default()
}

///（转换路径 · Anthropic 出站）：openai 型
/// 上游断流前先发一帧可映射内容，/v1/messages 的转换流以恰一个网关
/// 错误帧终止——错误帧后不得补 message_stop 等收尾事件（客户端可分辨
/// 失败），已收内容不重放，帧内无内部拓扑。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn converted_anthropic_stream_break_single_error_frame_no_trailing_stop() {
    let partial = json!({
        "id": "c1",
        "object": "chat.completion.chunk",
        "created": 1700000000,
        "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "slice-1"}, "finish_reason": null}]
    })
    .to_string();
    let (base_a, _served) = spawn_stream_upstream_with(
        1,
        &format!("data: {partial}\n\n"),
        "data: {\"id\":\"recovered\"}\n\ndata: [DONE]\n\n",
    )
    .await;
    let state = make_state(make_config(vec![make_vendor("cv-a", base_a)], 30_000)).await;

    let response = post_messages(state, chat_body(true)).await;
    assert_eq!(response.status(), 200, "流已建立 → 200 SSE");
    let body = read_body(response).await;

    assert!(
        body.contains("message_start"),
        "转换流以生命周期事件起始: {body}"
    );
    assert_eq!(
        body.matches("event: error").count(),
        1,
        "必须恰有一个错误帧: {body}"
    );
    assert!(
        !body.contains("message_stop"),
        "错误终止不得补 message_stop 收尾: {body}"
    );
    assert_eq!(
        body.matches("slice-1").count(),
        1,
        "已收内容不得重放: {body}"
    );

    let frame = assert_gateway_error_envelope(error_frame_payload(&body));
    assert!(
        frame["error"]["message"].as_str().is_some(),
        "错误帧信封完整: {frame}"
    );
    assert!(
        !body.contains("cv-a") && !body.contains("127.0.0.1"),
        "错误帧不得泄漏 vendor id / api_base: {body}"
    );
}

///（转换路径 · OpenAI 出站）：anthropic 型
/// 上游断流前先发一帧可映射内容，/v1/chat/completions 的转换流以恰一个
/// 网关错误帧终止、不补 [DONE]、已收内容不重放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn converted_openai_stream_break_single_error_frame_no_done() {
    let partial = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": "slice-1"}
    })
    .to_string();
    let (base_a, _served) = spawn_stream_upstream_with(
        1,
        &format!("event: content_block_delta\ndata: {partial}\n\n"),
        "event: message_stop\ndata: {}\n\n",
    )
    .await;
    let state = make_state(make_config(
        vec![make_anthropic_vendor("ca-a", base_a)],
        30_000,
    ))
    .await;

    let response = post_chat(state, None, chat_body(true)).await;
    assert_eq!(response.status(), 200, "流已建立 → 200 SSE");
    let body = read_body(response).await;

    assert!(body.contains("slice-1"), "断流前内容已转发: {body}");
    assert_eq!(
        body.matches("event: error").count(),
        1,
        "必须恰有一个错误帧: {body}"
    );
    assert!(!body.contains("[DONE]"), "错误终止不得补 [DONE]: {body}");
    assert_eq!(
        body.matches("slice-1").count(),
        1,
        "已收内容不得重放: {body}"
    );
    assert!(
        !body.contains("ca-a") && !body.contains("127.0.0.1"),
        "错误帧不得泄漏 vendor id / api_base: {body}"
    );
    assert_gateway_error_envelope(error_frame_payload(&body));
}
