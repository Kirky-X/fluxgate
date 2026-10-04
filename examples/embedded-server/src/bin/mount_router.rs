// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 库形态嵌入：build_app 取网关 Router 与停机句柄，merge 宿主自有路由后
//! 自行 bind/serve。一行起服形态见 standalone.rs。

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use nexusflow::config::loader::ConfigLoader;
use nexusflow::server::build_app;
use serde_json::json;

// 运行目录（CWD）须有 config.toml，可从仓库根 config.example.toml 复制为起点；
// 监听地址取 [server] 段的 host/port。
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_manager = Arc::new(ConfigLoader::new(Some("config.toml".into())).load()?);

    // 进程级约束一：同进程 run / build_app 合计只能调用一次——装配经
    // init_global_state 注册 sdforge handler 全局态（OnceCell 一次性写入），
    // 二次调用即 panic（"Global state already initialized"）。
    let (app, handle) = build_app(Arc::clone(&config_manager)).await?;

    let app = app.merge(Router::new().route(
        "/examples-info",
        get(|| async {
            Json(json!({
                "example": "embedded-server/mount_router",
                "gateway": "nexusflow",
            }))
        }),
    ));

    let (host, port) = {
        let config = config_manager.read();
        (config.server.host.clone(), config.server.port)
    };
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let addr = listener.local_addr()?;

    println!("gateway + host routes listening on http://{addr} (GET /examples-info)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            if tokio::signal::ctrl_c().await.is_err() {
                eprintln!("Ctrl+C 监听失败：无法优雅停机，进程只能被外部信号终止");
            }
        })
        .await?;

    // 进程级约束二：退出前必须调用 shutdown()——句柄持有日志管道与预聚合器，
    // 不收尾则 flush worker 泄漏（每 60s tick 持有 Repository）且缓冲日志丢失。
    handle.shutdown().await;
    Ok(())
}
