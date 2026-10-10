// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(all(feature = "web-server", feature = "logging"))]

//! server 模块冒烟 — 库化装配链 `nexusflow::server::build_app` 的最小行为
//! 契约（oneshot，无网络依赖）。完整装配矩阵由 assembly_smoke_test.rs
//! 承载，此处不重复其断言；本文件锁定 server 门控模块自身可达且行为正确：
//! 1. test_config 装配 → build_app 不 panic
//! 2. /api/v1/health → 200（sdforge #[forge] 站点路由在合并后可达）
//! 3. /v1/chat/completions 无 key → 401（认证层拦截）
//!
//! build_app 内部执行 init_global_state（OnceCell 二次 set 即 panic，
//! 见 api/global_state.rs），故全部断言收敛在单一 #[tokio::test] 内。

#[path = "../common/mod.rs"]
mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use nexusflow::config::{AuthSection, Config, ConfigManager, ServerConfig};
use nexusflow::server::build_app;
use std::sync::Arc;
use tower::ServiceExt;

/// sha256("smoke-test-key") — 与 assembly_smoke_test 同源摘要
const SMOKE_KEY_HASH: &str = "e9547242bd6a03bfab1da509ab0da8b70381955dbb0f3a36d0fe292e58a9b9e9";

fn test_config() -> Config {
    let mut vendor = common::base_vendor("openai_main", String::new());
    // 与共享基座的差异字段（显式保留原值）：冒烟装配不触网，锁真实 env 名
    // "OPENAI_API_KEY" 与历史 vendor 名，不引入共享 mock env 依赖
    vendor.name = "OpenAI Main".into();
    vendor.api_key_env = "OPENAI_API_KEY".into();
    Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            // build_app 只装配不 bind，但 SdForgeConfig 校验拒绝 port=0
            // （sdforge/src/config/server.rs）；取非常规高位端口避免
            // 与开发环境 8080 冲突
            port: 18421,
        },
        auth: Some(AuthSection {
            enabled: true,
            api_keys: vec![SMOKE_KEY_HASH.to_string()],
            keys: vec![],
            header: Default::default(),
        }),
        ..common::test_config(vec![vendor])
    }
}

#[tokio::test]
async fn build_app_serves_health_and_guards_chat() {
    let config_manager = Arc::new(ConfigManager::new(test_config()));
    let (app, handle) = build_app(config_manager)
        .await
        .expect("build_app should assemble the full chain");

    let response = app
        .clone()
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
        "/api/v1/health must be served by build_app"
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-4"}"#))
                .unwrap(),
        )
        .await
        .expect("oneshot should not fail");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "no key → 401");

    // 停机收尾句柄：flush 管道置位 + 补刷；幂等（二次调用 no-op 不 panic）
    handle.shutdown().await;
    handle.shutdown().await;
}
