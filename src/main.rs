// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 薄启动入口 — 装配链已下沉 `fluxgate::server`（库化承载）。此处仅解析
//! config_path 参数，缺省 "config.toml" 的语义由 `server::run` 内部承担。

// 装配链 async 块在 mysql 组合下 trait 求解深度超默认 128 层（sqlx 类型层级更深）
#![recursion_limit = "256"]

#[tokio::main]
async fn main() -> Result<(), fluxgate::error::FluxgateError> {
    // 仅读 CLI 配置路径，不进 exec/网络面；扫描器 INFO 按误报行内豁免
    let config_path = std::env::args().nth(1); // nosemgrep
    fluxgate::server::run(config_path).await
}
