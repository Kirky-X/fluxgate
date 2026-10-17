// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! HTTP 服务器：SdForgeConfig 构建、sdforge 站点 + 业务路由合并、
//! bind/serve、serve 结果映射与优雅停机 flush 管道收尾。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;

use crate::config::ConfigManager;
use crate::error::FluxgateError;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::server::bootstrap;
use crate::server::handle::ServerHandle;

/// 装配至 Router 为止：bootstrap 全链装配 + sdforge 站点路由合并；供测试与嵌入式直接持有 Router。
///
/// 返回的 [`ServerHandle`] 承载停机收尾（幂等）：嵌入式进程退出前必须调用
/// [`ServerHandle::shutdown`]，否则 flush worker 泄漏、缓冲数据丢失。
///
/// # Panics
///
/// 经 `init_global_state` 注册 sdforge handler 全局态（OnceCell 一次性写入）：
/// 同进程二次调用即 panic（重复装配是调用方 bug）。
pub async fn build_app(
    config_manager: Arc<ConfigManager>,
) -> Result<(Router, ServerHandle), FluxgateError> {
    let assembly = bootstrap::assemble(config_manager.clone()).await?;
    let app = assemble_router(&config_manager, &assembly).await?;
    // gRPC 装配（grpc feature）：禁用/缺段为 None；登记入句柄，由调用方 take 后启动
    #[cfg(feature = "grpc")]
    let grpc = crate::server::grpc::build(&config_manager, &assembly)?;
    let handle = ServerHandle::from_assembly(assembly);
    #[cfg(feature = "grpc")]
    handle.set_grpc_server(grpc);
    Ok((app, handle))
}

/// 完整运行时：bootstrap 装配 + serve + 优雅停机 flush 收尾。
///
/// # Panics
///
/// 同 [`build_app`]：经 `init_global_state`，同进程二次调用 panic。
pub async fn run(config_path: Option<String>) -> Result<(), FluxgateError> {
    // 进程级日志句柄：活到 run 返回
    let _logger = bootstrap::init_logger().await?;
    let config_manager = bootstrap::load_config(config_path)?;
    let assembly = bootstrap::assemble(config_manager.clone()).await?;
    let app = assemble_router(&config_manager, &assembly).await?;
    // gRPC 装配（同一登记路径；take 于下方 serve 前）
    #[cfg(feature = "grpc")]
    let mut handle = {
        let grpc = crate::server::grpc::build(&config_manager, &assembly)?;
        let handle = ServerHandle::from_assembly(assembly);
        handle.set_grpc_server(grpc);
        handle
    };
    #[cfg(not(feature = "grpc"))]
    let handle = ServerHandle::from_assembly(assembly);

    let server_config = {
        let config = config_manager.read();
        config.server.clone()
    };

    let ipv4: std::net::Ipv4Addr =
        server_config
            .host
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                FluxgateError::from(GateError::Config {
                    message: t_args(
                        "main-invalid-server-host",
                        &[
                            ("host", server_config.host.clone()),
                            ("error", e.to_string()),
                        ],
                    ),
                })
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

    // 停机信号走 sdforge graceful 默认触发（SIGTERM/Ctrl-C），HTTP 与 gRPC 各自注册；
    // inklog manager 移交收尾钩子（logger 仍由 _logger 活到收尾完成）。
    {
        let hook: crate::server::shutdown::InklogShutdownHook =
            std::sync::Arc::new(move || _logger.shutdown().map_err(|e| e.to_string()));
        handle.set_inklog_shutdown_hook(hook);
    }

    // gRPC 服务器启动（[grpc] 段启用时）
    #[cfg(feature = "grpc")]
    let grpc_task = match handle.take_grpc_server() {
        Some(grpc) => Some(tokio::spawn(async move {
            grpc.serve(sdforge::http::default_shutdown_signal()).await
        })),
        None => None,
    };

    // serve 走 sdforge graceful 三阶段（触发 → 排空 → 收尾钩子）；排空超时 30s，超时强制中止。
    let result = sdforge::http::serve_with_graceful_shutdown(
        app,
        listener,
        crate::server::shutdown::shutdown_signal(),
        sdforge::http::GracefulShutdownConfig::with_drain_timeout(std::time::Duration::from_secs(
            30,
        )),
    )
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
            return Err(FluxgateError::from(GateError::Config {
                message: t_args("main-server-error", &[("error", e.to_string())]),
            }));
        }
    }

    // flush 与 build_app 句柄共用同一实现；gRPC 侧先收尾，等它返回或超时后再 flush。
    #[cfg(feature = "grpc")]
    if let Some(task) = grpc_task {
        const GRPC_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        match tokio::time::timeout(GRPC_DRAIN_TIMEOUT, task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(e),
            Ok(Err(join_err)) => {
                return Err(FluxgateError::from(GateError::Config {
                    message: t_args("main-grpc-server-error", &[("error", join_err.to_string())]),
                }));
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

/// SdForgeConfig 构建。
fn sdforge_config(config_manager: &Arc<ConfigManager>) -> sdforge::config::SdForgeConfig {
    let config_ref = config_manager.read();

    // CORS：sdforge 要求 origin 带 http(s):// 前缀且拒绝空列表，故过滤 "*"；
    // 过滤后为空则不挂 CORS 层。
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
        server: sdforge::config::ServerConfig {
            host: config_ref.server.host.clone(),
            port: config_ref.server.port,
            request_timeout_secs: 300,
            max_body_size: 10 * 1024 * 1024,
            cors,
            // Fluxgate 不经 sdforge 终结 TLS（明文监听，TLS 由外部反代承担）
            tls: None,
        },
        // 认证由自建 api_auth_middleware 承担（[auth] SHA256 摘要制）；
        // sdforge ApiKey 仅支持站点级明文 keys，故禁用。
        authentication: sdforge::config::AuthConfig::None,
        timeout: Some(sdforge::config::TimeoutConfig::default()),
        security: sdforge::config::SecurityConfig::default(),
        // security feature 连带启用 sdforge 的 cache feature，字段必填
        cache: sdforge::config::CacheConfig::default(),
    }
}

/// 装配链：sdforge 站点（#[forge] 注册）+ 业务路由合并。
async fn assemble_router(
    config_manager: &Arc<ConfigManager>,
    assembly: &bootstrap::Assembly,
) -> Result<Router, FluxgateError> {
    // 双栈语言对齐：sdforge 框架栈（OpenAPI/health 描述串与框架 HTTP 响应）有
    // 独立检测链（SDFORGE_LANG 起步），不自动继承 FLUXGATE_LANG——在此显式同步
    // 为业务栈当前 locale，README 承诺的单一语言入口对两栈一致（set_locale
    // 优先于其检测缓存，晚于首次 get_locale 也能覆盖）。
    sdforge::i18n::set_locale(&crate::i18n::current_locale().to_string());
    let sdforge_config = sdforge_config(config_manager);
    let mut app = sdforge::http::build_with_config(&sdforge_config)?;
    app = app.merge(crate::api::create_router(assembly.api_state.clone()).await);
    Ok(app)
}
