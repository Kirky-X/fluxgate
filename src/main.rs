// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::net::SocketAddr;
use std::sync::Arc;

use nexusflow::api::{init_global_state, ApiState};
use nexusflow::config::loader::ConfigLoader;
use nexusflow::database::Database;
use nexusflow::error::NexusFlowError;
use nexusflow::i18n::{t, t_args};
use nexusflow::protocol::AnthropicConverter;
use nexusflow::protocol::OpenAIConverter;
use nexusflow::router::Router as RouterStruct;
use nexusflow::vendor::VendorManager;

#[tokio::main]
async fn main() -> Result<(), NexusFlowError> {
    println!("{}", t("main-starting"));
    let _logger = inklog::LoggerManager::builder()
        .level("info")
        .console(true)
        .build()
        .await
        .map_err(|e| NexusFlowError::Config {
            message: t_args("main-logger-init-failed", &[("error", e.to_string())]),
        })?;

    let config_path = std::env::args().nth(1).unwrap_or("config.toml".to_string());
    tracing::info!(
        "{}",
        t_args("main-loading-config", &[("path", config_path.clone())])
    );
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    tracing::info!("{}", t("main-config-loaded"));

    let db_config = {
        let config = config_manager.read();
        config.database.clone()
    };
    tracing::info!(
        "{}",
        t_args(
            "main-initializing-database",
            &[("path", db_config.path.clone())]
        )
    );
    let database = Arc::new(Database::new(&db_config.path, db_config.pool_size).await?);
    tracing::info!("{}", t("main-database-initialized"));

    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await?;
    tracing::info!("{}", t("main-vendor-manager-initialized"));

    // recId=9②：日志管道构造 — AsyncLogger（异步批量落 request_logs）与
    // TokenPreAggregator（usage 小时预聚合）自 Database 的 repository 构造，
    // 经 ApiState 注入 handler（push 即返回，flush 由后台 worker 承担）。
    let logger_handle = Arc::new(nexusflow::database::AsyncLoggerHandle::new(
        nexusflow::database::AsyncLogger::new(
            database.repository.clone(),
            1024,
            std::time::Duration::from_secs(1),
        ),
    ));
    let pre_aggregator = Arc::new(nexusflow::database::TokenPreAggregator::new(
        database.repository.clone(),
        std::time::Duration::from_secs(60),
    ));
    // recId=9⑤：flush worker 挂后台，停机位在 serve 结束后置位
    let pa_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let pa = pre_aggregator.clone();
        let flag = pa_shutdown.clone();
        tokio::spawn(async move {
            pa.start_flush_worker(flag).await;
        });
    }
    tracing::info!("{}", t("main-log-pipeline-initialized"));

    // Build trait-kit AsyncKit DI container: oxcache → dbnexus → inklog
    // chain + limiteron governor. Proves all 4 brother libs are DI-compatible.
    #[cfg(all(
        feature = "infrastructure",
        feature = "database",
        feature = "vendor-management",
        feature = "logging"
    ))]
    {
        tracing::info!("{}", t("main-building-di-container"));
        let di_db_config = dbnexus::foundation::config::DbConfig {
            url: format!("sqlite:{}", db_config.path),
            pool_config: dbnexus::foundation::config::PoolConfig {
                max_connections: db_config.pool_size as u32,
                min_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let kit = nexusflow::infrastructure::di::build_kit_with_config(di_db_config).await?;
        nexusflow::infrastructure::di::verify_kit(&kit)?;
        tracing::info!("{}", t("main-di-container-built"));
    }

    let router = RouterStruct::new(config_manager.clone(), vendor_manager.clone());

    // recId=15：LinUCB 状态（Router 选择档与 handler 奖励回传共享）。
    // 臂矩阵正则初值 = max(α²,1)·I 在构造期由 [routing].ucb_alpha 定死；
    // selection 默认 "weighted" 时状态不被咨询、不更新（默认路径不变）。
    let bandit_state = Arc::new(nexusflow::router::bandit::BanditState::new(
        config_manager.read().routing.ucb_alpha,
    ));

    // recId=11：会话亲和表（oxcache memory 后端，TTL 1h 活跃续期）。
    // 存储完全走 oxcache CacheBackend（set 带 TTL / get / expire 续期）。
    let affinity_table = Arc::new(
        nexusflow::router::affinity::AffinityTable::with_memory_backend(
            nexusflow::router::affinity::DEFAULT_AFFINITY_TTL,
        ),
    );

    // recId=12：精确响应缓存（oxcache memory 后端）。enabled/TTL 由
    // [cache] 段热读控制——默认关闭，运维在 config.toml 显式打开。
    // recId=12：TTL 负载联动的指标来源（L_p=vendor_ewma、Q=traffic_shares）
    let response_cache = Arc::new(nexusflow::api::response_cache::ResponseCache::new(
        Arc::new(oxcache::backend::default_memory_backend()),
        config_manager.clone(),
        vendor_manager.metrics().clone(),
    ));
    {
        let cache_enabled = config_manager.read().cache.enabled;
        tracing::info!(
            "{}",
            if cache_enabled {
                t("main-response-cache-enabled")
            } else {
                t("main-response-cache-disabled")
            }
        );
    }

    let api_state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(
            router
                .with_affinity(affinity_table.clone())
                .with_bandit(bandit_state.clone()),
        ),
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: database.clone(),
        logger: Some(logger_handle.clone()),
        pre_aggregator: Some(pre_aggregator.clone()),
        affinity: Some(affinity_table.clone()),
        response_cache: Some(response_cache),
        bandit: Some(bandit_state),
    });

    init_global_state(api_state.clone());
    tracing::info!("{}", t("main-global-state-initialized"));

    // Build app with sdforge's configuration
    let sdforge_config = {
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
    };

    // 生产装配链：sdforge 站点（#[forge] 注册的 /api/v1/health 等）+ 业务路由合并。
    // 修复前 main 只 serve build_with_config 产物、未 merge create_router，
    // /v1 与 /admin 全部 404。build_with_config 返回原生 axum::Router，
    // 直接 merge（sdforge/src/http/http_impl.rs:274）。
    let mut app = sdforge::http::build_with_config(&sdforge_config)?;
    app = app.merge(nexusflow::api::create_router(api_state.clone()).await);

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

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    match result {
        Ok(_) => tracing::info!("{}", t("main-server-stopped")),
        Err(e) => {
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

    // recId=9：优雅停机 flush — serve 返回即 in-flight 请求已全部完成，
    // 此刻补最终 flush 保证缓冲中的日志与聚合数据不丢
    tracing::info!("{}", t("main-flushing-log-pipeline"));
    if let Err(e) = logger_handle.shutdown().await {
        tracing::warn!(
            "{}",
            t_args("main-async-logger-shutdown", &[("error", e.to_string())])
        );
    }
    pa_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(e) = pre_aggregator.flush().await {
        tracing::warn!(
            "{}",
            t_args("main-pre-aggregator-flush", &[("error", e.to_string())])
        );
    }
    tracing::info!(
        "{}",
        t_args(
            "main-log-pipeline-flushed",
            &[("dropped", logger_handle.dropped_logs().to_string())]
        )
    );

    Ok(())
}

/// 监听终止信号触发优雅停机：先收到 SIGTERM/Ctrl+C 后 axum 会停止接受新连接，
/// 等待 in-flight 请求完成再退出。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", t("main-ctrl-c-handler-failed")));
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sig = signal(SignalKind::terminate())
            .unwrap_or_else(|e| panic!("{}: {e:?}", t("main-sigterm-handler-failed")));
        sig.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("{}", t("main-received-ctrl-c")),
        _ = terminate => tracing::info!("{}", t("main-received-sigterm")),
    }
}
