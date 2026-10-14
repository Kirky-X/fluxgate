// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 装配链：logger → config → db → vendor → 日志管道 → DI kit →
//! router/bandit/affinity/response_cache → ApiState → init_global_state。

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::config::loader::ConfigLoader;
use crate::config::ConfigManager;
use crate::database::AsyncLoggerHandle;
use crate::database::TokenPreAggregator;
use crate::error::GateError;
use crate::error::NexusFlowError;
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
pub(crate) async fn init_logger() -> Result<inklog::LoggerManager, NexusFlowError> {
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
            NexusFlowError::from(GateError::Config {
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

/// 装配 db → vendor → 日志管道 → DI kit → 路由栈 → ApiState。
pub(crate) async fn assemble(
    config_manager: Arc<ConfigManager>,
) -> Result<Assembly, NexusFlowError> {
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

    // 密钥保密柜（key-vault）：master key 分层解析 → 按构建形态选后端（db 优先，
    // 无库回落文件柜）→ env 吸收（幂等不覆盖）→ 全量注入 inline key pool。
    #[cfg(feature = "vault")]
    let _vault_store: Arc<dyn crate::vault::KeyStore> = {
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
            tracing::info!("vault: absorbed {absorbed} key(s) from env");
        }
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
                            "difficulty classifier disabled: laya load failed; rule-only mode engaged"
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
                .with_bandit(bandit_state.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::async_logger::GateSink;
    use inklog::LogSink;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt;

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
            log::info!(target: "nexusflow::bridge_test", "gateway boot token {secret}");
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
            serde_json::json!("nexusflow::bridge_test"),
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
