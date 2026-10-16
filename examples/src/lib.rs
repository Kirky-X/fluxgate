// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Fluxgate 示例集单 crate：把裸 HTTP 直连、Rust SDK 客户端、库形态嵌入、
//! gRPC 门面四种接入形态收拢到一个 crate，按功能划分为模块隔离示例逻辑，
//! `src/bin/` 下的极薄入口负责启动 tokio 运行时并转发到对应模块的 `run`。
//!
//! 每个模块由各自 feature 门控（`rest` / `sdk` / `embedded` / `grpc`），使
//! 依赖足迹与拆分前的四个独立 crate 等价：例如 SDK 客户端在
//! `--no-default-features --features sdk` 下不会编译网关服务器本体，gRPC
//! 代码生成（需 protoc）只在启用 `grpc` feature 时触发。

/// 裸 HTTP 直连网关：不经 SDK，直接对线级协议编程。
#[cfg(feature = "rest")]
pub mod rest_http;

/// Rust SDK 客户端调用已部署网关：chat / stream_chat / embed / admin 四形态。
#[cfg(feature = "sdk")]
pub mod sdk_usage;

/// 库形态嵌入宿主应用：standalone 一行起服、mount_router 挂载宿主路由。
#[cfg(feature = "embedded")]
pub mod embedded_server;

/// tonic gRPC 客户端：调用网关的 sdforge.v1.SdForgeService 门面。
#[cfg(feature = "grpc")]
pub mod grpc_client;
