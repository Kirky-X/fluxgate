// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 装配链：logger → config → db → vendor → 日志管道 → DI kit →
//! router/bandit/affinity/response_cache → ApiState → init_global_state。
//!
//! 自 main.rs:22-162 平移，装配顺序与原二进制逐位一致。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::config::loader::ConfigLoader;
use crate::config::ConfigManager;
use crate::database::AsyncLoggerHandle;
use crate::database::TokenPreAggregator;
use crate::error::NexusFlowError;
use crate::i18n::{t, t_args};
use crate::ApiState;

/// 装配产物：Router 构建与 serve 后停机 flush 收尾所需的运行时句柄。
pub(crate) struct Assembly {
    pub(crate) api_state: Arc<ApiState>,
    pub(crate) logger_handle: Arc<AsyncLoggerHandle>,
    pub(crate) pre_aggregator: Arc<TokenPreAggregator>,
    pub(crate) pa_shutdown: Arc<AtomicBool>,
}

/// 初始化日志系统（原 main.rs:22-29）。
pub(crate) async fn init_logger() -> Result<(), NexusFlowError> {
    let _logger = inklog::LoggerManager::builder()
        .level("info")
        .console(true)
        .build()
        .await
        .map_err(|e| NexusFlowError::Config {
            message: t_args("main-logger-init-failed", &[("error", e.to_string())]),
        })?;
    Ok(())
}

/// 加载配置（原 main.rs:31-37）。
pub(crate) fn load_config(
    config_path: Option<String>,
) -> Result<Arc<ConfigManager>, NexusFlowError> {
    let config_path = config_path.unwrap_or("config.toml".to_string());
    tracing::info!(
        "{}",
        t_args("main-loading-config", &[("path", config_path.clone())])
    );
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    tracing::info!("{}", t("main-config-loaded"));
    Ok(config_manager)
}

/// 装配 db → vendor → 日志管道 → DI kit → 路由栈 → ApiState
/// （原 main.rs:39-162，顺序逐位一致，含 init_global_state）。
pub(crate) async fn assemble(
    config_manager: Arc<ConfigManager>,
) -> Result<Assembly, NexusFlowError> {
    use crate::database::Database;
    use crate::protocol::AnthropicConverter;
    use crate::protocol::OpenAIConverter;
    use crate::router::Router as RouterStruct;
    use crate::vendor::VendorManager;

    println!("{}", t("main-starting"));

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
    let logger_handle = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
        database.repository.clone(),
        1024,
        std::time::Duration::from_secs(1),
    )));
    let pre_aggregator = Arc::new(TokenPreAggregator::new(
        database.repository.clone(),
        std::time::Duration::from_secs(60),
    ));
    // recId=9⑤：flush worker 挂后台，停机位在 serve 结束后置位
    let pa_shutdown = Arc::new(AtomicBool::new(false));
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
                max_connections: db_config.pool_size,
                min_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let kit = crate::infrastructure::di::build_kit_with_config(di_db_config).await?;
        crate::infrastructure::di::verify_kit(&kit)?;
        tracing::info!("{}", t("main-di-container-built"));
    }

    let router = RouterStruct::new(config_manager.clone(), vendor_manager.clone());

    // recId=15：LinUCB 状态（Router 选择档与 handler 奖励回传共享）。
    // 臂矩阵正则初值 = max(α²,1)·I 在构造期由 [routing].ucb_alpha 定死；
    // selection 默认 "weighted" 时状态不被咨询、不更新（默认路径不变）。
    let bandit_state = Arc::new(crate::router::bandit::BanditState::new(
        config_manager.read().routing.ucb_alpha,
    ));

    // recId=11：会话亲和表（oxcache memory 后端，TTL 1h 活跃续期）。
    // 存储完全走 oxcache CacheBackend（set 带 TTL / get / expire 续期）。
    let affinity_table = Arc::new(crate::router::affinity::AffinityTable::with_memory_backend(
        crate::router::affinity::DEFAULT_AFFINITY_TTL,
    ));

    // recId=12：精确响应缓存（oxcache memory 后端）。enabled/TTL 由
    // [cache] 段热读控制——默认关闭，运维在 config.toml 显式打开。
    // recId=12：TTL 负载联动的指标来源（L_p=vendor_ewma、Q=traffic_shares）
    let response_cache = Arc::new(crate::api::response_cache::ResponseCache::new(
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

    crate::api::init_global_state(api_state.clone());
    tracing::info!("{}", t("main-global-state-initialized"));

    Ok(Assembly {
        api_state,
        logger_handle,
        pre_aggregator,
        pa_shutdown,
    })
}
