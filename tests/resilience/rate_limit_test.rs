// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(feature = "web-server")]

//! 入站限流落地测试。本文件锁定：
//! - [[rate_limit.rules]] 配置解析与生效；
//! - per-endpoint 规则（path 前缀）经中间件端点表生效：超限 429 +
//!   Retry-After、租户分桶（API key SHA256）、匿名归入单一 anonymous 桶、
//!   健康检查白名单放行；
//! - 全局规则（path 缺省）注入 limiteron Governor 生效；
//! - rules 为空 = Governor 默认兜底 + 无端点表。

use axum::{body::Body, http::Request, http::StatusCode, Router};
use dbnexus::{DbConfig, DbPool};
use nexusflow::{
    api::create_router,
    config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, RateLimitRule, RateLimitSection,
        ServerConfig, VendorConfig,
    },
    database::Database,
    protocol::{AnthropicConverter, OpenAIConverter},
    vendor::VendorManager,
    ApiState,
};
use std::sync::Arc;
use tower::ServiceExt;

#[path = "../common/mod.rs"]
mod common;

// ==================== 配置解析 ====================

/// [[rate_limit.rules]] 解析：字段形态与 per_tenant 缺省 true（Red：
/// Config 无 rate_limit 字段时本测试编译失败）。
#[test]
fn config_parses_rate_limit_rules() {
    let content = r#"
[server]
host = "127.0.0.1"
port = 1

[database]
path = ":memory:"

[[rate_limit.rules]]
id = "chat-tenant"
path = "/v1/chat/completions"
per_tenant = true
window_secs = 60
max_requests = 100

[[rate_limit.rules]]
id = "global-cap"
window_secs = 3600
max_requests = 5000

[[vendors]]
id = "v"
name = "V"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "token", max_tokens = 1 }
concurrency = { requests_per_second = 1 }
"#;
    let config: Config = toml::from_str(content).unwrap();
    assert_eq!(config.rate_limit.rules.len(), 2);
    let r0 = &config.rate_limit.rules[0];
    assert_eq!(r0.id, "chat-tenant");
    assert_eq!(r0.path.as_deref(), Some("/v1/chat/completions"));
    assert!(r0.per_tenant);
    assert_eq!(r0.window_secs, 60);
    assert_eq!(r0.max_requests, 100);
    // per_tenant 缺省 true；path 缺省 = 全局规则（进 Governor）
    assert!(config.rate_limit.rules[1].per_tenant);
    assert_eq!(config.rate_limit.rules[1].path, None);
}

// ==================== 运行时行为 ====================

fn rate_rule(id: &str, path: Option<&str>, max_requests: u64) -> RateLimitRule {
    RateLimitRule {
        id: id.into(),
        path: path.map(Into::into),
        per_tenant: true,
        window_secs: 60,
        max_requests,
    }
}

fn make_config(rules: Vec<RateLimitRule>) -> Config {
    // vendors 为空：handler 走路由失败路径（非 429 响应），不影响限流断言
    make_config_with(rules, vec![])
}

fn make_config_with(rules: Vec<RateLimitRule>, vendors: Vec<VendorConfig>) -> Config {
    Config {
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
        // auth: None → 认证关闭，全部请求透传至限流中间件（租户维度按
        // 请求携带的 Bearer key 摘要分桶，无 key 归入 anonymous）
        auth: None,
        cors: None,
        retry: nexusflow::config::RetrySection::default(),
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: nexusflow::config::ResilienceSection::default(),
        docs: Default::default(),
        rate_limit: RateLimitSection { rules },
    }
}

async fn build_app(rules: Vec<RateLimitRule>) -> Router {
    build_app_with_vendors(rules, vec![]).await
}

async fn build_app_with_vendors(rules: Vec<RateLimitRule>, vendors: Vec<VendorConfig>) -> Router {
    let config_manager = Arc::new(ConfigManager::new(make_config_with(rules, vendors)));
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

    let database = Arc::new(Database {
        pool: pool.clone(),
        repository: Arc::new(nexusflow::database::Repository::new(pool)),
    });

    let router = Arc::new(nexusflow::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));

    let state = Arc::new(ApiState {
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
    });

    create_router(state).await
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: &str,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        builder = builder.header("Authorization", format!("Bearer {k}"));
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

const CHAT_BODY: &str = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;

/// per-endpoint 规则：max_requests=2 下同租户第 3 个请求 429 且带
/// Retry-After（表内 limiter 的 reset_secs）。
#[tokio::test]
async fn endpoint_rule_rejects_third_request_with_429_and_retry_after() {
    let app = build_app(vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 2)]).await;

    for i in 0..2 {
        let resp = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request {i} must pass the endpoint rule"
        );
    }

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("tenant-a"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = resp
        .headers()
        .get("Retry-After")
        .expect("429 must carry Retry-After");
    let secs: u64 = retry_after.to_str().unwrap().parse().unwrap();
    assert!(secs >= 1, "Retry-After must be positive, got {secs}");
}

/// 租户隔离：tenant-a 打满配额后，不同 Bearer key（tenant-b）不受影响。
#[tokio::test]
async fn different_tenant_is_not_affected_by_other_tenant_quota() {
    let app = build_app(vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 2)]).await;

    for _ in 0..2 {
        send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
    }

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("tenant-b"),
        CHAT_BODY,
    )
    .await;
    assert_ne!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "tenant-b must have its own bucket"
    );
}

/// path 前缀只约束匹配端点：chat 配额打满后 /v1/messages 不受该规则约束。
#[tokio::test]
async fn path_prefix_scopes_rule_to_matching_endpoint() {
    let app = build_app(vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 2)]).await;

    for _ in 0..2 {
        send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
    }

    let resp = send(
        app,
        "POST",
        "/v1/messages",
        Some("tenant-a"),
        r#"{"model":"claude-2","messages":[]}"#,
    )
    .await;
    assert_ne!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "/v1/messages must not be scoped by the /v1/chat/completions rule"
    );
}

/// 健康检查白名单断言位于 src/api/router_builder.rs 内联单测（axum 0.8 的
/// layer 只作用于已有路由，create_router 返回后再 .route 的路径不经中间件，
/// 集成形态存在断言空洞）。匿名语义锁定：无 Authorization 头的请求归入
/// 单一 anonymous 桶，第 3 个请求 429
#[tokio::test]
async fn anonymous_requests_share_single_bucket() {
    let app = build_app(vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 2)]).await;

    for i in 0..2 {
        let resp = send(app.clone(), "POST", "/v1/chat/completions", None, CHAT_BODY).await;
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "anonymous request {i} must pass"
        );
    }

    let resp = send(app, "POST", "/v1/chat/completions", None, CHAT_BODY).await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "all keyless requests must share the single anonymous bucket"
    );
}

/// 全局规则（path 缺省）注入 limiteron Governor（Matcher::User）生效。
#[tokio::test]
async fn global_rule_enforced_through_governor() {
    let app = build_app(vec![rate_rule("global-cap", None, 2)]).await;

    for i in 0..2 {
        let resp = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request {i} must pass the Governor rule"
        );
    }

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("tenant-a"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "global rule must be enforced through the Governor"
    );
}

/// 空 rules = 现状逐位：Governor 默认兜底（TokenBucket 100/10）+ 无端点表，
/// 3 个同租户请求不会触发 429（冒烟）。
#[tokio::test]
async fn empty_rules_keep_default_governor_behavior() {
    let app = build_app(vec![]).await;

    for i in 0..3 {
        let resp = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request {i}: empty rules must keep the default Governor (no early 429)"
        );
    }
}

/// 新契约：全局 tower 层拒绝与 per-endpoint 429 同形——429 +
/// Retry-After（窗口剩余秒）+ `router-rate-limit-exceeded` 体。
#[tokio::test]
async fn global_rule_rejection_carries_retry_after_and_standard_body() {
    let app = build_app(vec![rate_rule("global-cap", None, 2)]).await;

    for _ in 0..2 {
        let resp = send(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            Some("tenant-a"),
            CHAT_BODY,
        )
        .await;
        assert_ne!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("tenant-a"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = resp
        .headers()
        .get("Retry-After")
        .expect("global 429 must carry Retry-After");
    let secs: u64 = retry_after.to_str().unwrap().parse().unwrap();
    assert!(
        (1..60).contains(&secs),
        "Retry-After must be the remaining window seconds (window 60s), got {secs} — \
         整窗回法会给 60"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        body,
        nexusflow::i18n::t("router-rate-limit-exceeded"),
        "global 429 body must match the standard rate-limit body"
    );
}

/// 精确性：窗口内已流逝 ≥1s 后拒绝，Retry-After 必须随窗口递减
/// （= 精确剩余秒），而非恒回整窗——遵守该头的客户端/agent 因此不白等
/// 近一整窗。
#[tokio::test]
async fn global_rule_retry_after_tracks_window_remaining() {
    let app = build_app(vec![rate_rule("global-precise", None, 1)]).await;

    let first = send(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        Some("tenant-a"),
        CHAT_BODY,
    )
    .await;
    assert_ne!(
        first.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "窗口内首请求应放行"
    );

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("tenant-a"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let secs: u64 = resp
        .headers()
        .get("Retry-After")
        .expect("global 429 must carry Retry-After")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // 窗口 60s、已流逝 ~1.2s → 剩余 ≈58.8s（向下取整 58）；断言 <60 即证
    // 取值随窗口递减而非恒回整窗（慢机抖动只会让值更小）。
    assert!(
        (1..60).contains(&secs),
        "Retry-After 应为窗口剩余秒，实测 {secs}（整窗回法会给 60）"
    );
}

/// 入口侧断言：stream=true 的请求同样只在
/// 请求入口裁决——首个请求消耗配额（handler 路由失败非 429），第二个
/// 请求入口即 429。限流不进入流生命周期，不存在「流中途限流」形态。
#[tokio::test]
async fn stream_request_is_adjudicated_at_entry_only() {
    let app = build_app(vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 1)]).await;
    let stream_body =
        r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"stream":true}"#;

    let resp = send(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        Some("sk-test-stream-entry"),
        stream_body,
    )
    .await;
    assert_ne!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "首个流式请求必须在入口放行（vendors 为空 → 路由失败非 429）"
    );

    let resp = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("sk-test-stream-entry"),
        stream_body,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "配额打满后的流式请求必须在入口被 429 拒绝"
    );
}

/// 装配顺序锁定：认证在限流块外层——全局配额打满后，无效 key 的请求仍
/// 先被认证拒绝（401），而非限流 429（若 tower 层跑到认证外层，此处将
/// 得到 429）。
#[tokio::test]
async fn auth_runs_before_rate_limit_layers() {
    use nexusflow::config::AuthSection;
    let mut config = make_config(vec![rate_rule("global-cap", None, 1)]);
    config.auth = Some(AuthSection {
        enabled: true,
        // sha256("test-good-key")
        api_keys: vec![
            "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303".to_string(),
        ],
        header: Default::default(),
        keys: Vec::new(),
    });
    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
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
    let database = Arc::new(Database {
        pool: pool.clone(),
        repository: Arc::new(nexusflow::database::Repository::new(pool)),
    });
    let router = Arc::new(nexusflow::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let state = Arc::new(ApiState {
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
    });
    let app = create_router(state).await;

    // 好 key 第 1 次通过（非 429），第 2 次打满全局配额 → 429
    let first = send(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        Some("test-good-key"),
        CHAT_BODY,
    )
    .await;
    assert_ne!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let second = send(
        app.clone(),
        "POST",
        "/v1/chat/completions",
        Some("test-good-key"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    // 配额已空：坏 key 仍走认证 401（认证在 tower 层外侧）
    let bad = send(
        app,
        "POST",
        "/v1/chat/completions",
        Some("wrong-key"),
        CHAT_BODY,
    )
    .await;
    assert_eq!(
        bad.status(),
        StatusCode::UNAUTHORIZED,
        "auth must run outside the rate-limit layers"
    );
}

// ==================== 已放行的流不因限流中断 ====================

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

/// 裸 TCP 流式上游（带放闸）：读满请求后写 200 SSE 头 + 首帧，停在
/// gate 上；`notify_one` 放闸后才续写尾段并断连——让「已放行的流」
/// 确定性地处于在途状态。响应无 content-length，以连接关闭定界。
async fn spawn_gated_stream_upstream() -> (String, Arc<tokio::sync::Notify>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let gate_task = gate.clone();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
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
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
            .await
            .ok();
        socket
            .write_all(b"data: {\"id\":\"first-half\"}\n\n")
            .await
            .ok();
        socket.flush().await.ok();
        gate_task.notified().await;
        socket
            .write_all(b"data: {\"id\":\"second-half\"}\n\ndata: [DONE]\n\n")
            .await
            .ok();
        socket.flush().await.ok();
    });
    (format!("http://{addr}"), gate)
}

fn stream_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
    common::ensure_test_api_key();
    common::base_vendor(id, api_base)
}

/// cap=1 端点规则下：首个流式请求放行且在途（上游发完首帧后停闸），
/// 此时窗口已耗尽（第二个请求入口 429）；随后放闸，在途流仍完整收束
///（两段内容 + [DONE]、无错误帧）——限流裁决不存在流生命周期内的
/// 检查点。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_stream_completes_while_entry_quota_exhausted() {
    let (base, gate) = spawn_gated_stream_upstream().await;
    let app = build_app_with_vendors(
        vec![rate_rule("chat-cap", Some("/v1/chat/completions"), 1)],
        vec![stream_vendor("gate-a", base)],
    )
    .await;
    let stream_body =
        r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
    let post_stream = |app: Router| {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer sk-test-admitted")
                .body(Body::from(stream_body.to_string()))
                .unwrap(),
        )
    };

    let admitted = post_stream(app.clone()).await.unwrap();
    assert_eq!(
        admitted.status(),
        StatusCode::OK,
        "首个流式请求必须入口放行且建流"
    );

    let rejected = post_stream(app.clone()).await.unwrap().status();
    assert_eq!(
        rejected,
        StatusCode::TOO_MANY_REQUESTS,
        "窗口耗尽后的新请求必须入口 429"
    );

    gate.notify_one();

    let bytes = axum::body::to_bytes(admitted.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("first-half"), "断流前首段内容在位: {body}");
    assert!(
        body.contains("second-half"),
        "限流打满后在途流继续传输尾段: {body}"
    );
    assert!(body.contains("[DONE]"), "已放行流完整收束: {body}");
    assert!(
        !body.contains("event: error"),
        "已放行的流不得被限流中断: {body}"
    );
}
