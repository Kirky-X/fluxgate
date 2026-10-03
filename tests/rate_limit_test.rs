// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(feature = "web-server")]

//! recId=3（W2）：入站限流落地测试。
//!
//! 缺陷背景：config.toml 的 [rate_limit] 段此前被 serde 静默丢弃（Config
//! 无该字段），rate_limit_middleware 一直跑 limiteron Governor 的库内置
//! 默认兜底规则（TokenBucket 100/10）——配置的 requests_per_second 等
//! 从未生效。
//!
//! 本文件锁定：
//! - [[rate_limit.rules]] 配置解析（Red 形态：无字段编译即败）；
//! - per-endpoint 规则（path 前缀）经中间件端点表生效：超限 429 +
//!   Retry-After、租户分桶（API key SHA256）、匿名归入单一 anonymous 桶、
//!   健康检查白名单放行；
//! - 全局规则（path 缺省）注入 limiteron Governor 生效；
//! - rules 为空 = Governor 默认兜底 + 无端点表（与历史行为逐位一致）。

use axum::{body::Body, http::Request, http::StatusCode, Router};
use dbnexus::{DbConfig, DbPool};
use nexusflow::{
    api::create_router,
    config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, RateLimitRule, RateLimitSection,
        ServerConfig,
    },
    database::Database,
    protocol::{AnthropicConverter, OpenAIConverter},
    vendor::VendorManager,
    ApiState,
};
use std::sync::Arc;
use tower::ServiceExt;

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
    Config {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
        },
        // vendors 为空：handler 走路由失败路径（非 429 响应），不影响限流断言
        vendors: vec![],
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![],
        // auth: None → 认证关闭，全部请求透传至限流中间件（租户维度按
        // 请求携带的 Bearer key 摘要分桶，无 key 归入 anonymous）
        auth: None,
        cors: None,
        retry: nexusflow::config::RetrySection::default(),
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        grpc: None,
        resilience: nexusflow::config::ResilienceSection::default(),
        rate_limit: RateLimitSection { rules },
    }
}

async fn build_app(rules: Vec<RateLimitRule>) -> Router {
    let config_manager = Arc::new(ConfigManager::new(make_config(rules)));
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

/// 健康检查白名单断言位于 src/api/router_builder.rs 内联单元测试
///（health_check_bypasses_endpoint_rate_limit）：axum 0.8 的 layer 只作用于
/// 已有路由（axum-0.8.9 src/docs/routing/layer.md:6），create_router 返回后
/// 再 .route 的路径不经过中间件——集成形态的断言空洞（评审 major 探针
/// 实证），故以 route 先于 layer 的单测锁定，并带中间件生效对照组。
/// 匿名语义锁定：无 Authorization 头的请求全部归入单一 anonymous 桶
/// （沿用 response_cache::cache_api_key_hash 先例），第 3 个请求 429。
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
