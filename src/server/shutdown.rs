// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 优雅停机编排：信号与排空走 sdforge graceful 三阶段（触发 → 排空 → 收尾钩子），
//! 组件收尾序经 trait-kit `AsyncShutdownCoordinator` 显式化。收尾序（serve 排空后执行）：
//! 1. `StopRequests` — AsyncLogger 关停补刷；
//! 2. `DrainQueue` — pre-aggregator 停机位 + 最终 flush；配额账本快照直落 + 落盘器关停；
//! 3. `CloseConnections` — inklog manager 关闭。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::database::{AsyncLoggerHandle, TokenPreAggregator};
use crate::i18n::{t, t_args};
use crate::middleware::quota::QuotaState;
use crate::middleware::quota_persist::QuotaLedgerPersister;

/// inklog 关闭钩子（`run` 注入 manager；嵌入式未注入时该阶段 no-op）。
pub(crate) type InklogShutdownHook = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// OTLP 停机收尾钩子（feature = "otlp" 且 [otlp].enabled 时装配；None = no-op）。
/// force_flush + provider shutdown 为同步阻塞，钩子体内 spawn_blocking 执行。
pub(crate) type OtlpTeardownHook = Option<Arc<dyn Fn() + Send + Sync>>;

/// 配额账本停机收尾句柄（quota-aware-routing）：快照生产方 + 落盘器。
/// 在位时 DrainQueue 阶段执行「内存快照 → 直落 UPSERT → persister 关停」，
/// 干净重启不再吃 ≤30s 崩溃回退窗口；None = 账本未装配（该步 no-op）。
pub(crate) type QuotaLedgerTeardownHook = Option<(Arc<QuotaState>, Arc<QuotaLedgerPersister>)>;

/// 终止信号：sdforge 默认触发（SIGTERM/Ctrl-C，首个信号即决）。
pub(crate) async fn shutdown_signal() {
    sdforge::http::default_shutdown_signal().await;
}

/// 单钩子执行上限（秒）：trait-kit 阶段超时是钩子间软限制，单个挂起的钩子不会被
/// 阶段超时打断，故每个钩子体再包一层超时，超时记 warn 并继续。
const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 构建三阶段收尾协调器（调用方 `shutdown.await` 执行；幂等由 `ServerHandle` 的 done 位保证）。
pub(crate) fn build_teardown_coordinator(
    logger_handle: &Arc<AsyncLoggerHandle>,
    pre_aggregator: &Arc<TokenPreAggregator>,
    pa_shutdown: &Arc<AtomicBool>,
    inklog_hook: Option<InklogShutdownHook>,
    otlp_teardown: OtlpTeardownHook,
    quota_ledger: QuotaLedgerTeardownHook,
) -> trait_kit::kit::shutdown::AsyncShutdownCoordinator {
    use trait_kit::kit::shutdown::{AsyncShutdownCoordinator, ShutdownPhase};

    let coordinator = AsyncShutdownCoordinator::new();

    {
        let logger_handle = logger_handle.clone();
        coordinator
            .register_hook(ShutdownPhase::StopRequests, move || {
                let logger_handle = logger_handle.clone();
                Box::pin(async move {
                    match tokio::time::timeout(HOOK_TIMEOUT, logger_handle.shutdown()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(
                                "{}",
                                t_args("main-async-logger-shutdown", &[("error", e.to_string())])
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                "{}",
                                t_args(
                                    "main-teardown-hook-timeout",
                                    &[("phase", "StopRequests".to_string())]
                                )
                            );
                        }
                    }
                })
            })
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "{}",
                    t_args(
                        "main-teardown-hook-registration-failed",
                        &[("error", e.to_string())]
                    )
                );
            });
    }

    {
        let pre_aggregator = pre_aggregator.clone();
        let pa_shutdown = pa_shutdown.clone();
        let otlp_teardown = otlp_teardown.clone();
        let quota_ledger = quota_ledger.clone();
        coordinator
            .register_hook(ShutdownPhase::DrainQueue, move || {
                let pre_aggregator = pre_aggregator.clone();
                let pa_shutdown = pa_shutdown.clone();
                let otlp_teardown = otlp_teardown.clone();
                let quota_ledger = quota_ledger.clone();
                Box::pin(async move {
                    pa_shutdown.store(true, Ordering::Relaxed);
                    match tokio::time::timeout(HOOK_TIMEOUT, pre_aggregator.flush()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(
                                "{}",
                                t_args("main-pre-aggregator-flush", &[("error", e.to_string())])
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                "{}",
                                t_args(
                                    "main-teardown-hook-timeout",
                                    &[("phase", "DrainQueue".to_string())]
                                )
                            );
                        }
                    }
                    if let Some(otlp) = otlp_teardown {
                        let flush = tokio::task::spawn_blocking(move || otlp());
                        if tokio::time::timeout(HOOK_TIMEOUT, flush).await.is_err() {
                            tracing::warn!(
                                "{}",
                                t_args(
                                    "main-teardown-hook-timeout",
                                    &[("phase", "OtlpFlush".to_string())]
                                )
                            );
                        }
                    }
                    // 配额账本最终 flush（quota-aware-routing）：内存快照直落 +
                    // 落盘器关停——30s ticker 随 closed 位退出，干净重启不再吃
                    // ≤30s 回退窗口（UPSERT 幂等，与周期 flush 重叠无害）。
                    if let Some((quota_state, persister)) = quota_ledger {
                        let final_flush = async {
                            let rows = quota_state.ledger_snapshot();
                            if let Err(e) = persister.flush_now(&rows).await {
                                tracing::warn!(
                                    "{}",
                                    t_args(
                                        "main-quota-ledger-final-flush",
                                        &[("error", e.to_string())]
                                    )
                                );
                            }
                            if let Err(e) = persister.shutdown().await {
                                tracing::warn!(
                                    "{}",
                                    t_args(
                                        "main-quota-ledger-shutdown",
                                        &[("error", e.to_string())]
                                    )
                                );
                            }
                        };
                        if tokio::time::timeout(HOOK_TIMEOUT, final_flush)
                            .await
                            .is_err()
                        {
                            tracing::warn!(
                                "{}",
                                t_args(
                                    "main-teardown-hook-timeout",
                                    &[("phase", "QuotaLedgerFlush".to_string())]
                                )
                            );
                        }
                    }
                })
            })
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "{}",
                    t_args(
                        "main-teardown-hook-registration-failed",
                        &[("error", e.to_string())]
                    )
                );
            });
    }

    {
        coordinator
            .register_hook(ShutdownPhase::CloseConnections, move || {
                Box::pin(async move {
                    let run = async {
                        match inklog_hook {
                            Some(ref hook) => {
                                if let Err(e) = hook() {
                                    tracing::warn!(
                                        "{}",
                                        t_args("main-inklog-shutdown", &[("error", e.to_string())])
                                    );
                                }
                            }
                            None => {
                                tracing::debug!("{}", t("main-inklog-shutdown-skipped"));
                            }
                        }
                    };
                    if tokio::time::timeout(HOOK_TIMEOUT, run).await.is_err() {
                        tracing::warn!(
                            "{}",
                            t_args(
                                "main-teardown-hook-timeout",
                                &[("phase", "CloseConnections".to_string())]
                            )
                        );
                    }
                })
            })
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "{}",
                    t_args(
                        "main-teardown-hook-registration-failed",
                        &[("error", e.to_string())]
                    )
                );
            });
    }

    coordinator
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Repository;
    use dbnexus::{DbConfig, DbPool};

    async fn memory_repository() -> Repository {
        let pool = DbPool::with_config(DbConfig {
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
        let repo = Repository::new(pool);
        repo.initialize().await.expect("schema");
        repo
    }

    /// 真实组件 wiring：三阶段全部执行——logger 关停、聚合落库、inklog 钩子触发、停机位置位。
    #[tokio::test]
    async fn teardown_runs_all_components_in_order() {
        let repo = memory_repository().await;
        let logger = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
            std::sync::Arc::new(repo.clone()),
            100,
            std::time::Duration::from_secs(60),
        )));
        let aggregator = Arc::new(TokenPreAggregator::new(
            std::sync::Arc::new(repo.clone()),
            std::time::Duration::from_secs(60),
        ));
        let pa_shutdown = Arc::new(AtomicBool::new(false));
        let inklog_ran = Arc::new(AtomicBool::new(false));
        let inklog_ran_hook = inklog_ran.clone();
        let hook: InklogShutdownHook = Arc::new(move || {
            inklog_ran_hook.store(true, Ordering::Relaxed);
            Ok(())
        });

        // 预聚合一条用量：DrainQueue 落定后该行必须在库里
        aggregator
            .record(
                "t010-vendor",
                "t010-model",
                0.001,
                &crate::protocol::TokenUsage::default(),
            )
            .await;

        let coord =
            build_teardown_coordinator(&logger, &aggregator, &pa_shutdown, Some(hook), None, None);
        let result = coord.shutdown().await.expect("shutdown");
        assert!(result.is_ok(), "三阶段应全部按时完成");

        assert!(
            pa_shutdown.load(Ordering::Relaxed),
            "DrainQueue 必须置位 pre-aggregator 停机位"
        );
        assert!(
            inklog_ran.load(Ordering::Relaxed),
            "CloseConnections 必须触发 inklog 钩子"
        );
        let stats = repo
            .get_token_stats(Some("t010-vendor"))
            .await
            .expect("stats");
        assert!(
            stats
                .iter()
                .any(|r| r.model == "t010-model" && r.request_count >= 1),
            "聚合必须落库，got {stats:?}"
        );
    }

    /// 超时强退：drain 超时后挂起请求被强制中止，serve 在超时量级内返回。
    #[tokio::test]
    async fn serve_drain_timeout_force_aborts_hanging_requests() {
        use std::time::{Duration, Instant};
        use tokio::net::TcpListener;

        let app = axum::Router::new().route(
            "/hang",
            axum::routing::get(|| async {
                std::future::pending::<axum::response::Response>().await
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let serve = tokio::spawn(sdforge::http::serve_with_graceful_shutdown(
            app,
            listener,
            async move {
                let _ = rx.await;
            },
            sdforge::http::GracefulShutdownConfig::with_drain_timeout(Duration::from_millis(200)),
        ));

        // 制造一个排空中挂起的请求
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        use tokio::io::AsyncWriteExt as _;
        stream
            .write_all(b"GET /hang HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        tokio::time::sleep(Duration::from_millis(150)).await;

        let started = Instant::now();
        if tx.send(()).is_err() {
            // 接收端已走则 serve 已结束，无需触发
        }
        let serve_result = tokio::time::timeout(Duration::from_secs(10), serve)
            .await
            .expect("serve 必须在超时量级内返回而非永久挂起")
            .expect("serve 任务无 panic");
        assert!(
            serve_result.is_ok(),
            "排空超时后 serve 必须干净返回: {serve_result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "drain 超时必须强制中止挂起请求"
        );
    }

    /// 最小配置（[quota] 缺省编译为 id="default" 的 rolling 单窗）：
    /// 给 QuotaState::restore_from_ledger 提供可恢复行（window_id 对齐、未过期）。
    fn quota_config() -> crate::config::Config {
        let toml_v = "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n[database]\npath = \":memory:\"\n";
        crate::config::parse_config_content(std::path::Path::new("memory.toml"), toml_v)
            .expect("样例配置解析")
    }

    /// 点亮并求值 tracing 事件（registry + 全字段格式化层）：无订阅者时
    /// warn/error 宏整体跳过；on_event 内 format 全部字段迫使 message 的
    /// 惰性格式化闭包（i18n 文案）执行——收尾告警分支的文案行进入覆盖。
    /// 全局安装失败（进程内他测已装）无妨，callsite 已点亮即达目的。
    fn install_evaluating_subscriber() {
        struct FormatAllLayer;
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FormatAllLayer {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                struct FormatAll;
                impl tracing::field::Visit for FormatAll {
                    fn record_debug(
                        &mut self,
                        _field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        let _ = format!("{value:?}");
                    }
                }
                let mut visitor = FormatAll;
                event.record(&mut visitor);
            }
        }

        use tracing_subscriber::layer::SubscriberExt as _;
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(FormatAllLayer),
        );
    }

    /// StopRequests 阶段错误分支：logger 已提前 shutdown（幂等面：二次调用
    /// 返回 Err channel closed）→ 收尾钩子内 Ok(Err(e)) 走 warn 告警，
    /// 阶段不置败、后续阶段照常执行。
    #[tokio::test]
    async fn teardown_warns_when_async_logger_already_shutdown() {
        install_evaluating_subscriber();
        let repo = memory_repository().await;
        let logger = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
            std::sync::Arc::new(repo.clone()),
            100,
            std::time::Duration::from_secs(60),
        )));
        let aggregator = Arc::new(TokenPreAggregator::new(
            std::sync::Arc::new(repo.clone()),
            std::time::Duration::from_secs(60),
        ));
        let pa_shutdown = Arc::new(AtomicBool::new(false));

        logger.shutdown().await.expect("首次 shutdown 必须成功");

        let coord =
            build_teardown_coordinator(&logger, &aggregator, &pa_shutdown, None, None, None);
        let result = coord.shutdown().await.expect("shutdown");
        assert!(
            result.is_ok(),
            "logger 二次 shutdown 只告警不置败阶段: {result:?}"
        );
        assert!(
            pa_shutdown.load(Ordering::Relaxed),
            "StopRequests 告警后 DrainQueue 仍须执行（停机位置位）"
        );
    }

    /// DrainQueue 可选组件在位分支：OTLP 钩子执行 + 配额账本最终 flush——
    /// persister 预先关停一次，钩子内二次 shutdown 走 Err 告警分支（幂等面）；
    /// CloseConnections 钩子返回 Err 走 warn 告警分支，阶段不置败。
    #[tokio::test]
    async fn teardown_runs_otlp_hook_quota_final_flush_and_inklog_hook_error() {
        install_evaluating_subscriber();
        let repo = memory_repository().await;
        let logger = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
            std::sync::Arc::new(repo.clone()),
            100,
            std::time::Duration::from_secs(60),
        )));
        let aggregator = Arc::new(TokenPreAggregator::new(
            std::sync::Arc::new(repo.clone()),
            std::time::Duration::from_secs(60),
        ));
        let pa_shutdown = Arc::new(AtomicBool::new(false));

        let otlp_ran = Arc::new(AtomicBool::new(false));
        let otlp_flag = otlp_ran.clone();
        let otlp_teardown: OtlpTeardownHook = Some(Arc::new(move || {
            otlp_flag.store(true, Ordering::Relaxed);
        }));

        let quota_state = Arc::new(QuotaState::new());
        let config = quota_config();
        let now = chrono::Utc::now().timestamp();
        let restored = quota_state.restore_from_ledger(
            &[crate::database::QuotaLedgerRow {
                api_key_hash: "t-shutdown-tenant".to_string(),
                window_id: "default".to_string(),
                window_start: now - 60,
                tokens_used: 100,
                cost_used_millicents: 5,
                updated_at: now,
            }],
            &config,
        );
        assert_eq!(restored, 1, "预置行必须可恢复（快照非空的前提）");

        let persister = Arc::new(QuotaLedgerPersister::new(
            std::sync::Arc::new(repo.clone()),
            4,
            None,
        ));
        persister.shutdown().await.expect("首次 persister 关停");

        let inklog_ran = Arc::new(AtomicBool::new(false));
        let inklog_flag = inklog_ran.clone();
        let hook: InklogShutdownHook = Arc::new(move || {
            inklog_flag.store(true, Ordering::Relaxed);
            Err("inklog teardown failed".to_string())
        });

        let coord = build_teardown_coordinator(
            &logger,
            &aggregator,
            &pa_shutdown,
            Some(hook),
            otlp_teardown,
            Some((quota_state, persister)),
        );
        let result = coord.shutdown().await.expect("shutdown");
        assert!(result.is_ok(), "各钩子内部告警不置败阶段: {result:?}");
        assert!(otlp_ran.load(Ordering::Relaxed), "OTLP 收尾钩子必须执行");
        assert!(
            inklog_ran.load(Ordering::Relaxed),
            "inklog 钩子返回 Err 仍须先执行"
        );
        assert!(pa_shutdown.load(Ordering::Relaxed));
    }

    /// DrainQueue 落库失败分支：占满唯一连接（池 max=1 且 acquire_timeout
    /// 收紧到 200ms）后，pre-aggregator 最终 flush（缓冲非空）与配额账本
    /// flush_now（快照非空、persister 未预关停）双双在 get_session 获取
    /// 超时处走 Err 告警分支——落库失败只告警，阶段不置败、停机继续。
    #[tokio::test]
    async fn teardown_warns_when_final_flushes_time_out_on_exhausted_pool() {
        install_evaluating_subscriber();
        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                acquire_timeout: 200,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("memory pool");
        let repo = Repository::new(pool);
        repo.initialize().await.expect("schema");

        let logger = Arc::new(AsyncLoggerHandle::new(crate::database::AsyncLogger::new(
            std::sync::Arc::new(repo.clone()),
            100,
            std::time::Duration::from_secs(60),
        )));
        let aggregator = Arc::new(TokenPreAggregator::new(
            std::sync::Arc::new(repo.clone()),
            std::time::Duration::from_secs(60),
        ));
        // 非空缓冲：DrainQueue flush 必须真正走到落库语句才可能失败
        aggregator
            .record(
                "t-vendor",
                "t-model",
                0.001,
                &crate::protocol::TokenUsage::default(),
            )
            .await;

        let quota_state = Arc::new(QuotaState::new());
        let config = quota_config();
        let now = chrono::Utc::now().timestamp();
        let restored = quota_state.restore_from_ledger(
            &[crate::database::QuotaLedgerRow {
                api_key_hash: "t-shutdown-tenant".to_string(),
                window_id: "default".to_string(),
                window_start: now - 60,
                tokens_used: 100,
                cost_used_millicents: 5,
                updated_at: now,
            }],
            &config,
        );
        assert_eq!(restored, 1, "预置行必须可恢复（快照非空的前提）");
        let persister = Arc::new(QuotaLedgerPersister::new(
            std::sync::Arc::new(repo.clone()),
            4,
            None,
        ));

        let pa_shutdown = Arc::new(AtomicBool::new(false));
        // 占住唯一连接直到停机结束：后续 get_session 全部撞 acquire 超时
        let held = repo.pool.get_session("admin").await.expect("hold session");

        let coord = build_teardown_coordinator(
            &logger,
            &aggregator,
            &pa_shutdown,
            None,
            None,
            Some((quota_state, persister)),
        );
        let result = coord.shutdown().await.expect("shutdown");
        assert!(result.is_ok(), "落库失败只告警不置败停机: {result:?}");
        assert!(pa_shutdown.load(Ordering::Relaxed));
        drop(held);
    }
}
