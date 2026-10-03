// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 薄启动入口 — 装配链已下沉 `nexusflow::server`（库化承载，可独立
//! 部署或嵌入式调用）。此处仅保留 config_path 参数解析，缺省
//! "config.toml" 的语义由 `server::run` 内部承担。

#[tokio::main]
async fn main() -> Result<(), nexusflow::error::NexusFlowError> {
    let config_path = std::env::args().nth(1);
    nexusflow::server::run(config_path).await
}
