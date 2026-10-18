// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(all(feature = "grpc", feature = "web-server", feature = "logging"))]

//! gRPC 装配分支覆盖测试。
//!
//! 新建目的：grpc_smoke_test 只走 [grpc].enabled=true 的成功链路；本文件补
//! 三个此前 0 覆盖的行为契约：
//! 1. 端口被占时 GrpcServer::serve 把 sdforge 起服失败映射为 Config 错误
//!    （main-grpc-server-error 错误映射分支）
//! 2. GrpcServer / ServerHandle 的 Debug 形态暴露 addr 与停机状态位
//! 3. take_grpc_server 一次性语义（二次取用为 None）+ 停机收尾幂等
//!
//! build_app 内部执行 init_global_state（OnceCell 二次 set 即 panic），
//! 全部断言收敛在单一 #[tokio::test] 内。

use fluxgate::config::{
    Config, ConfigManager, DatabaseConfig, GlobalModelConfig, GrpcConfig, ModelConfig,
    ServerConfig, VendorConfig,
};
use fluxgate::error::{FluxgateError, GateError};
use fluxgate::server::build_app;
use std::sync::Arc;
use std::time::Duration;

fn test_config(grpc_port: u16) -> Config {
    Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 18427,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors: vec![VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            supports_batch: false,
            chat_path: None,
            api_style: None,
            id: "openai_main".into(),
            name: "OpenAI Main".into(),
            type_: "openai".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: String::new(),
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
        }],
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
        grpc: Some(GrpcConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: grpc_port,
            timeout_seconds: 30,
            max_connections: 500,
            // 非 None 的 keepalive 值覆盖 [grpc] 段 → sdforge 配置的映射分支
            http2_keepalive_interval_secs: Some(15),
            http2_keepalive_timeout_secs: Some(5),
        }),
        resilience: fluxgate::config::ResilienceSection::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
    }
}

#[tokio::test]
async fn grpc_serve_maps_bind_failure_and_handle_semantics() {
    // 手动挂 inklog 全局 subscriber（与 bootstrap::init_logger 同参）：
    // serve 路径的 tracing 事件才会真实发往 subscriber 并求值消息参数
    let _logger = inklog::LoggerManager::builder()
        .level("info")
        .console(true)
        .build()
        .await
        .expect("inklog logger should build");

    // 先占住动态端口：serve 内部 bind 必失败
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blocker");
    let grpc_port = blocker.local_addr().unwrap().port();

    let config_manager = Arc::new(ConfigManager::new(test_config(grpc_port)));
    let (_app, mut handle) = build_app(config_manager)
        .await
        .expect("build_app should assemble the full chain");

    let handle_debug = format!("{handle:?}");
    assert!(
        handle_debug.contains("shutdown_done: false"),
        "停机前 Debug 应显示 shutdown_done: false: {handle_debug}"
    );

    let grpc = handle
        .take_grpc_server()
        .expect("[grpc].enabled=true 时 take_grpc_server 必须产出服务器");
    assert_eq!(
        grpc.addr().to_string(),
        format!("127.0.0.1:{grpc_port}"),
        "addr() 必须与 [grpc] 段配置一致"
    );
    let grpc_debug = format!("{grpc:?}");
    assert!(
        grpc_debug.contains(&grpc_port.to_string()),
        "Debug 应暴露监听地址: {grpc_debug}"
    );

    // take 一次性：装配 ≠ 启动，取出后二次取用为 None
    assert!(
        handle.take_grpc_server().is_none(),
        "take_grpc_server 必须一次性"
    );

    // 端口被占：sdforge 起服失败 → 映射为 Config 错误（错误映射分支）
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        grpc.serve(std::future::pending::<()>()),
    )
    .await
    .expect("端口被占时 serve 必须快速失败而非挂起");
    match result {
        Err(FluxgateError::Gate(GateError::Config { message })) => {
            assert_ne!(
                message, "main-grpc-server-error",
                "错误键必须注册进 FTL 目录（缺键回退返回键名）"
            );
            assert!(
                message.contains("transport error"),
                "应携带 sdforge 底层绑定失败文本: {message}"
            );
        }
        other => panic!("端口被占的 serve 应报 Config 错误，实际 {other:?}"),
    }

    // 停机收尾句柄可用且幂等；Debug 状态位翻转
    handle.shutdown().await;
    handle.shutdown().await;
    let after = format!("{handle:?}");
    assert!(
        after.contains("shutdown_done: true"),
        "停机后 Debug 应显示 shutdown_done: true: {after}"
    );

    drop(blocker);
}
