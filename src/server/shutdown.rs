// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 终止信号监听（原 main.rs:305-329 平移）+ 多服务器共享停机广播。

use crate::i18n::t;

/// 监听终止信号触发优雅停机：先收到 SIGTERM/Ctrl+C 后 axum 会停止接受新连接，
/// 等待 in-flight 请求完成再退出。
pub(crate) async fn shutdown_signal() {
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

/// 共享停机信号：Ctrl+C/SIGTERM 一次触发，HTTP 与 gRPC 两路 graceful
/// shutdown 各持一个 [`ShutdownRx`] 消费（axum `with_graceful_shutdown`
/// 与 sdforge `serve_with_shutdown` 的 signal future 同源）。
#[derive(Clone)]
pub(crate) struct ShutdownRx(tokio::sync::watch::Receiver<bool>);

impl ShutdownRx {
    /// 等待停机信号触发（sender 侧 drop 视为触发，防御消费方永挂）。
    /// 以 `async move { rx.fired().await }` 形态作为 signal future 传入
    /// axum / sdforge 的 graceful shutdown 入口。
    pub(crate) async fn fired(&mut self) {
        if *self.0.borrow_and_update() {
            return;
        }
        let _ = self.0.changed().await;
    }
}

/// 启动停机广播任务：[`shutdown_signal`] 触发后向 watch channel 发送
/// true，返回共享接收端（HTTP/grPC 各 clone）。
pub(crate) fn spawn_shutdown_broadcaster() -> ShutdownRx {
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = tx.send(true);
    });
    ShutdownRx(rx)
}
