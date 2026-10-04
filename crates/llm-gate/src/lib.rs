// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! llm-gate — 纯路由核心库。
//!
//! 从 NexusFlow 单 crate workspace 化抽出的路由半边：IR 与协议转换、
//! Vendor 管理与韧性、路由决策、配置热读。零 axum 假设、零存储依赖
//! （dbnexus/sea-orm 不进编译图）；HTTP 服务面（api/middleware/server）、
//! SDK 与数据库落库留在 nexusflow 应用 crate，经本 crate 正向引用。
//!
//! 模块按 feature 门控：config/error/i18n/protocol/utils 常开，
//! vendor/router/resilience/cost-tracking 按 feature 编译（见 Cargo.toml）。

pub mod error;
pub mod i18n;
