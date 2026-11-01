// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 分层守卫：`src/router/` 不得引用 web 层模块，`router` feature 必须能脱离
//! `web-server` 单独编译。
//!
//! 背景：`router` 与 `web-server` 是两个独立 feature（`Cargo.toml:49` 让
//! `web-server` 依赖 `router`，而非反之）。`quota-aware-routing` 落地时
//! `src/router/router.rs` 直接写了 `crate::middleware::quota::*`，而
//! `middleware` 只在 `web-server` 下编译——`--features
//! "vendor-management,router,protocol,resilience"` 因此长期编译失败，CI 的
//! 单特性矩阵也没覆盖该组合，直到跨仓下游（agentstem 的 gate crate）按此
//! 组合构建才暴露。
//!
//! 本守卫是一条**快速本地绊线**，不是权威门禁：它是字面行扫描，多行 `use`、
//! 别名重导出（`use crate as c; c::middleware::…`）、`super::super::middleware`、
//! `#[path]`/`include!` 等形态均可绕过。**权威门禁是编译器**——`ci.yml` 的
//! 单特性组合 `--features "vendor-management,router,protocol,resilience"`
//! 一编译即暴露跨 feature 依赖，无法绕过。
//!
//! 绊线的价值是本地秒级反馈，把「路由核心（纯策略层）不得依赖 axum / tower /
//! `crate::middleware` / `crate::api` / `crate::server` / `crate::database`；
//! 跨层共享的数据与端口走根级契约缝（`crate::quota_contract`、`crate::storage`）」
//! 这条纪律变成可执行断言。
//!
//! 扫描边界：`src/router/`（含子模块）。`#[cfg(test)]` 内的引用同样禁止——
//! 测试也属于该 feature 的编译面。

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 路由核心禁止出现的跨层引用（前缀匹配）。
const FORBIDDEN: &[&str] = &[
    "crate::middleware",
    "crate::api::",
    "crate::server",
    "crate::database",
    "axum::",
    "tower::",
    "tower_http::",
    // 相对路径别名同样等于跨层引用（扫描无法解析，故按字面拦）
    "super::super::middleware",
];

#[test]
fn router_core_has_no_web_layer_dependency() {
    let dir = repo_root().join("src/router");
    let mut files = Vec::new();
    collect_rs_files(&dir, &mut files);
    assert!(
        !files.is_empty(),
        "扫描面为空说明路径失效（守卫会静默通过）：{dir:?}"
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for (lineno, line) in text.lines().enumerate() {
            let code = line.trim_start();
            // 注释行不构成真实依赖
            if code.starts_with("//") {
                continue;
            }
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    let rel = file.strip_prefix(repo_root()).unwrap_or(file);
                    violations.push(format!("{}:{}: {needle}", rel.display(), lineno + 1));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "router 层不得依赖 web 层（会破坏 `--features router` 单独构建）。\n\
         跨层共享的数据/端口请放根级契约缝（quota_contract / storage）。\n\
         违规点：{violations:#?}"
    );
}

/// 契约缝自身必须保持无 web/db 依赖——它是路由与 web 层的共享内核。
#[test]
fn quota_contract_stays_layer_neutral() {
    let path = repo_root().join("src/quota_contract.rs");
    let text = fs::read_to_string(&path).expect("契约缝模块必须存在");
    for needle in [
        "crate::middleware",
        "crate::api",
        "crate::server",
        "crate::database",
        "axum::",
    ] {
        assert!(
            !text
                .lines()
                .any(|l| !l.trim_start().starts_with("//") && l.contains(needle)),
            "契约缝不得依赖 {needle}：{path:?}"
        );
    }
}
