// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "console")]

//! 内置控制台嵌入面（console feature）装配冒烟：build_app 装配后
//! 1. GET /console → 200 且含 SPA 根标记 `<div id="root"`
//! 2. GET /console/missing-route → 200 SPA fallback（index.html）
//! 3. GET /api/v1/health → 200（既有路由不受 console 合并影响）
//!
//! build_app 内部执行 init_global_state（OnceCell 二次 set 即 panic），
//! 全部断言收敛在单一 #[tokio::test] 内（与 server_smoke_test 同因）。

#[path = "../common/mod.rs"]
mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use fluxgate::config::{AuthSection, Config, ConfigManager, ServerConfig};
use fluxgate::server::build_app;
use std::sync::Arc;
use tower::ServiceExt;

const CONSOLE_KEY_HASH: &str = "e9547242bd6a03bfab1da509ab0da8b70381955dbb0f3a36d0fe292e58a9b9e9";

fn test_config() -> Config {
    let mut vendor = common::base_vendor("openai_main", String::new());
    vendor.name = "OpenAI Main".into();
    vendor.api_key_env = "OPENAI_API_KEY".into();
    Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 18422,
        },
        auth: Some(AuthSection {
            enabled: true,
            api_keys: vec![CONSOLE_KEY_HASH.to_string()],
            keys: vec![],
            header: Default::default(),
            oidc_rules: None,
        }),
        ..common::test_config(vec![vendor])
    }
}

#[tokio::test]
async fn console_routes_serve_embedded_assets() {
    let config_manager = Arc::new(ConfigManager::new(test_config()));
    let (app, handle) = build_app(config_manager)
        .await
        .expect("build_app should assemble the full chain");

    // 1. GET /console → 200 + SPA 根标记
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should not fail");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /console must be 200"
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body should be collectable");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains(r#"<div id="root""#),
        "embedded index.html must contain the SPA root marker"
    );
    // H-1 回归锚：资产引用必须落在 /console 前缀内。相对路径（./assets/）
    // 在 /console 无尾斜杠入口下会解析到 /assets/*（404 → 浏览器白屏），
    // 仅断言壳可达的测试看不见这一层。
    assert!(
        text.contains("/console/assets/"),
        "index.html must reference assets under the /console prefix, got: {text}"
    );
    assert!(
        !text.contains("./assets/"),
        "relative asset paths escape the mount prefix on /console (no trailing slash)"
    );

    // 2. GET /console/missing-route → 200 SPA fallback（同 index.html 壳）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console/missing-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should not fail");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "SPA fallback must serve index.html with 200"
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body should be collectable");
    assert!(
        String::from_utf8_lossy(&body).contains(r#"<div id="root""#),
        "SPA fallback body must be the index shell"
    );

    // 3. /api/v1/health 仍 200：console 合并不影响既有路由
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should not fail");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/api/v1/health must remain 200 after console merge"
    );

    handle.shutdown().await;
}
