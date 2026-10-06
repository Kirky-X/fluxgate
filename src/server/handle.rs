// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 停机收尾句柄：把原二进制 main.rs 的 flush 管道收尾能力暴露给
//! 嵌入式调用方（build_app 产物），收尾顺序与 run() 共用同一实现。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::database::{AsyncLoggerHandle, TokenPreAggregator};
use crate::i18n::{t, t_args};
use crate::server::bootstrap::Assembly;
use crate::server::shutdown::InklogShutdownHook;

/// 服务器停机收尾句柄（[`build_app`](crate::server::build_app) 与
/// [`run`](crate::server::run) 产物之一）。
///
/// 持有日志管道与预聚合器的运行时句柄；不持有则嵌入式退出时
/// flush worker 泄漏（每 60s tick 持有 Repository）且缓冲数据丢失。
///
/// feature = "grpc" 时另持有装配好的 [`GrpcServer`]（`take_grpc_server`
/// 取出后由调用方 spawn；未取出则随句柄丢弃——装配 ≠ 启动）。
pub struct ServerHandle {
    logger_handle: Arc<AsyncLoggerHandle>,
    pre_aggregator: Arc<TokenPreAggregator>,
    pa_shutdown: Arc<AtomicBool>,
    done: AtomicBool,
    /// inklog 关闭钩子（`run()` 注入 manager；嵌入式未注入时收尾阶段 no-op）。
    inklog_hook: std::sync::OnceLock<Option<InklogShutdownHook>>,
    /// 热重载守护（装配期启动；随句柄存活，drop 即停止监听）。
    /// 监听是否装配经 [`Debug`] 暴露（`hot_reload_watcher` 字段），
    /// 嵌入式调用方据此查询热重载面状态。
    hot_reloader: Option<confers::watcher::HotReloader<crate::config::Config>>,
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
            #[cfg(feature = "grpc")]
            grpc_server: std::sync::OnceLock::new(),
        }
    }

    /// 登记 inklog 关闭钩子（`run()` 装配期一次性；重复调用为 no-op；
    /// 未登记则收尾时该阶段跳过）。
    pub(crate) fn set_inklog_shutdown_hook(&self, hook: InklogShutdownHook) {
        let _ = self.inklog_hook.set(Some(hook));
    }

    /// 登记 gRPC 服务器（装配期一次性；重复调用为 no-op）。
    #[cfg(feature = "grpc")]
    pub(crate) fn set_grpc_server(&self, grpc: Option<crate::server::grpc::GrpcServer>) {
        let _ = self.grpc_server.set(grpc);
    }

    /// 取出 gRPC 服务器（一次性）：`Some(server)` = [grpc] 段启用且装配
    /// 成功，调用方 `tokio::spawn(server.serve(signal))` 启动；`None` =
    /// gRPC 禁用或已被取走。不取则随句柄丢弃（装配 ≠ 启动）。
    #[cfg(feature = "grpc")]
    pub fn take_grpc_server(&mut self) -> Option<crate::server::grpc::GrpcServer> {
        self.grpc_server.take().flatten()
    }

    /// 优雅停机收尾（G9）：serve 排空返回后执行三阶段收尾——AsyncLogger
    /// 关停补刷 → pre-aggregator 停机位 + 最终 flush → inklog 关闭——经
    /// trait-kit `AsyncShutdownCoordinator` 显式编排（顺序见
    /// `crate::server::shutdown::build_teardown_coordinator`）。
    ///
    /// 幂等：重复调用为 no-op（首次调用后置位）。阶段超时只告警不中断
    /// 后续阶段（协调器语义），最后统一汇报 dropped 日志数。
    pub async fn shutdown(&self) {
        if self.done.swap(true, Ordering::Relaxed) {
            return;
        }
        // recId=9：优雅停机 flush — serve 返回即 in-flight 请求已全部完成，
        // 此刻补最终 flush 保证缓冲中的日志与聚合数据不丢
        tracing::info!("{}", t("main-flushing-log-pipeline"));
        let coordinator = crate::server::shutdown::build_teardown_coordinator(
            &self.logger_handle,
            &self.pre_aggregator,
            &self.pa_shutdown,
            self.inklog_hook.get().cloned().flatten(),
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
