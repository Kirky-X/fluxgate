// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 一行起服入口：启动 tokio 运行时并转发到 embedded_server::standalone::run。

#[tokio::main]
async fn main() -> Result<(), nexusflow::NexusFlowError> {
    nexusflow_examples::embedded_server::standalone::run().await
}
