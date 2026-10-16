// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(feature = "web-server")]

//! OpenAPI 文档面——默认关闭（404），开关打开后可达，
//! JSON 含主端点路径。

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

fn make_config(docs_enabled: bool) -> Config {
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
        docs: fluxgate::config::DocsSection {
            enabled: docs_enabled,
        },
        rate_limit: fluxgate::config::RateLimitSection { rules: vec![] },
        lexcompress: Default::default(),
    }
}

async fn build_app(docs_enabled: bool) -> Router {
    let config_manager = Arc::new(ConfigManager::new(make_config(docs_enabled)));
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
        repository: Arc::new(fluxgate::database::Repository::new(pool)),
    });
    let router = Arc::new(fluxgate::router::Router::new(
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

/// 默认关闭：文档路由不存在（404）。
#[tokio::test]
async fn docs_disabled_by_default() {
    let app = build_app(false).await;
    assert_eq!(
        get(app.clone(), "/api-docs/openapi.json").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(app, "/swagger-ui/").await.status(),
        StatusCode::NOT_FOUND
    );
}

/// 开关打开：OpenAPI JSON 含主端点路径，Swagger UI 可达。
#[tokio::test]
async fn docs_enabled_serves_openapi_and_ui() {
    let app = build_app(true).await;

    let resp = get(app.clone(), "/api-docs/openapi.json").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let spec: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let paths = spec["paths"].as_object().expect("spec must have paths");
    for path in [
        "/v1/chat/completions",
        "/v1/messages",
        "/v1/embeddings",
        "/api/v1/health",
        "/admin/logs",
        "/admin/vendors",
        "/admin/config",
        "/admin/models",
        "/readyz",
        "/metrics",
    ] {
        assert!(paths.contains_key(path), "OpenAPI JSON must contain {path}");
    }

    let resp = get(app, "/swagger-ui/").await;
    assert_eq!(resp.status(), StatusCode::OK);
}
