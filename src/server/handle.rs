// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 停机收尾句柄：flush 管道收尾暴露给嵌入式调用方（`build_app` 产物），
//! 收尾顺序与 `run` 共用同一实现。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::database::{AsyncLoggerHandle, TokenPreAggregator};
use crate::i18n::{t, t_args};
use crate::server::bootstrap::Assembly;
use crate::server::shutdown::InklogShutdownHook;

/// 停机收尾句柄（[`build_app`](crate::server::build_app) 与 [`run`](crate::server::run) 产物）：持有日志管道与预聚合器句柄，
/// 不持有则嵌入式退出时 flush worker 泄漏、缓冲数据丢失；grpc feature 下另持 `GrpcServer`。
pub struct ServerHandle {
    logger_handle: Arc<AsyncLoggerHandle>,
    pre_aggregator: Arc<TokenPreAggregator>,
    pa_shutdown: Arc<AtomicBool>,
    done: AtomicBool,
    /// inklog 关闭钩子（`run` 注入 manager；嵌入式未注入时收尾阶段 no-op）。
    inklog_hook: std::sync::OnceLock<Option<InklogShutdownHook>>,
    /// 热重载守护（装配期启动；随句柄存活，drop 即停止监听）。
    hot_reloader: Option<confers::watcher::HotReloader<crate::config::Config>>,
    /// 健康探针任务句柄（装配期按 [health].mode 启动；shutdown 首阶段 abort）。
    probe_handle: Option<tokio::task::JoinHandle<()>>,
    /// 配额账本停机最终 flush 句柄（装配期注入；None = 账本未装配，收尾跳过）。
    quota_ledger_teardown: crate::server::shutdown::QuotaLedgerTeardownHook,
    /// OTLP 桥（feature = "otlp" 且 [otlp].enabled 装配；shutdown 时 flush + 停 reader）。
    #[cfg(feature = "otlp")]
    otlp_bridge: Option<Arc<crate::utils::otlp::OtlpBridge>>,
    /// OTLP 桥轮询任务句柄（shutdown 首阶段 abort）。
    #[cfg(feature = "otlp")]
    otlp_task: Option<tokio::task::JoinHandle<()>>,
    /// OTLP 尝试级追踪控制器（shutdown 时 flush + 停 processor + uninstall）。
    #[cfg(feature = "otlp")]
    otlp_trace: Option<Arc<crate::utils::otlp_export::TraceController>>,
    /// 目录同步周期任务句柄（catalog-sync feature；shutdown 首阶段 abort）。
    #[cfg(feature = "catalog-sync")]
    catalog_sync_task: Option<tokio::task::JoinHandle<()>>,
    #[cfg(feature = "grpc")]
    grpc_server: std::sync::OnceLock<Option<crate::server::grpc::GrpcServer>>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("shutdown_done", &self.done.load(Ordering::Relaxed))
            .field("hot_reload_watcher", &self.hot_reloader.is_some())
            .finish()
    }
}

impl ServerHandle {
    pub(crate) fn from_assembly(assembly: Assembly) -> Self {
        Self {
            logger_handle: assembly.logger_handle.clone(),
            pre_aggregator: assembly.pre_aggregator.clone(),
            pa_shutdown: assembly.pa_shutdown.clone(),
            done: AtomicBool::new(false),
            inklog_hook: std::sync::OnceLock::new(),
            hot_reloader: assembly.hot_reloader,
            probe_handle: assembly.probe_handle,
            quota_ledger_teardown: assembly.quota_ledger_teardown,
            #[cfg(feature = "otlp")]
            otlp_bridge: assembly.otlp_bridge,
            #[cfg(feature = "otlp")]
            otlp_task: assembly.otlp_task,
            #[cfg(feature = "otlp")]
            otlp_trace: assembly.otlp_trace,
            #[cfg(feature = "catalog-sync")]
            catalog_sync_task: assembly.catalog_sync_task,
            #[cfg(feature = "grpc")]
            grpc_server: std::sync::OnceLock::new(),
        }
    }

    /// 登记 inklog 关闭钩子（装配期一次性；重复调用 no-op，未登记则收尾跳过）。
    pub(crate) fn set_inklog_shutdown_hook(&self, hook: InklogShutdownHook) {
        let _ = self.inklog_hook.set(Some(hook));
    }

    /// 登记 gRPC 服务器（装配期一次性；重复调用为 no-op）。
    #[cfg(feature = "grpc")]
    pub(crate) fn set_grpc_server(&self, grpc: Option<crate::server::grpc::GrpcServer>) {
        let _ = self.grpc_server.set(grpc);
    }

    /// 取出 gRPC 服务器（一次性）：`Some` = [grpc] 段启用且装配成功，调用方 spawn 启动；
    /// `None` = gRPC 禁用或已被取走（不取则随句柄丢弃）。
    #[cfg(feature = "grpc")]
    pub fn take_grpc_server(&mut self) -> Option<crate::server::grpc::GrpcServer> {
        self.grpc_server.take().flatten()
    }

    /// 优雅停机收尾：serve 排空后三阶段收尾（AsyncLogger 补刷 → pre-aggregator
    /// 停机位 + 最终 flush → inklog 关闭）；幂等，阶段超时只告警不中断后续阶段。
    pub async fn shutdown(&self) {
        if self.done.swap(true, Ordering::Relaxed) {
            return;
        }
        // 探针任务先停（abort 即停，不等待周期睡眠）。
        if let Some(handle) = &self.probe_handle {
            handle.abort();
        }
        // OTLP 桥轮询任务同停（flush 钩子负责最后一跳导出）。
        #[cfg(feature = "otlp")]
        if let Some(handle) = &self.otlp_task {
            handle.abort();
        }
        // 目录同步周期任务同停（admin 手动触发的单次同步不在此列——
        // in-flight 守卫保证完成后自然收尾）。
        #[cfg(feature = "catalog-sync")]
        if let Some(handle) = &self.catalog_sync_task {
            handle.abort();
        }
        // serve 返回即 in-flight 已完成，此刻补最终 flush 防缓冲日志与聚合数据丢失
        tracing::info!("{}", t("main-flushing-log-pipeline"));
        let otlp_teardown: crate::server::shutdown::OtlpTeardownHook = {
            #[cfg(feature = "otlp")]
            {
                let otlp_trace = self.otlp_trace.clone();
                self.otlp_bridge.clone().map(move |bridge| {
                    Arc::new(move || {
                        // flush 与 shutdown 各自独立：末次导出失败（端点不可达、
                        // 接收端 4xx）不代表 worker 线程该泄漏。调度顺序保留
                        // flush 在前，错误合并上报。
                        let flush_err = bridge.force_flush().err();
                        let shutdown_err = bridge.shutdown().err();
                        // 尝试级追踪同点收尾：flush 最后一跳 + 停 processor +
                        // uninstall（best-effort：无 span 丢失承诺，失败只告警）
                        let trace_flush_err =
                            otlp_trace.as_ref().and_then(|t| t.force_flush().err());
                        let trace_shutdown_err =
                            otlp_trace.as_ref().and_then(|t| t.shutdown().err());
                        crate::utils::otlp_export::uninstall();
                        if let Some(e) = flush_err
                            .or(shutdown_err)
                            .or(trace_flush_err)
                            .or(trace_shutdown_err)
                        {
                            tracing::warn!(
                                "{}",
                                t_args("main-otlp-teardown-failed", &[("error", e.to_string())])
                            );
                        }
                    }) as Arc<dyn Fn() + Send + Sync>
                })
            }
            #[cfg(not(feature = "otlp"))]
            {
                None
            }
        };
        let coordinator = crate::server::shutdown::build_teardown_coordinator(
            &self.logger_handle,
            &self.pre_aggregator,
            &self.pa_shutdown,
            self.inklog_hook.get().cloned().flatten(),
            otlp_teardown,
            self.quota_ledger_teardown.clone(),
        );
        match coordinator.shutdown().await {
            Ok(result) if result.is_ok() => {}
            Ok(result) => {
                tracing::warn!(
                    "{}",
                    t_args(
                        "main-teardown-timeout",
                        &[("phases", format!("{:?}", result.timed_out_phases()))]
                    )
                );
            }
            Err(e) => {
                tracing::warn!(
                    "{}",
                    t_args("main-teardown-error", &[("error", e.to_string())])
                );
            }
        }
        tracing::info!(
            "{}",
            t_args(
                "main-log-pipeline-flushed",
                &[("dropped", self.logger_handle.dropped_logs().to_string())]
            )
        );
    }
}
