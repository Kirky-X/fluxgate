// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! 就绪探针 /readyz 与指标出口 /metrics。
//!
//! - /readyz 聚合 sdforge 注册检查：全绿 200 `{"status":"ready"}`，任一
//!   失败 503；探针免认证（现有 /api/v1/health 由 assembly_smoke_test 覆盖）。
//! - /metrics 为 Prometheus 文本（含 HELP/TYPE），RED 序列来自中间件记录。

use axum::{body::Body, http::Request, http::StatusCode, Router};
use dbnexus::{DbConfig, DbPool};
use fluxgate::{
    api::create_router,
    config::{Config, ConfigManager, DatabaseConfig},
    database::Database,
    protocol::{AnthropicConverter, OpenAIConverter},
    vendor::VendorManager,
    ApiState,
};
use std::sync::Arc;
use tower::ServiceExt;

fn make_config() -> Config {
    Config {
        server: fluxgate::config::ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors: vec![],
        global_model: None,
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
        resilience: fluxgate::config::ResilienceSection::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        rate_limit: fluxgate::config::RateLimitSection { rules: vec![] },
    }
}

async fn memory_pool() -> DbPool {
    DbPool::with_config(DbConfig {
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
    .unwrap()
}

async fn build_app() -> (Router, Arc<ApiState>, DbPool) {
    let config_manager = Arc::new(ConfigManager::new(make_config()));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    let pool = memory_pool().await;
    let database = Arc::new(Database {
        pool: pool.clone(),
        repository: Arc::new(fluxgate::database::Repository::new(pool.clone())),
    });
    let router = Arc::new(fluxgate::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let response_cache = Arc::new(fluxgate::api::response_cache::ResponseCache::new(
        Arc::new(oxcache::backend::default_memory_backend()),
        config_manager.clone(),
        vendor_manager.metrics().clone(),
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
        response_cache: Some(response_cache),
        bandit: None,
        quota: None,
        difficulty: None,
    });
    let app = create_router(state.clone()).await;
    // /readyz 由 sdforge 站点在生产合并侧自挂；直连 create_router 时经
    // mount_probes 补挂（同一 handler + 同一全局注册表，行为一致）。
    // /metrics 同理（sdforge 站点自挂），此处手动挂载同一 handler。
    let app = sdforge::health::mount_probes(app).route(
        "/metrics",
        axum::routing::get(sdforge::metrics::metrics_handler),
    );
    (app, state, pool)
}

async fn get(app: Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

fn payload_check_names(body: &serde_json::Value) -> Vec<(String, bool)> {
    body["checks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap_or("").to_string(),
                c["healthy"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

/// /readyz 聚合真实检查：生产三检查全绿 → 200；注入失败检查 → 503。
/// 单测内顺序执行（sdforge 注册表进程全局，避免并发测试交叉污染）。
#[tokio::test]
async fn readyz_reflects_component_health() {
    let (app, state, pool) = build_app().await;
    fluxgate::api::readiness::register_readiness_checks(&state, &pool);

    let resp = get(app.clone(), "/readyz").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"], "ready");
    let checks = payload_check_names(&payload);
    for name in ["db-pool", "cache-backend", "vendor-manager"] {
        assert!(
            checks.iter().any(|(n, h)| n == name && *h),
            "check {name} must be healthy, got {checks:?}"
        );
    }

    // 注入失败：/readyz 翻转为 503，且载荷携带该检查的 unhealthy 条目
    sdforge::health::register_async_readiness_check_fn("t008-simulated-outage", || async {
        sdforge::health::CheckOutcome {
            name: "t008-simulated-outage".to_string(),
            healthy: false,
            details: Some(serde_json::json!({"error": "simulated"})),
        }
    });
    let resp = get(app, "/readyz").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"], "unavailable");
    let checks = payload_check_names(&payload);
    assert!(
        checks
            .iter()
            .any(|(n, h)| n == "t008-simulated-outage" && !h),
        "outage check must be present and unhealthy, got {checks:?}"
    );
}

/// /metrics 为 Prometheus 文本（含 HELP/TYPE）；一次业务请求后出现
/// RED 序列。探针与指标端点免认证（无 key 可达）。
#[tokio::test]
async fn metrics_serves_prometheus_text_after_traffic() {
    let (app, _, _) = build_app().await;

    // 无 key 请求业务端点（vendors 为空走路由失败路径，但 RED 记录不受影响）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .body(Body::from(
                    r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    let resp = get(app.clone(), "/metrics").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .expect("metrics must set content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type {content_type}"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("# HELP sdforge_http_requests_total"),
        "missing HELP line"
    );
    assert!(
        text.contains("# TYPE sdforge_http_requests_total counter"),
        "missing TYPE line"
    );
    assert!(
        text.contains("route=\"/v1/chat/completions\""),
        "RED series for the request must be present"
    );
}
