// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Rust SDK 客户端调用已部署网关：chat / stream_chat / embed / admin 四形态。
//! 每个子模块暴露 `run`，由 `src/bin` 下的同名入口启动 tokio 运行时后转发进来。

pub mod admin;
pub mod chat;
pub mod embed;
pub mod stream_chat;

/// 读取必填环境变量：缺失或为空即显性退出，四个示例共用（不带空值发请求）。
pub(crate) fn require_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        Ok(_) => {
            eprintln!("环境变量 {name} 为空，拒绝发出无效请求");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("缺少环境变量 {name}（{err}），先执行 export {name}=<值>");
            std::process::exit(1);
        }
    }
}
