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
use crate::storage::{RequestLogSink, UsageSink};
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
        // `vault()` 取用；不挂则管理面恒 503（装配期一次性）。
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

    let api_state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(
            router
                .with_affinity(affinity_table.clone())
                .with_bandit(bandit_state.clone())
                .with_embedding_pin(embedding_pin),
        ),
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: db_sink,
        logger: log_front,
        pre_aggregator: usage_front,
        affinity: Some(affinity_table.clone()),
        response_cache: Some(response_cache),
        bandit: Some(bandit_state),
        // 配额账本：中间件入口预扣与回补钩共享同一实例（enabled=false 时零判限）。
        quota: Some(Arc::new(crate::middleware::quota::QuotaState::new())),
        // 难度定档：enabled=false / 分类器未接线时为 None（历史行为）
        difficulty: difficulty_router,
    });

    crate::api::init_global_state(api_state.clone());
    tracing::info!("{}", t("main-global-state-initialized"));

    // 健康探针常驻（mode/interval/strategy 每周期热读），句柄移交 Assembly 停机 abort。
    let probe_handle = Some(tokio::spawn(crate::vendor::probe::run_probe_loop(
        vendor_manager.clone(),
        config_manager.clone(),
    )));

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
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::async_logger::GateSink;
    use inklog::LogSink;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt;

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
