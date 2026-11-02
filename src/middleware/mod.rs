// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

pub mod admission;
pub mod auth;
pub mod key_budget;
pub mod lexcompress;
// OIDC 换短时网关键（oidc feature 编译门，不进 full）：JWT 验签 + 进程内
// TTL 短时键；无 [auth.oidc_rules] 规则时认证路径不进入本模块。
#[cfg(feature = "oidc")]
pub mod oidc;
pub mod quota;
pub mod quota_period;
pub mod quota_persist;
pub use auth::{auth_middleware, ApiAuthState};
