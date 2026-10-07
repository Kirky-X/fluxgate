// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 运维端点入口：启动 tokio 运行时并转发到 sdk_usage::admin::run。

#[tokio::main]
async fn main() -> Result<(), nexusflow::sdk::NexusFlowSdkError> {
    nexusflow_examples::sdk_usage::admin::run().await
}
