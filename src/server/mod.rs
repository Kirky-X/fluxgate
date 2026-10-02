// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 服务器装配与运行时（库化承载）。
//!
//! 原二进制 main.rs 的装配链下沉为库模块：嵌入式调用方可直接
//! [`build_app`] 拿到 Router（供测试/内嵌），或经 [`run`] 完成起服与
//! 优雅停机 flush 收尾（薄启动 main.rs 仅做参数解析后转调）。
//!
//! - [`bootstrap`]：logger → config → db → vendor → 日志管道 → DI kit →
//!   router/bandit/affinity/response_cache → ApiState → init_global_state
//! - [`http`]：SdForgeConfig 构建、sdforge 站点 + 业务路由合并、bind/serve、
//!   serve 结果映射与停机 flush 管道收尾
//! - [`shutdown`]：终止信号监听（SIGTERM/Ctrl+C）

pub use http::{build_app, run};

mod bootstrap;
mod http;
mod shutdown;
