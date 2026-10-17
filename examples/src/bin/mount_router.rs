// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 挂载宿主路由入口：启动 tokio 运行时并转发到 embedded_server::mount_router::run。

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fluxgate_examples::embedded_server::mount_router::run().await
}
