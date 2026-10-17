// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流式 chat 入口：启动 tokio 运行时并转发到 sdk_usage::stream_chat::run。

#[tokio::main]
async fn main() -> Result<(), fluxgate::sdk::FluxgateSdkError> {
    fluxgate_examples::sdk_usage::stream_chat::run().await
}
