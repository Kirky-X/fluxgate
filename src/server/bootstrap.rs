// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 装配链：logger → config → db → vendor → 日志管道 → DI kit →
//! router/bandit/affinity/response_cache → ApiState → init_global_state。
//!
//! 自 main.rs:22-162 平移，装配顺序与原二进制逐位一致。

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
}

/// 初始化日志系统（原 main.rs:22-29）。
///
/// 返回的 LoggerManager 句柄由调用方（run）持有到进程结束——
/// 原二进制中 `let _logger` 活在 main 作用域，提前丢弃会使 inklog
/// worker 断连、tracing 事件静默丢失。嵌入式调用方自管日志。
///
/// 出站脱敏：自建 [`GateSink`] 包装内置 ConsoleSink（fail-closed + 超门限
/// 截断，与落库通路同一 fail 姿态；见
/// [`crate::database::async_logger::redact_text`]）。内置 console 通道的
/// masker 由 inklog 写死为 `DataMasker::new()`（fail-open 无 gate），
/// LoggerManager builder 层无注入配置——故 `console(false)` 关内置通道
/// （防同一条记录双写），经 add_sink 挂自建 sink。行为代价：自定义 sink
/// 通道会进 ops 事件广播，内部故障 ops 事件从此上控制台。
///
/// log 门面（reqwest/sea-orm/tokio-tungstenite 等依赖）：inklog 的
/// LogAdapter 结构上只投 console+主 async 通道，自定义 sink 通道到不了，
/// `console(false)` 后这些记录会无痕消失——故先装 tracing-log 的
/// [`tracing_log::LogTracer`] 把 log 记录转成 tracing 事件进统一总线
/// （随 tracing 事件走 gate console sink 输出并脱敏）；inklog 随后自身
/// 的 LogLogger 安装会因全局 logger 已设置而良性跳过（其装配注释明言
/// 该场景为预期）。重复初始化（测试）时 LogTracer 安装失败被忽略。
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

/// log 门面桥的进程级安装尝试标记：init_logger 每次装配都会触及——
/// 装配测试以此锁「桥接线存在」与执行顺序无关（LogTracer 全局单例的
/// 成功与否无法作为接线证据：并发测试的桥机制测试也会尝试安装）。
static LOG_BRIDGE_TOUCHED: AtomicBool = AtomicBool::new(false);

fn install_log_facade_bridge() {
    LOG_BRIDGE_TOUCHED.store(true, Ordering::Relaxed);
    if let Err(e) = tracing_log::LogTracer::init() {
        tracing::debug!("log facade bridge already installed: {e}");
    }
}

/// 桥接线是否被 init_logger 触及（仅装配测试读取）。
#[cfg(test)]
pub(crate) fn log_bridge_touched() -> bool {
    LOG_BRIDGE_TOUCHED.load(Ordering::Relaxed)
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
    // URL 单一出口（db-backend-selection D2）：主库与下方 DI kit 共用同一
    // 解析结果——url 配置优先，path 按编译期后端回退
    let db_url = resolve_database_url(&db_config)?;
    // 日志走脱敏形态：url 配置（postgres:// 等）惯例内嵌口令，原文只进
    // 连接参数，绝不进日志通道（redact_db_url 剥 userinfo 留 host/port）
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

    let db_sink: Arc<dyn RequestLogSink> = database.clone();
    let log_front: Option<Arc<dyn RequestLogSink>> = Some(logger_handle.clone());
    let usage_front: Option<Arc<dyn UsageSink>> = Some(pre_aggregator.clone());
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
    });

    crate::api::init_global_state(api_state.clone());
    tracing::info!("{}", t("main-global-state-initialized"));

    // G7：就绪探针注册（sdforge 全局表，装配期一次性；检查读既有句柄，
    // 不发起上游请求）。
    crate::api::readiness::register_readiness_checks(&api_state, &database.pool);

    Ok(Assembly {
        api_state,
        logger_handle,
        pre_aggregator,
        pa_shutdown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::async_logger::GateSink;
    use inklog::LogSink;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt;

    /// 装配运行时验证：console 替代组装（内置通道关闭 + 自建 gate sink
    /// 经 add_sink）必须能真实 build——通道拓扑错误只在构建期暴露，
    /// 编译期查不出。
    ///
    /// 追加断言 log 门面桥已生效：LogTracer::init 成功时把 log 全局
    /// max_level 提到 Trace（进程内唯一装点就是本模块的 init_logger）。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_init_logger_builds_masked_console_assembly() {
        let manager = init_logger().await.expect("init_logger 必须能构建");
        manager.shutdown().ok();
        // 接线锁：桥安装必须随装配被触及（与本测试执行顺序无关——旗标
        // 只由 init_logger 的 install_log_facade_bridge 设置；删掉该调用
        // 本断言即红，不受并发测试自装 LogTracer 影响）
        assert!(
            log_bridge_touched(),
            "init_logger 必须安装 log 门面桥（install_log_facade_bridge）"
        );
        // LogTracer::init 成功时把 log 全局 max_level 提到 Trace；进程内
        // 若并发测试先行安装亦为 Trace（同一装点族）——此为弱补强断言
        assert_eq!(
            log::max_level(),
            log::LevelFilter::Trace,
            "log 门面桥应把全局 max_level 提到 Trace"
        );
    }

    /// F1 桥端到端行为锁：log 门面记录经 LogTracer 转成 tracing 事件后，
    /// 必须以 GateSink 同款脱敏通路出站——含明文 sk- token 的 log 记录
    /// 到达捕获 sink 时 message 已被脱敏、target 保留。分层语义：桥本身
    /// 只转发（tracing 事件层是明文），脱敏发生在 GateSink——本测试把
    /// GateSink 挂在捕获 Layer 内，端到端锁「桥转发 + 出站脱敏」两环。
    /// 进程级单例说明：log 全局 logger 只能装一次，LogTracer::init 失败
    /// 意味着并发测试的 init_logger 已装桥——装上的同为 LogTracer，
    /// 断言不受影响。
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
                // tracing-log 转发布局（其 src/lib.rs）：事件 target 固定
                // "log"，原始 target 落在 "log.target" 字段
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
                // Layer 回调是同步上下文：GateSink.write 无真实异步 IO，
                // 独立 executor 驱动即可
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
        // tracing-log 转发特征：事件 callsite target 固定 "log"，原始
        // target 保留在 log.target 字段
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
