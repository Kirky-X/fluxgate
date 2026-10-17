// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 一行起服形态：装配、起服、信号停机与 flush 收尾全部由 run 承担，
//! 阻塞至停机完成。需要挂载宿主自有路由时见 mount_router。

use fluxgate::server;

/// 运行目录（CWD）须有 config.toml，可从仓库根 config.example.toml 复制为起点；
/// 配置缺失或非法时 run 返回 Err，进程以非零码退出，无默认配置回退。
pub async fn run() -> Result<(), fluxgate::FluxgateError> {
    server::run(Some("config.toml".into())).await
}
