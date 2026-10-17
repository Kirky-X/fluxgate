// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! gRPC 门面客户端入口：启动 tokio 运行时并转发到 grpc_client::run。

#[tokio::main]
async fn main() {
    fluxgate_examples::grpc_client::run().await
}
