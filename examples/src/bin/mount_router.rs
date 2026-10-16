// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 挂载宿主路由入口：启动 tokio 运行时并转发到 embedded_server::mount_router::run。

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fluxgate_examples::embedded_server::mount_router::run().await
}
