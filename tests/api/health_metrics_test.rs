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
use fluxweir::{
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
        masking: Default::default(),
        web_search: Default::default(),
        vendor_budgets: Vec::new(),
        key_budgets: Default::default(),
        server: fluxweir::config::ServerConfig {
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
        retry: fluxweir::config::RetrySection::default(),
        routing: fluxweir::config::RoutingSection::default(),
        cache: fluxweir::config::CacheSection::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: fluxweir::config::ResilienceSection::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
        rate_limit: fluxweir::config::RateLimitSection { rules: vec![] },
        otlp: Default::default(),
        usage_insights: Default::default(),
        catalog_sync: Default::default(),
        subscription: Default::default(),
        subscription_accounts: Vec::new(),
        metrics: Default::default(),
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
        repository: Arc::new(fluxweir::database::Repository::new(pool.clone())),
    });
    let router = Arc::new(fluxweir::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let response_cache = Arc::new(fluxweir::api::response_cache::ResponseCache::new(
        Arc::new(oxcache::backend::default_memory_backend()),
        config_manager.clone(),
        vendor_manager.metrics().clone(),
    ));
    let state = Arc::new(ApiState {
        affinity_writer: None,
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
        responses: None,
        #[cfg(feature = "web-search")]
        web_search: None,
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
    fluxweir::api::readiness::register_readiness_checks(&state, &pool);

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

/// /admin/metrics 暴露缓存经济学亲和计数与两个丢失去向（T008）：字段存在
/// 且可累加。settle/恢复打点经 GlobalMetrics；通道丢弃与落库失败各有
/// 精确出口（两者原因不同，运维处置不同）。
#[tokio::test]
async fn admin_metrics_exposes_affinity_counters() {
    let (app, _, _) = build_app().await;
    let resp = get(app, "/admin/metrics").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    for field in [
        "affinity_stick_total",
        "affinity_drift_total",
        "affinity_persist_drop_total",
        "affinity_restored_count",
        "affinity_persist_dropped",
        "affinity_flush_failures",
    ] {
        assert!(
            json.get(field).is_some_and(|v| v.is_u64()),
            "/admin/metrics 缺亲和计数字段 {field}（实测 {json}）"
        );
    }
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

/// /admin/metrics 暴露配额观测三计数（quota-aware-routing）：一次 429 后
/// `quota_window_refused_total` 逐窗计数非空、`quota_ledger_flush_fail_total`
/// 字段在位。与亲和计数同出口（GlobalMetrics 快照 → JSON）。
#[tokio::test]
async fn admin_metrics_exposes_quota_counters() {
    // 配置 quota：租户限额 1 token（首请求即 429，触发受拒计数）。
    // 装配前注入（ConfigManager::new 无路径，update_config 拒绝持久化）。
    let mut config = make_config();
    let good_key_digest =
        "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303".to_string();
    config.auth = Some(fluxweir::config::AuthSection {
        enabled: true,
        api_keys: vec![good_key_digest.clone()],
        header: Default::default(),
        keys: vec![fluxweir::config::AuthKeyEntry {
            hash: good_key_digest,
            tier: fluxweir::config::AuthTier::Elastic,
            token_quota: 1,
            cost_quota_millicents: 0,
        }],
        oidc_rules: None,
    });
    config.quota.enabled = true;

    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    let pool = memory_pool().await;
    let database = Arc::new(Database {
        pool: pool.clone(),
        repository: Arc::new(fluxweir::database::Repository::new(pool.clone())),
    });
    let router = Arc::new(fluxweir::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let state = Arc::new(ApiState {
        affinity_writer: None,
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
        quota: Some(Arc::new(fluxweir::middleware::quota::QuotaState::new())),
        difficulty: None,
        responses: None,
        #[cfg(feature = "web-search")]
        web_search: None,
    });
    let app = create_router(state.clone()).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-good-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "限额 1 token 首请求即 429"
    );

    // auth 已启用：admin 面带合法 key 读取（与业务面同 key）
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/metrics")
                .header("authorization", "Bearer test-good-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let refused = json
        .get("quota_window_refused_total")
        .and_then(|v| v.as_array())
        .expect("quota_window_refused_total 字段在位（labeled 数组）");
    assert!(
        refused
            .iter()
            .any(|e| e[0] == serde_json::json!("default") && e[1].as_u64().unwrap_or(0) >= 1),
        "受拒窗（default）必须计数 ≥1: {refused:?}"
    );
    assert!(
        json.get("quota_ledger_flush_fail_total")
            .is_some_and(|v| v.is_u64()),
        "/admin/metrics 缺 quota_ledger_flush_fail_total"
    );
}

/// /admin/metrics 暴露目录同步观测计数（catalog-sync）：reason 分列失败
/// 计数（labeled 数组）与累计新增条目字段在位且可累加。打点经 GlobalMetrics
///（plan_from_source 失败路径 / persist_plan 落盘成功路径写），此处直录
/// 断言出口形状——与配额三计数同款出口测试。
#[tokio::test]
async fn admin_metrics_exposes_catalog_sync_counters() {
    let (app, state, _) = build_app().await;
    let metrics = state.vendor_manager.metrics();
    metrics.record_catalog_sync_fail("timeout");
    metrics.record_catalog_sync_fail("timeout");
    metrics.record_catalog_sync_fail("network");
    metrics.record_catalog_sync_added(3);

    let resp = get(app, "/admin/metrics").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["catalog_sync_added_total"], 3,
        "新增条目计数必须透出（实测 {json}）"
    );
    let failures = json["catalog_sync_fail_total"]
        .as_array()
        .expect("失败计数必须为 labeled 数组");
    let find = |reason: &str| {
        failures
            .iter()
            .find(|pair| pair[0] == reason)
            .and_then(|pair| pair[1].as_u64())
            .unwrap_or(0)
    };
    assert_eq!(find("timeout"), 2, "timeout 计数分列: {failures:?}");
    assert_eq!(find("network"), 1, "network 计数分列: {failures:?}");
}

/// /admin/metrics 暴露订阅账号观测（subscription-vendor）：刷新失败累计与
/// 当前降级账号数（gauge）。打点经 GlobalMetrics（池降级/恢复/摘除路径写），
/// 此处直录断言出口形状——与目录同步计数同款出口测试。
#[tokio::test]
async fn admin_metrics_exposes_subscription_counters() {
    let (app, state, _) = build_app().await;
    let metrics = state.vendor_manager.metrics();
    metrics.record_subscription_refresh_fail();
    metrics.record_subscription_refresh_fail();
    metrics.record_subscription_account_degraded_enter();

    let resp = get(app, "/admin/metrics").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["subscription_refresh_fail_total"], 2,
        "刷新失败计数必须透出（实测 {json}）"
    );
    assert_eq!(
        json["subscription_accounts_degraded"], 1,
        "降级账号 gauge 必须透出（实测 {json}）"
    );
    metrics.record_subscription_account_degraded_leave();
    assert_eq!(
        state
            .vendor_manager
            .metrics()
            .snapshot()
            .subscription_accounts_degraded,
        0,
        "gauge 减到 0 不下穿"
    );
}

/// 掩码观测四计数经 MetricsSnapshot 进 /admin/metrics JSON（feature
/// `masking`；门未接线时字段恒在、值为 0——零行为口径的观测面）。
#[cfg(feature = "masking")]
#[test]
fn admin_metrics_snapshot_exposes_masking_counters() {
    let metrics = fluxweir::utils::metrics::GlobalMetrics::new();

    // 未接线：四字段在场且为 0
    let snapshot = metrics.snapshot();
    let value = serde_json::to_value(&snapshot).expect("快照可序列化");
    assert_eq!(value["masking_hits_total"], serde_json::json!([]));
    assert_eq!(value["masking_blocked_total"], serde_json::json!(0));
    assert_eq!(value["masking_fail_total"], serde_json::json!(0));
    assert_eq!(value["masking_restore_total"], serde_json::json!(0));

    // 打点后四计数按标签/数量出现（kind:action 分列）
    metrics.record_masking_hit("api_key", "mask");
    metrics.record_masking_hit("email", "warn");
    metrics.record_masking_hit("custom", "block");
    metrics.record_masking_blocked();
    metrics.record_masking_fail();
    metrics.record_masking_restore(3);
    let snapshot = metrics.snapshot();
    let value = serde_json::to_value(&snapshot).expect("快照可序列化");
    assert_eq!(
        value["masking_hits_total"],
        serde_json::json!([["api_key:mask", 1], ["custom:block", 1], ["email:warn", 1]])
    );
    assert_eq!(value["masking_blocked_total"], serde_json::json!(1));
    assert_eq!(value["masking_fail_total"], serde_json::json!(1));
    assert_eq!(value["masking_restore_total"], serde_json::json!(3));
}
