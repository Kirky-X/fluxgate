// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 裸 HTTP 直连示例入口：启动 tokio 运行时并转发到 rest_http::run。

#[tokio::main]
async fn main() -> std::process::ExitCode {
    fluxgate_examples::rest_http::run().await
}
