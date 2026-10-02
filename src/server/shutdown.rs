// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 终止信号监听（原 main.rs:305-329 平移）。

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
