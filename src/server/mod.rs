// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 服务器装配与运行时（库化承载）：嵌入式调用方可 [`build_app`] 拿 Router 与
//! [`ServerHandle`]（退出前必须 shutdown），或经 [`run`] 起服并收尾。
//! 子模块：`bootstrap` 装配链；`http` 站点配置与 serve；`grpc`（grpc feature）
//! 门面与认证；`handle` 停机句柄；`shutdown` 三阶段收尾编排。
//! 进程级约束：`init_global_state` 为 OnceCell 一次性写入，同进程二次调用即 panic。

pub use handle::ServerHandle;
pub use http::{build_app, run};

mod bootstrap;
mod handle;
mod hot_reload;
mod http;
mod shutdown;

#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "grpc")]
pub use grpc::GrpcServer;
