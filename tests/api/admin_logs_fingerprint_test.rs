// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! /admin/logs 透出面回归（magpie 吸收审计 D4）：`upstream_key_fingerprint`
//! 必须出现在响应 JSON——透出当前由 `RequestLog` derive Serialize 结构性
//! 保证，本断言防未来响应字段白名单化时静默消失（API_REFERENCE 已承诺
//! 该字段与 NULL 语义）。

use axum::{body::Body, extract::State, http::Request, Json};
use fluxweir::{
    api::{admin, create_router},
    config::ConfigManager,
    database::Database,
    protocol::{AnthropicConverter, OpenAIConverter},
    router::Router,
    storage::RequestLog,
    vendor::VendorManager,
    ApiState,
};

#[path = "../common/mod.rs"]
mod common;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn log(id: &str, fp: Option<&str>) -> RequestLog {
    RequestLog {
        id: None,
        request_id: id.to_string(),
        timestamp: 1_700_000_000,
        endpoint: "/v1/chat/completions".to_string(),
        input_format: "openai".to_string(),
        abstract_model: Some("gpt-4".to_string()),
        messages: "[]".to_string(),
        actual_vendor: "v".to_string(),
        actual_model: "gpt-4".to_string(),
        routing_strategy: "direct".to_string(),
        status_code: Some(200),
        response: None,
        error_message: None,
        prompt_tokens: Some(1),
        completion_tokens: Some(1),
        total_tokens: Some(2),
        duration_ms: Some(3),
        cost_usd: None,
        session_key: None,
        context_window_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        upstream_key_fingerprint: fp.map(str::to_string),
    }
}

/// 装配面与 model_catalog_admin_test::create_state 同口径（真实路由 +
/// 内存库）。
async fn create_state() -> Arc<ApiState> {
    let config_manager = Arc::new(ConfigManager::new(common::test_config(vec![])));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
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
    .expect("pool");
    // request_logs 表由迁移链落地（V1..=V9）——裸池无表，先 initialize
    fluxweir::database::Repository::new(pool.clone())
        .initialize()
        .await
        .expect("init schema");
    Arc::new(ApiState {
        affinity_writer: None,
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(Router::new(config_manager, vendor_manager)),
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota: None,
        difficulty: None,
        responses: None,
        #[cfg(feature = "web-search")]
        web_search: None,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxweir::database::Repository::new(pool)),
        }),
    })
}

#[tokio::test]
async fn admin_logs_response_exposes_upstream_key_fingerprint() {
    let state = create_state().await;
    state
        .database
        .log(log("fp-some", Some("aabb001122334455")))
        .await
        .expect("seed some");
    state
        .database
        .log(log("fp-null", None))
        .await
        .expect("seed null");

    let app = create_router(state).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/admin/logs?limit=10")
                .header("authorization", "Bearer test-key-for-mockito")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "/admin/logs 必须可达（auth 面用 common 测试键）"
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");

    let rows = value.as_array().expect("array response");
    let some = rows
        .iter()
        .find(|r| r["request_id"] == "fp-some")
        .expect("seeded row must be listed");
    assert_eq!(
        some["upstream_key_fingerprint"], "aabb001122334455",
        "指纹必须透出到 /admin/logs JSON（API_REFERENCE 承诺面）"
    );
    let null = rows
        .iter()
        .find(|r| r["request_id"] == "fp-null")
        .expect("null row must be listed");
    assert!(
        null["upstream_key_fingerprint"].is_null(),
        "NULL 语义透出（历史行/未走键池）"
    );
}

// 直调 handler 形态的等价断言（与 HTTP 层互为冗余，防路由/序列化单点）
#[tokio::test]
async fn logs_handler_serializes_fingerprint_field() {
    let state = create_state().await;
    state
        .database
        .log(log("fp-direct", Some("ccdd001122334455")))
        .await
        .expect("seed");

    let Json(value) = admin::logs(
        State(state),
        axum::extract::Query(fluxweir::api::admin::LogQuery {
            limit: Some(10),
            start_time: None,
            end_time: None,
            vendor: None,
            model: None,
        }),
    )
    .await
    .expect("logs");
    let rows = value.as_array().expect("array");
    let row = rows
        .iter()
        .find(|r| r["request_id"] == "fp-direct")
        .expect("row");
    assert_eq!(row["upstream_key_fingerprint"], "ccdd001122334455");
}
