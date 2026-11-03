// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 运维端点入口：启动 tokio 运行时并转发到 sdk_usage::admin::run。

#[tokio::main]
async fn main() -> Result<(), fluxweir::sdk::FluxweirSdkError> {
    fluxweir_examples::sdk_usage::admin::run().await
}
