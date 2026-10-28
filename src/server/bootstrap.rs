// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 装配链：logger → config → db → vendor → 日志管道 → DI kit →
//! router/bandit/affinity/response_cache → ApiState → init_global_state。

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::config::loader::ConfigLoader;
use crate::config::ConfigManager;
use crate::database::AsyncLoggerHandle;
use crate::database::TokenPreAggregator;
use crate::error::FluxgateError;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::storage::{RequestLogSink, ResponseStoreSink, UsageSink};
use crate::ApiState;

/// 装配产物：Router 构建与 serve 后停机 flush 收尾所需的运行时句柄。
pub(crate) struct Assembly {
    pub(crate) api_state: Arc<ApiState>,
    pub(crate) logger_handle: Arc<AsyncLoggerHandle>,
    pub(crate) pre_aggregator: Arc<TokenPreAggregator>,
    pub(crate) pa_shutdown: Arc<AtomicBool>,
    /// 热重载守护（`None` = 纯内存配置或监听失败；持有即监听生效，drop 即停止）。
    pub(crate) hot_reloader: Option<confers::watcher::HotReloader<crate::config::Config>>,
    /// 健康探针后台任务句柄（常驻；mode 每周期热读，off = no-op；随停机 abort）。
    pub(crate) probe_handle: Option<tokio::task::JoinHandle<()>>,
    /// 配额账本停机最终 flush 句柄（quota_state 快照生产方 + 落盘器；
    /// None = 账本未装配，收尾该步 no-op）。
    pub(crate) quota_ledger_teardown: Option<(
        Arc<crate::middleware::quota::QuotaState>,
        Arc<crate::middleware::quota_persist::QuotaLedgerPersister>,
    )>,
    /// OTLP 桥（feature = "otlp" 且 [otlp].enabled 时装配；None = 禁用）。
    #[cfg(feature = "otlp")]
    pub(crate) otlp_bridge: Option<Arc<crate::utils::otlp::OtlpBridge>>,
    /// OTLP 桥轮询任务句柄（随停机 abort）。
    #[cfg(feature = "otlp")]
    pub(crate) otlp_task: Option<tokio::task::JoinHandle<()>>,
    /// 目录同步周期任务句柄（catalog-sync feature；随停机 abort）。
    #[cfg(feature = "catalog-sync")]
    pub(crate) catalog_sync_task: Option<tokio::task::JoinHandle<()>>,
}

/// 初始化日志系统。
///
/// 返回的句柄须由调用方持有到进程结束——提前丢弃会使 worker 断连、事件静默丢失。
/// 出站脱敏：关内置 console 通道，经 add_sink 挂自建 GateSink（fail-closed）。
/// log 门面（reqwest/sea-orm 等）经 LogTracer 转 tracing 事件统一走 gate sink。
pub(crate) async fn init_logger() -> Result<inklog::LoggerManager, FluxgateError> {
    install_log_facade_bridge();
    let console_sink = crate::database::async_logger::GateSink::new(
        Arc::new(inklog::sink::ConsoleSink::new(
            inklog::ConsoleSinkConfig::default(),
            inklog::LogTemplate::default(),
        )),
        Arc::new(crate::database::async_logger::outbound_masker()),
    );
    inklog::LoggerManager::builder()
        .level("info")
        .console(false)
        .add_sink(Arc::new(console_sink))
        .build()
        .await
        .map_err(|e| {
            FluxgateError::from(GateError::Config {
                message: t_args("main-logger-init-failed", &[("error", e.to_string())]),
            })
        })
}

/// 桥安装标记：init_logger 每次装配触及，装配测试据此断言桥接线存在。
static LOG_BRIDGE_TOUCHED: AtomicBool = AtomicBool::new(false);

fn install_log_facade_bridge() {
    LOG_BRIDGE_TOUCHED.store(true, Ordering::Relaxed);
    if let Err(e) = tracing_log::LogTracer::init() {
        tracing::debug!(
            "{}",
            t_args(
                "main-log-bridge-already-installed",
                &[("error", e.to_string())]
            )
        );
    }
}

/// 桥接线是否被 init_logger 触及（仅装配测试读取）。
#[cfg(test)]
pub(crate) fn log_bridge_touched() -> bool {
    LOG_BRIDGE_TOUCHED.load(Ordering::Relaxed)
}

pub(crate) fn load_config(
    config_path: Option<String>,
) -> Result<Arc<ConfigManager>, FluxgateError> {
    let config_path = config_path.unwrap_or("config.toml".to_string());
    tracing::info!(
        "{}",
        t_args("main-loading-config", &[("path", config_path.clone())])
    );
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    tracing::info!("{}", t("main-config-loaded"));
    Ok(config_manager)
}

/// 装配 db → vendor → 日志管道 → DI kit → 路由栈 → ApiState。
pub(crate) async fn assemble(
    config_manager: Arc<ConfigManager>,
) -> Result<Assembly, FluxgateError> {
    use crate::database::redact_db_url;
    use crate::database::resolve_database_url;
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
    // URL 单一出口：主库与下方 DI kit 共用同一解析结果（url 优先，path 按编译期后端回退）。
    let db_url = resolve_database_url(&db_config)?;
    // url 内嵌口令：日志只走脱敏形态，原文只进连接参数。
    tracing::info!(
        "{}",
        t_args(
            "main-initializing-database",
            &[("path", redact_db_url(&db_url))],
        )
    );
    let database = Arc::new(Database::new(&db_url, db_config.pool_size).await?);
    tracing::info!("{}", t("main-database-initialized"));

    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await?;
    tracing::info!("{}", t("main-vendor-manager-initialized"));

    // 出站掩码门（feature `masking`）：装配期一次性挂到 VendorManager；
    // 未挂门 = admin masking 面恒空、出站路径零掩码。enabled 由 `[masking]`
    // 热读，门本体始终在位。
    #[cfg(feature = "masking")]
    {
        vendor_manager.set_mask_gate(Arc::new(crate::masking::MaskGate::new(
            config_manager.clone(),
            vendor_manager.metrics().clone(),
        )));
        tracing::info!("{}", t("main-masking-gate-ready"));
    }

    // 订阅账号池（feature `subscription-vendor`）：provider/client/池装配 +
    // `[[subscription_accounts]]` 逐条登记（vault 有 refresh token 即 Active，
    // 无则待授权）。`[subscription]`.enabled 是运行时门（热读），池常在位
    // ——启用与授权均无需重启。无库形态（vault 特性关）refresh token 不落盘，
    // 刷新路径会降级（配置校验层已提示）。
    #[cfg(feature = "subscription-vendor")]
    {
        let (client_id, accounts) = {
            let snapshot = config_manager.read();
            (
                snapshot.subscription.client_id.clone(),
                snapshot.subscription_accounts.clone(),
            )
        };
        let pool = assemble_subscription_pool(
            vendor_manager.vault(),
            client_id,
            &accounts,
            vendor_manager.metrics().clone(),
        )
        .await;
        vendor_manager.set_subscription_pool(pool);
        tracing::info!("{}", t("main-subscription-pool-ready"));
    }

    // 成本内存视图启动恢复：文件库时按窗口聚合 token_usage 回填
    // CostTracker（先恢复再对外服务）；内存库无跨重启数据，warn 跳过。
    #[cfg(feature = "cost-tracking")]
    {
        let window_hours = config_manager.read().metrics.cost_restore_window_hours;
        restore_cost_tracker(&vendor_manager, &database.pool, &db_url, window_hours).await;
    }

    // OTLP 指标桥（feature = "otlp" + [otlp].enabled 双门控）：启动期一次性
    // 装配；exporter 构建失败 fail-open（warn 后停用，服务不受影响）。
    #[cfg(feature = "otlp")]
    let (otlp_bridge, otlp_task) = {
        match crate::utils::otlp::bridge_from_section(
            &config_manager.read().otlp,
            vendor_manager.cost_tracker().clone(),
        ) {
            crate::utils::otlp::BridgeSpawn::Disabled => (None, None),
            crate::utils::otlp::BridgeSpawn::Failed(e) => {
                tracing::warn!(
                    "{}",
                    t_args("main-otlp-bridge-build-failed", &[("error", e)])
                );
                (None, None)
            }
            crate::utils::otlp::BridgeSpawn::Ready(bridge) => {
                let task =
                    tokio::spawn(run_otlp_bridge_loop(bridge.clone(), config_manager.clone()));
                tracing::info!(
                    "{}",
                    t_args(
                        "main-otlp-bridge-started",
                        &[(
                            "endpoint",
                            redact_endpoint(&config_manager.read().otlp.endpoint)
                        )]
                    )
                );
                (Some(bridge), Some(task))
            }
        }
    };

    // 嵌入兼容组 pin 表（进程内存）：与 Router 共享；配置应用成功（落盘写回
    // 或热载桥接）后清空重新粘着——旧配置时代的组划分/维度基准一并失效。
    // 钩子经 ConfigManager 注册表在两处 apply 成功路径统一触发（manager.rs）。
    let embedding_pin = Arc::new(crate::router::EmbeddingPin::new());
    register_embedding_pin_hook(
        &config_manager,
        embedding_pin.clone(),
        vendor_manager.metrics().clone(),
    );

    // 密钥保密柜（key-vault）：master key 分层解析 → 按构建形态选后端（db 优先，
    // 无库回落文件柜）→ env 吸收（幂等不覆盖）→ 全量注入 inline key pool。
    #[cfg(feature = "vault")]
    {
        use crate::vault::{load_or_create_master_key, KeyStore};
        let master_key = load_or_create_master_key()?;
        let store: Arc<dyn KeyStore> = {
            #[cfg(feature = "database")]
            {
                Arc::new(crate::vault::DbKeyStore::new(
                    master_key,
                    database.pool.clone(),
                ))
            }
            #[cfg(not(feature = "database"))]
            {
                Arc::new(crate::vault::FileKeyStore::open(
                    master_key,
                    crate::vault::home_vault_path()?,
                )?)
            }
        };
        let vendors = config_manager.read().vendors.clone();
        let lookup = crate::vault::std_env_lookup;
        let absorbed = crate::vault::absorb_env_keys(store.as_ref(), &vendors, &lookup).await?;
        if absorbed > 0 {
            tracing::info!(
                "{}",
                t_args(
                    "main-vault-absorbed-keys",
                    &[("absorbed", absorbed.to_string())],
                )
            );
        }
        // 柜句柄挂 VendorManager：API 管理面（/admin/vault/keys）经
        // `vault` 取用；不挂则管理面恒 503（装配期一次性）。
        vendor_manager.set_vault(store.clone());
        for vendor in &vendors {
            if vendor.enabled {
                crate::vault::inject_vendor_keys(store.as_ref(), &vendor_manager, &vendor.id)
                    .await?;
            }
        }
        store
    };

    // 日志管道：AsyncLogger 批量落 request_logs，TokenPreAggregator 按小时预聚合 usage。
    let logger_handle = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
        database.repository.clone(),
        1024,
        std::time::Duration::from_secs(1),
    )));
    let pre_aggregator = Arc::new(TokenPreAggregator::new(
        database.repository.clone(),
        std::time::Duration::from_secs(60),
    ));
    // flush worker 挂后台，停机位在 serve 结束后置位
    let pa_shutdown = Arc::new(AtomicBool::new(false));
    {
        let pa = pre_aggregator.clone();
        let flag = pa_shutdown.clone();
        tokio::spawn(async move {
            pa.start_flush_worker(flag).await;
        });
    }
    tracing::info!("{}", t("main-log-pipeline-initialized"));

    // 构建 trait-kit AsyncKit DI 容器（oxcache → dbnexus → inklog + limiteron governor）
    #[cfg(all(
        feature = "infrastructure",
        feature = "database",
        feature = "vendor-management",
        feature = "logging"
    ))]
    {
        tracing::info!("{}", t("main-building-di-container"));
        let di_db_config = dbnexus::foundation::config::DbConfig {
            url: db_url.clone(),
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

    // LinUCB 状态（Router 选择与 handler 奖励回传共享）；selection=weighted 时不咨询不更新。
    let bandit_state = Arc::new(crate::router::bandit::BanditState::new(
        config_manager.read().routing.ucb_alpha,
    ));

    // 会话亲和表（oxcache memory 后端，TTL 1h 活跃续期）。
    let affinity_table = Arc::new(crate::router::affinity::AffinityTable::with_memory_backend(
        crate::router::affinity::DEFAULT_AFFINITY_TTL,
    ));

    // 缓存经济学亲和（cache-economy-affinity）：粘性记账表 + write-behind
    // 落盘管道。legacy 模式下 settle 不消费落盘通道（不落盘），管道仍装配
    // 以便热切到 auto/turn 时立即生效。
    let affinity_sticks = Arc::new(crate::router::affinity::AffinityStickTable::new());
    let affinity_sink: Option<Arc<dyn crate::storage::AffinityStickSink>> =
        Some(Arc::new(crate::database::AffinityPersister::new(
            database.repository.clone(),
            crate::database::affinity_persist::DEFAULT_SLOTS,
            crate::database::affinity_persist::DEFAULT_FLUSH_INTERVAL,
        )));
    let affinity_writer = Arc::new(crate::router::affinity_settle::AffinityWriter::new(
        config_manager.clone(),
        affinity_table.clone(),
        affinity_sticks.clone(),
        affinity_sink.clone(),
        vendor_manager.metrics().clone(),
    ));

    // 精确响应缓存（oxcache memory）；enabled/TTL 由 [cache] 段热读，默认关闭。
    // O4 语义响应缓存层（semantic-cache feature + [cache.semantic].enabled；
    // BGE 加载失败 → 旁路 + ERROR 限频，精确缓存不受影响）
    let semantic_layer: Option<std::sync::Arc<crate::api::semantic_cache::SemanticCacheLayer>> = {
        let sem_cfg = config_manager.read().cache.semantic.clone();
        if !sem_cfg.enabled {
            None
        } else {
            #[cfg(feature = "semantic-cache")]
            {
                match crate::api::semantic_cache::BgeEmbedder::load(&sem_cfg.model_path) {
                    Ok(embedder) => Some(std::sync::Arc::new(
                        crate::api::semantic_cache::SemanticCacheLayer::new(
                            sem_cfg.similarity,
                            sem_cfg.max_entries,
                            std::sync::Arc::new(embedder),
                        ),
                    )),
                    Err(error) => {
                        tracing::error!(%error, "{}", t("main-semantic-cache-disabled"));
                        None
                    }
                }
            }
            #[cfg(not(feature = "semantic-cache"))]
            {
                tracing::error!("{}", t("main-semantic-cache-feature-missing"));
                None
            }
        }
    };
    let response_cache = Arc::new(crate::api::response_cache::ResponseCache::with_semantic(
        Arc::new(oxcache::backend::default_memory_backend()),
        config_manager.clone(),
        vendor_manager.metrics().clone(),
        semantic_layer,
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

    let db_sink: Arc<dyn RequestLogSink> = database.clone();
    let log_front: Option<Arc<dyn RequestLogSink>> = Some(logger_handle.clone());
    let usage_front: Option<Arc<dyn UsageSink>> = Some(pre_aggregator.clone());

    // 难度定档路由（[routing.difficulty]）：enabled=false 不接线（模块旁路，
    // 行为与历史一致）。接线 = 决策缓存 + 熔断 guard + 分类器（feature 门控；
    // laya 加载失败 → F3 常驻纯规则降级，ERROR 级显性日志）
    let difficulty_router: Option<Arc<crate::router::difficulty::DifficultyRouter>> = {
        let d = config_manager.read().routing.difficulty.clone();
        if !d.enabled {
            None
        } else {
            let cache = crate::router::difficulty::DecisionCache::with_memory_backend(
                d.classifier.cache_ttl_secs,
            );
            #[cfg(feature = "difficulty-classifier")]
            let (classifier, guard) = if d.w_clf > 0.0 {
                match crate::router::difficulty::vecboost_engine::LayaClassifier::load(
                    &d.classifier.model_path,
                    &d.classifier.engine,
                ) {
                    Ok(engine) => (
                        Some(Arc::new(engine)
                            as Arc<dyn crate::router::difficulty::DifficultyClassifier>),
                        crate::router::difficulty::Guard::new(
                            d.classifier.failure_threshold,
                            d.classifier.probe_interval_secs,
                        ),
                    ),
                    Err(error) => {
                        tracing::error!(
                            %error,
                            "{}",
                            t("main-difficulty-classifier-disabled")
                        );
                        (None, crate::router::difficulty::Guard::always_open())
                    }
                }
            } else {
                (
                    None,
                    crate::router::difficulty::Guard::new(
                        d.classifier.failure_threshold,
                        d.classifier.probe_interval_secs,
                    ),
                )
            };
            #[cfg(not(feature = "difficulty-classifier"))]
            // V4 已保证该构建组合下 w_clf=0（enabled 时），纯规则形态
            let (classifier, guard) = (
                None,
                crate::router::difficulty::Guard::new(
                    d.classifier.failure_threshold,
                    d.classifier.probe_interval_secs,
                ),
            );
            Some(Arc::new(crate::router::difficulty::DifficultyRouter::new(
                config_manager.clone(),
                classifier,
                guard,
                cache,
                vendor_manager.metrics().clone(),
            )))
        }
    };

    // 配额账本 + 落盘接线（quota-aware-routing T005）：结算 UPSERT（单事务
    // 批量）+ 30s 周期全量 flush + 启动恢复活动窗 + 保留清理。repository
    // 恢复读取失败不静默——ERROR 留痕后按空账本启动（预算约束短暂放松，
    // 与 flush 误差同性质）。
    let quota_state = Arc::new(crate::middleware::quota::QuotaState::new());
    // 停机最终 flush 句柄（快照生产方 + 落盘器；未装配 = None，收尾 no-op）
    // 装配块内恒定赋值（无初值声明：装配路径唯一，None 形态不存在）
    let quota_ledger_teardown: Option<(
        Arc<crate::middleware::quota::QuotaState>,
        Arc<crate::middleware::quota_persist::QuotaLedgerPersister>,
    )>;
    {
        let persister = Arc::new(crate::middleware::quota_persist::QuotaLedgerPersister::new(
            database.repository.clone(),
            crate::middleware::quota_persist::DEFAULT_LEDGER_SLOTS,
            Some(vendor_manager.metrics().clone()),
        ));
        // 恢复读下界 = now − 配置最长窗（SQL 层滤掉不可能仍活动的历史周期
        // 行，恢复内存不随历史增长；与保留清理同参单源）。
        let read_lower_bound = chrono::Utc::now().timestamp()
            - crate::middleware::quota::ledger_retention_secs(&config_manager.read());
        match database
            .repository
            .read_quota_ledger(read_lower_bound)
            .await
        {
            Ok(rows) => {
                let restored = quota_state.restore_from_ledger(&rows, &config_manager.read());
                if restored > 0 {
                    tracing::info!(
                        "{}",
                        t_args(
                            "main-quota-ledger-restored",
                            &[("count", restored.to_string())]
                        )
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    "{}",
                    t_args(
                        "main-quota-ledger-restore-failed",
                        &[("error", e.to_string())]
                    )
                );
            }
        }
        quota_state.set_ledger_persister(persister.clone());
        // 30s 全量 flush 循环：内存态权威，快照周期落库（崩溃回退 ≤30s）；
        // 保留清理随循环低频执行（默认每小时一次）——过期周期行按配置最长
        // 窗下界删除，表增长有界。
        let flush_state = quota_state.clone();
        let flush_persister = persister.clone();
        let flush_repo = database.repository.clone();
        let flush_config = config_manager.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(
                crate::middleware::quota_persist::DEFAULT_LEDGER_FLUSH_INTERVAL,
            );
            ticker.tick().await; // 首个 tick 立即完成——消费掉
            let mut ticks: u64 = 0;
            loop {
                ticker.tick().await;
                // 优雅停机最终 flush（DrainQueue 钩）关停落盘器后本循环退出，
                // 不再向已关闭通道投递快照（避免停机后失败计数噪声）
                if flush_persister.is_closed() {
                    break;
                }
                flush_persister.write(flush_state.ledger_snapshot());
                ticks += 1;
                if ticks.is_multiple_of(
                    crate::middleware::quota_persist::DEFAULT_LEDGER_PURGE_EVERY_TICKS,
                ) {
                    let not_before = chrono::Utc::now().timestamp()
                        - crate::middleware::quota::ledger_retention_secs(&flush_config.read());
                    match flush_repo.purge_expired_quota_ledger(not_before).await {
                        Ok(0) => {}
                        Ok(n) => {
                            tracing::debug!(
                                "{}",
                                t_args("main-quota-ledger-purged", &[("count", n.to_string())])
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "{}",
                                t_args(
                                    "main-quota-ledger-purge-failed",
                                    &[("error", e.to_string())]
                                )
                            );
                        }
                    }
                }
            }
        });
        // 优雅停机最终 flush 句柄（Assembly → ServerHandle → 收尾协调器）：
        // 快照 + 直落 + persister shutdown，干净重启不再吃 ≤30s 回退窗口。
        quota_ledger_teardown = Some((quota_state.clone(), persister));
    }

    let api_state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(
            router
                .with_affinity(affinity_table.clone())
                .with_bandit(bandit_state.clone())
                .with_embedding_pin(embedding_pin)
                .with_quota_state(quota_state.clone()),
        ),
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: db_sink,
        logger: log_front,
        pre_aggregator: usage_front,
        affinity: Some(affinity_table.clone()),
        affinity_writer: Some(affinity_writer.clone()),
        response_cache: Some(response_cache),
        bandit: Some(bandit_state),
        // 配额账本：中间件入口预扣与回补钩共享同一实例（enabled=false 时零判限）。
        // 落盘接线（quota-aware-routing）：结算 UPSERT + 30s 全量 flush +
        // 启动恢复活动窗——repository 未接线（无 database 特性）时纯内存态。
        quota: Some(quota_state.clone()),
        // 难度定档：enabled=false / 分类器未接线时为 None（历史行为）
        difficulty: difficulty_router,
        // Responses 有状态：存储接 Repository（端点开关/保留期请求期热读）
        responses: Some(Arc::new(crate::api::responses::ResponsesRuntime::new(
            database.repository.clone(),
        ))),
        // 搜索注入运行时（web-search feature；[web_search].enabled 运行时热读门）
        #[cfg(feature = "web-search")]
        web_search: Some(Arc::new(crate::api::web_search::WebSearchRuntime::new())),
    });

    crate::api::init_global_state(api_state.clone());
    tracing::info!("{}", t("main-global-state-initialized"));

    // 会话粘性启动恢复（`[routing.affinity]` 为 auto/turn 时）：把未过期的
    // 粘性按剩余 TTL 灌回内存亲和表，避免重启后刚建好的前缀缓存作废。
    {
        let mode =
            crate::config::AffinityMode::from_section(&config_manager.read().routing.affinity);
        crate::server::affinity_restore::restore_affinity_sticks(
            mode,
            &affinity_table,
            &affinity_sticks,
            affinity_sink.as_ref(),
            vendor_manager.metrics(),
        )
        .await;
    }

    // Responses 孤儿后台任务：进程重启后 queued/in_progress 不再有人推进，
    // 统一置 failed（失败只 warn，不阻断启动）。
    match database.repository.mark_orphaned_responses_failed().await {
        Ok(count) if count > 0 => {
            tracing::warn!(
                count,
                "{}",
                t_args(
                    "api-responses-orphans-failed",
                    &[("count", count.to_string())]
                )
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                error = %error,
                "{}",
                t_args(
                    "api-responses-orphan-cleanup-failed",
                    &[("error", error.to_string())]
                )
            );
        }
    }

    // 健康探针常驻（mode/interval/strategy 每周期热读），句柄移交 Assembly 停机 abort。
    let probe_handle = Some(tokio::spawn(crate::vendor::probe::run_probe_loop(
        vendor_manager.clone(),
        config_manager.clone(),
    )));

    // 目录同步周期任务（catalog-sync 双门控：enabled 且 interval_secs>0 才
    // 挂载；interval=0 = 不周期，仅 admin 手动触发）。句柄移交 Assembly 停机 abort。
    #[cfg(feature = "catalog-sync")]
    let catalog_sync_task = {
        if catalog_sync_task_due(&config_manager.read().catalog_sync) {
            let handle = tokio::spawn(run_catalog_sync_loop(
                config_manager.clone(),
                vendor_manager.metrics().clone(),
            ));
            tracing::info!(
                "{}",
                t_args(
                    "main-catalog-sync-started",
                    &[(
                        "interval_secs",
                        config_manager.read().catalog_sync.interval_secs.to_string(),
                    )]
                )
            );
            Some(handle)
        } else {
            None
        }
    };

    // 就绪探针注册（sdforge 全局表，装配期一次性；检查不发起上游请求）。
    crate::api::readiness::register_readiness_checks(&api_state, &database.pool);
    let hot_reloader = crate::server::hot_reload::maybe_start_hot_reload(&config_manager).await;

    Ok(Assembly {
        api_state,
        logger_handle,
        pre_aggregator,
        pa_shutdown,
        hot_reloader,
        probe_handle,
        quota_ledger_teardown,
        #[cfg(feature = "otlp")]
        otlp_bridge,
        #[cfg(feature = "otlp")]
        otlp_task,
        #[cfg(feature = "catalog-sync")]
        catalog_sync_task,
    })
}

/// 目录同步周期任务挂载判定（pure，测试钉）：双门控 enabled 且
/// interval_secs > 0（0 = 不周期）。
#[cfg(feature = "catalog-sync")]
pub(crate) fn catalog_sync_task_due(section: &crate::config::CatalogSyncSection) -> bool {
    section.enabled && section.interval_secs > 0
}

/// 订阅账号池装配（feature `subscription-vendor`）：provider 固定 Anthropic
/// OAuth 契约；独立 http 客户端（低频 OAuth 往返，与出站大流量池隔离）；
/// store 取保密柜句柄（None = 无库形态，refresh token 仅进程内）；声明账号
/// 逐条登记（vault 有 refresh token 即 Active，无则待授权）。
#[cfg(feature = "subscription-vendor")]
async fn assemble_subscription_pool(
    store: Option<Arc<dyn crate::vault::KeyStore>>,
    client_id: String,
    accounts: &[crate::config::SubscriptionAccountConfig],
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
) -> Arc<crate::vendor::subscription::pool::AccountPool> {
    let provider: Arc<dyn crate::vendor::subscription::SubscriptionProvider> =
        Arc::new(crate::vendor::subscription::anthropic::AnthropicProvider::new());
    // OAuth 往返独立 client（低频，与出站大流量池隔离）必须带超时：刷新在
    // per-account 刷新锁内 await、credential_for 顺序等候选——零超时下上游
    // token 端点「连上不响应」会楔死该 vendor 全部订阅路径，只能重启恢复
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("subscription oauth http client");
    let pool = Arc::new(crate::vendor::subscription::pool::AccountPool::new(
        provider, http, client_id, store, metrics,
    ));
    for account in accounts {
        if pool
            .register_declared(&account.id, &account.vendor_id)
            .await
        {
            tracing::info!(
                "{}",
                t_args(
                    "main-subscription-account-registered",
                    &[("account", account.id.clone())]
                )
            );
        }
    }
    pool
}

/// 目录同步周期循环（catalog-sync feature）：首轮立即同步（此后按
/// interval ±10% 抖动错峰）；enabled/interval 每轮热读——运行期关闭或
/// interval 改 0 时休眠待启（admin 手动触发不受影响）。失败 fail-open：
/// catalog 原样、fail_total 已在 plan_from_source 记，此处只 warn。
#[cfg(feature = "catalog-sync")]
async fn run_catalog_sync_loop(
    config_manager: Arc<crate::config::ConfigManager>,
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
) {
    loop {
        let section = config_manager.read().catalog_sync.clone();
        if !catalog_sync_task_due(&section) {
            // 热关/改 0：休眠待启（保持任务存活，重新打开无需重启）
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }
        let snapshot = config_manager.read();
        let (section, existing) = (
            snapshot.catalog_sync.clone(),
            snapshot.model_catalog.clone(),
        );
        drop(snapshot);
        match crate::config::catalog_sync::plan_from_source(&section, &existing, &metrics).await {
            Ok(crate::config::catalog_sync::PlanOutcome::NotModified) => {}
            Ok(crate::config::catalog_sync::PlanOutcome::Plan(plan, etag)) => {
                match crate::config::catalog_sync::persist_plan(&config_manager, &plan, &metrics) {
                    Ok(()) => {
                        // etag 随 persist 成功回存：失败不回存，下轮全量重拉重试
                        crate::config::catalog_sync::commit_etag(etag.as_deref());
                        tracing::info!(
                            "{}",
                            t_args(
                                "main-catalog-sync-synced",
                                &[("added", plan.additions.len().to_string())]
                            )
                        )
                    }
                    Err(e) => tracing::warn!(
                        "{}",
                        t_args("main-catalog-sync-failed", &[("error", e.to_string())])
                    ),
                }
            }
            Err(e) => {
                tracing::warn!(
                    "{}",
                    t_args("main-catalog-sync-failed", &[("error", e.to_string())])
                );
            }
        }
        // ±10% 抖动（0.9..=1.1）：SystemTime 纳秒做非确定性相位，防多实例
        // 同刻齐打上游（与 retry spread_jitter 同目的；周期语义不引入 rand 依赖）
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let jitter = 0.9 + (nanos % 201) as f64 / 1000.0;
        let period = (section.interval_secs as f64 * jitter).max(1.0) as u64;
        tokio::time::sleep(std::time::Duration::from_secs(period)).await;
    }
}

/// 启动日志用的端点脱敏：去掉 userinfo 与 query（凭证可能藏在两者中）。
#[cfg(feature = "otlp")]
fn redact_endpoint(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.to_string()
        }
        Err(_) => "<invalid endpoint>".to_string(),
    }
}

/// OTLP 桥轮询环（feature = "otlp"）：启动立即首拉（store 先于首次采集），
/// 之后按 interval 周期；`[otlp].enabled` 热读（false 跳过推送；启动期
/// disabled 不建任务，事后开启需重启进程）。
#[cfg(feature = "otlp")]
async fn run_otlp_bridge_loop(
    bridge: Arc<crate::utils::otlp::OtlpBridge>,
    config_manager: Arc<crate::config::ConfigManager>,
) {
    use tokio::time::MissedTickBehavior;

    let period = std::time::Duration::from_secs(config_manager.read().otlp.interval_secs.max(1));
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    bridge.poll_once();
    ticker.tick().await;
    loop {
        ticker.tick().await;
        if !config_manager.read().otlp.enabled {
            // 运行期关闭必须同时清空 store：否则 PeriodicReader 仍按周期把
            // 上一轮残留样本发出去——「导出总开关」不能只挡采集不挡发送。
            bridge.clear_series();
            continue;
        }
        bridge.poll_once();
    }
}

/// 注册「配置应用成功 → 清空嵌入 pin」钩子（提炼自 assemble 的接缝，
/// 单测覆盖注册行为：热载/落盘两条 apply 成功路径经 ConfigManager 注册表
/// 统一触发；校验失败的应用内存态未切换、不触发）
pub(crate) fn register_embedding_pin_hook(
    config_manager: &Arc<crate::config::ConfigManager>,
    pin: Arc<crate::router::EmbeddingPin>,
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
) {
    config_manager.register_swap_hook(Arc::new(move || {
        let cleared = pin.clear_all();
        if cleared > 0 {
            metrics.record_embedding_pin_reset(cleared as u64);
        }
    }));
}

/// 启动恢复 CostTracker 内存成本视图：内存库（`:memory:`）无跨重启数据，
/// warn 跳过；文件库按窗口聚合 `token_usage` 回填。失败仅 warn 不阻断启动
///（恢复是观测增强，不失败服务）。
#[cfg(feature = "cost-tracking")]
async fn restore_cost_tracker(
    vendor_manager: &crate::vendor::VendorManager,
    pool: &dbnexus::DbPool,
    db_url: &str,
    window_hours: u64,
) {
    if db_url.contains(":memory:") {
        tracing::warn!("{}", t("main-cost-restore-skipped-memory"));
        return;
    }
    match vendor_manager
        .cost_tracker()
        .restore_from_db(pool, window_hours)
        .await
    {
        Ok(groups) => tracing::info!(
            "{}",
            t_args(
                "main-cost-restored",
                &[
                    ("groups", groups.to_string()),
                    ("window_hours", window_hours.to_string()),
                ],
            )
        ),
        Err(e) => tracing::warn!(
            "{}",
            t_args("main-cost-restore-failed", &[("error", e.to_string())])
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::async_logger::GateSink;
    use inklog::LogSink;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt;

    /// 内存模式（`:memory:`）启动恢复跳过：不 panic 且计数为零。
    #[tokio::test]
    async fn cost_restore_skips_memory_db_without_panic() {
        let toml_v = "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n[database]\npath = \":memory:\"\n";
        let config: crate::config::Config =
            crate::config::parse_config_content(std::path::Path::new("memory.toml"), toml_v)
                .expect("样例配置解析");
        let manager = Arc::new(crate::config::ConfigManager::new(config));
        let vendor_manager = crate::vendor::VendorManager::new(manager);

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
        .expect("memory pool");

        restore_cost_tracker(&vendor_manager, &pool, "sqlite::memory:", 24).await;

        assert!(
            vendor_manager.cost_tracker().total_usd() == 0.0,
            "内存模式跳过恢复后计数必须为零"
        );
    }

    /// 目录同步周期任务挂载判定（T006）：双门控——enabled=false 不挂任务、
    /// interval_secs=0 不挂任务（仅 admin 手动触发）、两者同时满足才挂。
    #[cfg(feature = "catalog-sync")]
    #[test]
    fn catalog_sync_task_due_requires_enabled_and_positive_interval() {
        let disabled = crate::config::CatalogSyncSection::default();
        assert!(
            !catalog_sync_task_due(&disabled),
            "缺省（enabled=false）必须不挂周期任务"
        );
        let no_interval = crate::config::CatalogSyncSection {
            enabled: true,
            ..crate::config::CatalogSyncSection::default()
        };
        assert!(
            !catalog_sync_task_due(&no_interval),
            "interval_secs=0 必须不挂周期任务（仅手动触发）"
        );
        let due = crate::config::CatalogSyncSection {
            enabled: true,
            interval_secs: 3600,
            ..crate::config::CatalogSyncSection::default()
        };
        assert!(
            catalog_sync_task_due(&due),
            "enabled + interval>0 必须挂周期任务"
        );
    }

    /// F4：嵌入 pin 清空钩子注册（assemble 接缝函数）——配置应用成功后
    /// pin 全清并按条数计数
    #[test]
    fn embedding_pin_swap_hook_clears_and_counts_on_successful_apply() {
        use std::path::Path;

        let toml_v = |port: u16| {
            format!(
                "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = {port}\n\n[database]\npath = \":memory:\"\n"
            )
        };
        let config: crate::config::Config =
            crate::config::parse_config_content(Path::new("memory.toml"), &toml_v(8080))
                .expect("样例配置解析");
        let manager = Arc::new(crate::config::ConfigManager::new(config));
        let pin = Arc::new(crate::router::EmbeddingPin::new());
        let metrics = Arc::new(crate::utils::metrics::GlobalMetrics::new());
        pin.pin_or_get("emb-a", "g1");
        pin.record_and_check("v", "m", "-", 1024).ok();
        register_embedding_pin_hook(&manager, pin.clone(), metrics.clone());
        assert_eq!(
            metrics.snapshot().embedding_pin_reset_total,
            0,
            "注册本身不触发"
        );

        let next: crate::config::Config =
            crate::config::parse_config_content(Path::new("memory.toml"), &toml_v(8081))
                .expect("变体配置解析");
        manager.swap_memory(next).expect("内存态应用成功");
        assert!(pin.list().is_empty(), "应用成功后 pin 全清");
        assert_eq!(
            metrics.snapshot().embedding_pin_reset_total,
            1,
            "按清除条数计入 reset 计数"
        );
    }

    /// 装配运行时验证：通道拓扑错误只在构建期暴露。
    /// 断言 log 门面桥已生效（LogTracer::init 把全局 max_level 提到 Trace）。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_init_logger_builds_masked_console_assembly() {
        let manager = init_logger().await.expect("init_logger 必须能构建");
        manager.shutdown().ok();
        // 桥安装必须随装配被触及（旗标仅由 install_log_facade_bridge 设置）。
        assert!(
            log_bridge_touched(),
            "init_logger 必须安装 log 门面桥（install_log_facade_bridge）"
        );
        // 并发测试先行安装时亦为 Trace，弱补强断言
        assert_eq!(
            log::max_level(),
            log::LevelFilter::Trace,
            "log 门面桥应把全局 max_level 提到 Trace"
        );
    }

    /// 桥端到端：log 门面记录经 LogTracer 转 tracing 后必须经 GateSink 脱敏出站。
    #[test]
    fn test_log_facade_bridge_forwards_to_gate_sink_redaction() {
        let _ = tracing_log::LogTracer::init();

        #[derive(Default)]
        struct RecordingSink(Mutex<Vec<inklog::LogRecord>>);

        #[async_trait::async_trait]
        impl LogSink for RecordingSink {
            async fn write(&self, record: &inklog::LogRecord) -> Result<(), inklog::InklogError> {
                self.0.lock().expect("not poisoned").push(record.clone());
                Ok(())
            }
            async fn flush(&self) -> Result<(), inklog::InklogError> {
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), inklog::InklogError> {
                Ok(())
            }
        }

        struct FieldExtractor {
            message: String,
            log_target: Option<String>,
        }
        impl tracing::field::Visit for FieldExtractor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{value:?}");
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                // tracing-log 布局：事件 target 固定 "log"，原始 target 在 log.target 字段
                if field.name() == "log.target" {
                    self.log_target = Some(value.to_string());
                }
            }
        }

        let inner = Arc::new(RecordingSink::default());
        let gate_sink = Arc::new(GateSink::new(
            inner.clone(),
            Arc::new(crate::database::async_logger::outbound_masker()),
        ));

        struct BridgeCaptureLayer(Arc<GateSink>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for BridgeCaptureLayer {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut extractor = FieldExtractor {
                    message: String::new(),
                    log_target: None,
                };
                event.record(&mut extractor);
                let mut fields = std::collections::HashMap::new();
                if let Some(log_target) = extractor.log_target {
                    fields.insert("log.target".to_string(), serde_json::json!(log_target));
                }
                let record = inklog::LogRecord {
                    target: event.metadata().target().to_string(),
                    level: event.metadata().level().to_string(),
                    message: extractor.message,
                    fields,
                    ..Default::default()
                };
                // Layer 回调是同步上下文，block_on 驱动 GateSink.write 即可
                futures::executor::block_on(self.0.write(&record)).ok();
            }
        }

        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        let subscriber = tracing_subscriber::registry().with(BridgeCaptureLayer(gate_sink));
        tracing::subscriber::with_default(subscriber, || {
            log::info!(target: "fluxgate::bridge_test", "gateway boot token {secret}");
        });

        let written = inner.0.lock().expect("not poisoned");
        assert_eq!(
            written.len(),
            1,
            "log 门面记录必须经桥到达 tracing → GateSink 链"
        );
        assert_eq!(written[0].target, "log", "桥转发事件的特征 target");
        assert_eq!(
            written[0].fields["log.target"],
            serde_json::json!("fluxgate::bridge_test"),
            "原始 log target 必须保留在 log.target 字段"
        );
        assert!(
            !written[0].message.contains(secret),
            "出站 message 必须已脱敏: {}",
            written[0].message
        );
        assert!(
            written[0].message.contains("***REDACTED_API_KEY***"),
            "脱敏标记必须可见: {}",
            written[0].message
        );
    }
}
