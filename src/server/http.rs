// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! HTTP 服务器：SdForgeConfig 构建、sdforge 站点 + 业务路由合并、
//! bind/serve、serve 结果映射与优雅停机 flush 管道收尾。
//!
//! 自 main.rs:164-302 平移，serve 与 flush 顺序逐位一致。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;

use crate::config::ConfigManager;
use crate::error::NexusFlowError;
use crate::i18n::{t, t_args};
use crate::server::bootstrap;
use crate::server::handle::ServerHandle;

/// 装配至 Router 为止：bootstrap 全链装配 + sdforge 站点路由合并
/// （原 main.rs:164-223）。供测试与嵌入式场景直接持有 Router。
///
/// 返回的 [`ServerHandle`] 承载停机收尾（flush worker 停机位 +
/// 日志/聚合补刷，幂等）：嵌入式进程退出前必须调用
/// [`ServerHandle::shutdown`]，否则 flush worker 泄漏且缓冲数据丢失。
///
/// # Panics
///
/// 内部经 [`crate::api::init_global_state`] 注册 sdforge handler 全局态
/// （OnceCell 一次性写入）：同进程**二次调用即 panic**
/// （"Global state already initialized"）——重复装配是调用方 bug。
pub async fn build_app(
    config_manager: Arc<ConfigManager>,
) -> Result<(Router, ServerHandle), NexusFlowError> {
    let assembly = bootstrap::assemble(config_manager.clone()).await?;
    let handle = ServerHandle::from_assembly(&assembly);
    let app = assemble_router(&config_manager, &assembly).await?;
    // gRPC 装配（feature = "grpc"）：禁用/缺段为 None，随句柄登记，
    // 由调用方 take_grpc_server 取出后启动
    #[cfg(feature = "grpc")]
    {
        let grpc = crate::server::grpc::build(&config_manager, &assembly)?;
        handle.set_grpc_server(grpc);
    }
    Ok((app, handle))
}

/// 完整运行时：bootstrap 装配 + serve + 优雅停机 flush 收尾
/// （原 main() 的 serve 主体与收尾，main.rs:225-302）。
///
/// # Panics
///
/// 同 [`build_app`]：同进程二次调用经 `init_global_state` panic。
pub async fn run(config_path: Option<String>) -> Result<(), NexusFlowError> {
    // 进程级日志句柄：活到 run 返回（原 main.rs:22 的 main 作用域语义）
    let _logger = bootstrap::init_logger().await?;
    let config_manager = bootstrap::load_config(config_path)?;
    let assembly = bootstrap::assemble(config_manager.clone()).await?;
    #[cfg(feature = "grpc")]
    let mut handle = ServerHandle::from_assembly(&assembly);
    #[cfg(not(feature = "grpc"))]
    let handle = ServerHandle::from_assembly(&assembly);
    let app = assemble_router(&config_manager, &assembly).await?;
    // gRPC 装配（与 build_app 同一登记路径；take 于下方 serve 前）
    #[cfg(feature = "grpc")]
    {
        let grpc = crate::server::grpc::build(&config_manager, &assembly)?;
        handle.set_grpc_server(grpc);
    }

    let server_config = {
        let config = config_manager.read();
        config.server.clone()
    };

    let ipv4: std::net::Ipv4Addr =
        server_config
            .host
            .parse()
            .map_err(|e: std::net::AddrParseError| NexusFlowError::Config {
                message: t_args(
                    "main-invalid-server-host",
                    &[
                        ("host", server_config.host.clone()),
                        ("error", e.to_string()),
                    ],
                ),
            })?;
    let addr = SocketAddr::from((ipv4, server_config.port));

    tracing::info!(
        "{}",
        t_args("main-starting-server", &[("addr", addr.to_string())])
    );
    tracing::info!(
        "{}",
        t_args("main-server-listening", &[("addr", addr.to_string())])
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("{}", t("main-press-ctrl-c"));

    // 停机广播：Ctrl+C/SIGTERM 一次触发，HTTP 与 gRPC 两路 graceful
    // shutdown 共享同一信号（各持一个 ShutdownRx）
    let mut shutdown_rx = crate::server::shutdown::spawn_shutdown_broadcaster();

    // gRPC 服务器启动（[grpc] 段启用时；serve 内部打 starting/stopped 日志）
    #[cfg(feature = "grpc")]
    let grpc_task = match handle.take_grpc_server() {
        Some(grpc) => {
            let mut rx = shutdown_rx.clone();
            Some(tokio::spawn(async move {
                grpc.serve(async move { rx.fired().await }).await
            }))
        }
        None => None,
    };

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_rx.fired().await;
        })
        .await;

    match result {
        Ok(_) => tracing::info!("{}", t("main-server-stopped")),
        Err(e) => {
            // HTTP serve 失败：signal 未触发，gRPC 无 graceful 可言，立即终止
            #[cfg(feature = "grpc")]
            if let Some(task) = &grpc_task {
                task.abort();
            }
            tracing::error!(
                "{}",
                t_args(
                    "main-server-stopped-with-error",
                    &[("error", e.to_string())]
                )
            );
            return Err(NexusFlowError::Config {
                message: t_args("main-server-error", &[("error", e.to_string())]),
            });
        }
    }

    // recId=9：优雅停机 flush（序列与 build_app 句柄共用同一实现）。
    // gRPC 侧先收尾：HTTP serve 返回即 signal 已触发，gRPC 正在排空
    // in-flight；等它返回（或超时）后再 flush，保证请求路径日志不丢。
    #[cfg(feature = "grpc")]
    if let Some(task) = grpc_task {
        const GRPC_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        match tokio::time::timeout(GRPC_DRAIN_TIMEOUT, task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(e),
            Ok(Err(join_err)) => {
                return Err(NexusFlowError::Config {
                    message: t_args("main-grpc-server-error", &[("error", join_err.to_string())]),
                });
            }
            Err(_) => tracing::warn!(
                "{}",
                t_args(
                    "main-grpc-server-error",
                    &[(
                        "error",
                        t_args("main-grpc-drain-timeout", &[("secs", "30".to_string())])
                            .to_string()
                    )]
                ),
            ),
        }
    }

    handle.shutdown().await;

    Ok(())
}

/// SdForgeConfig 构建（原 main.rs:165-216）。
fn sdforge_config(config_manager: &Arc<ConfigManager>) -> sdforge::config::SdForgeConfig {
    let config_ref = config_manager.read();

    // sdforge 0.5 rc.5: AppConfig 更名 SdForgeConfig，ServerConfig 新增
    // max_body_size（旧版硬编码 10 MiB，沿用该值）。
    //
    // CORS：sdforge rc.5 校验强制 origin 带 http(s):// 前缀且拒绝空列表
    // （sdforge/src/config/cors.rs:20-49），通配 "*" 不再合法。取 [cors]
    // 段的 origin 并过滤 "*"；过滤后为空则不挂 CORS 层（网关场景由调用方
    // 侧处理跨域）。
    let cors = config_ref.cors.as_ref().and_then(|c| {
        if !c.enabled {
            return None;
        }
        let origins: Vec<String> = c
            .allowed_origins
            .iter()
            .filter(|o| o.as_str() != "*")
            .cloned()
            .collect();
        if origins.is_empty() {
            return None;
        }
        Some(sdforge::config::CorsConfig {
            allowed_origins: origins,
            allowed_methods: c.allowed_methods.clone(),
            allowed_headers: c.allowed_headers.clone(),
        })
    });

    sdforge::config::SdForgeConfig {
        // Override server config with required fields
        server: sdforge::config::ServerConfig {
            host: config_ref.server.host.clone(),
            port: config_ref.server.port,
            request_timeout_secs: 300,
            max_body_size: 10 * 1024 * 1024,
            cors,
            // NexusFlow 不经 sdforge 终结 TLS（自建 axum 明文监听，
            // TLS 由外部反代承担）
            tls: None,
        },
        // 认证由自建 api_auth_middleware 承担（[auth] 段 SHA256 摘要 +
        // 常量时间比较，见 middleware/auth.rs）。sdforge 层显式禁用：
        // 其 ApiKey 模式仅支持站点级明文 keys 且空表在 build 时直接报错
        // （sdforge/src/http/http_impl.rs:389），无摘要存储能力。
        authentication: sdforge::config::AuthConfig::None,
        timeout: Some(sdforge::config::TimeoutConfig::default()),
        security: sdforge::config::SecurityConfig::default(),
        // security feature 连带启用 sdforge 的 cache feature，字段必填
        cache: sdforge::config::CacheConfig::default(),
    }
}

/// 生产装配链：sdforge 站点（#[forge] 注册的 /api/v1/health 等）+ 业务路由合并
/// （原 main.rs:218-223）。
/// 修复前 main 只 serve build_with_config 产物、未 merge create_router，
/// /v1 与 /admin 全部 404。build_with_config 返回原生 axum::Router，
/// 直接 merge（sdforge/src/http/http_impl.rs:274）。
async fn assemble_router(
    config_manager: &Arc<ConfigManager>,
    assembly: &bootstrap::Assembly,
) -> Result<Router, NexusFlowError> {
    let sdforge_config = sdforge_config(config_manager);
    let mut app = sdforge::http::build_with_config(&sdforge_config)?;
    app = app.merge(crate::api::create_router(assembly.api_state.clone()).await);
    Ok(app)
}
