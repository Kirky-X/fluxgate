// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 服务器装配与运行时（库化承载）。
//!
//! 原二进制 main.rs 的装配链下沉为库模块：嵌入式调用方可直接
//! [`build_app`] 拿到 Router 与 [`ServerHandle`]（停机 flush 收尾，
//! 嵌入式退出前必须调用），或经 [`run`] 完成起服与收尾（薄启动
//! main.rs 仅做参数解析后转调）。
//!
//! - [`bootstrap`]：logger → config → db → vendor → 日志管道 → DI kit →
//!   router/bandit/affinity/response_cache → ApiState → init_global_state
//! - [`http`]：SdForgeConfig 构建、sdforge 站点 + 业务路由合并、bind/serve、
//!   serve 结果映射与停机 flush 管道收尾
//! - [`grpc`]（feature = "grpc"）：[grpc] 段装配、sdforge grpc 门面 +
//!   [auth] 摘要 verifier、优雅停机 serve
//! - [`handle`]：停机收尾句柄（三阶段协调器编排 flush，幂等）
//! - [`shutdown`]：G9 停机编排（sdforge 三阶段信号/排空 + trait-kit
//!   组件收尾协调器）
//!
//! ## 进程级约束
//!
//! 装配经 `init_global_state` 注册 sdforge handler 全局态（OnceCell
//! 一次性写入）：同进程对 [`build_app`] / [`run`] 的**二次调用即 panic**
//! （"Global state already initialized"）——重复装配是调用方 bug。

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
