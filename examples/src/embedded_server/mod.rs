// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 库形态嵌入宿主应用的两种形态：一行起服（standalone）与挂载宿主路由
//! （mount_router）。各子模块暴露 `run`，由 `src/bin` 下的同名入口启动
//! tokio 运行时后转发进来。

pub mod mount_router;
pub mod standalone;
