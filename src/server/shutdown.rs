// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 优雅停机编排：信号与排空走 sdforge graceful 三阶段（触发 → 排空 → 收尾钩子），
//! 组件收尾序经 trait-kit `AsyncShutdownCoordinator` 显式化。收尾序（serve 排空后执行）：
//! 1. `StopRequests` — AsyncLogger 关停补刷；
//! 2. `DrainQueue` — pre-aggregator 停机位 + 最终 flush；
//! 3. `CloseConnections` — inklog manager 关闭。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::database::{AsyncLoggerHandle, TokenPreAggregator};
use crate::i18n::{t, t_args};

/// inklog 关闭钩子（`run()` 注入 manager；嵌入式未注入时该阶段 no-op）。
pub(crate) type InklogShutdownHook = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// 终止信号：sdforge 默认触发（SIGTERM/Ctrl-C，首个信号即决）。
pub(crate) async fn shutdown_signal() {
    sdforge::http::default_shutdown_signal().await;
}

/// 单钩子执行上限（秒）：trait-kit 阶段超时是钩子间软限制，单个挂起的钩子不会被
/// 阶段超时打断，故每个钩子体再包一层超时，超时记 warn 并继续。
const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 构建三阶段收尾协调器（调用方 `shutdown().await` 执行；幂等由 `ServerHandle` 的 done 位保证）。
pub(crate) fn build_teardown_coordinator(
    logger_handle: &Arc<AsyncLoggerHandle>,
    pre_aggregator: &Arc<TokenPreAggregator>,
    pa_shutdown: &Arc<AtomicBool>,
    inklog_hook: Option<InklogShutdownHook>,
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
        coordinator
            .register_hook(ShutdownPhase::DrainQueue, move || {
                let pre_aggregator = pre_aggregator.clone();
                let pa_shutdown = pa_shutdown.clone();
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

        let coord = build_teardown_coordinator(&logger, &aggregator, &pa_shutdown, Some(hook));
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
}
