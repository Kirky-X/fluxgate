// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 内置控制台前端自动构建：检测 node/npm → 按需构建 web/console → 产出
//! dist/ 供 rust-embed 嵌入（feature `console`）。
//!
//! 容错契约（proposal 承诺）：默认 `cargo build` 不得因缺 node 失败——
//! 工具链缺失/构建失败均降级为 `cargo:warning` 后跳过，仅当 dist 已存在
//! 时嵌入产物仍然可用。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn npm_path() -> Option<PathBuf> {
    // PATH 直查 + 常见 Homebrew 位置兜底（macOS GUI 启动 cargo 时 PATH 常不含 /opt/homebrew/bin）
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("npm");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = Path::new(dir).join("npm");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// path（文件或目录子树）中任一文件 mtime 晚于 cutoff。
fn tree_newer_than(path: &Path, cutoff: SystemTime) -> bool {
    if path.is_dir() {
        std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| tree_newer_than(&e.path(), cutoff))
            })
            .unwrap_or(false)
    } else {
        mtime(path).map(|t| t > cutoff).unwrap_or(false)
    }
}

fn dist_is_current(console_dir: &Path) -> bool {
    let dist = console_dir.join("dist/index.html");
    let Ok(dist_meta) = std::fs::metadata(&dist) else {
        return false;
    };
    let Ok(dist_mtime) = dist_meta.modified() else {
        return false;
    };
    // 任一输入比 dist 新则视为过期，需要重建。
    !inputs_newer_than(console_dir, dist_mtime)
}

/// src 子树或任一顶层输入文件比 cutoff 新。顶层文件动态枚举，覆盖
/// vite/tailwind/postcss/tsconfig/package*.json 等现在与将来的构建输入，
/// 不靠硬编码清单（漏一项就嵌陈旧 dist）；*.tsbuildinfo 是 tsc 产物非
/// 输入，排除。
fn inputs_newer_than(console_dir: &Path, cutoff: SystemTime) -> bool {
    if tree_newer_than(&console_dir.join("src"), cutoff) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(console_dir) else {
        return true; // 枚举失败按过期处理：宁可多建，不嵌陈旧产物
    };
    entries.filter_map(|e| e.ok()).any(|e| {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        path.is_file()
            && !name.ends_with(".tsbuildinfo")
            && mtime(&path).is_some_and(|t| t > cutoff)
    })
}

fn main() {
    // 嵌入面只在 console feature 下需要；其他 feature 组合完全跳过探测
    let console_enabled = std::env::var("CARGO_FEATURE_CONSOLE").is_ok();
    if !console_enabled {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let console_dir = manifest_dir.join("web/console");

    if !console_dir.is_dir() {
        println!("cargo:warning=fluxgate console: web/console 缺失，跳过前端构建（/console 路由将不可用）");
        return;
    }

    if dist_is_current(&console_dir) {
        // dist 已存在且较新：直接复用，不打 npm
        return;
    }

    let Some(npm) = npm_path() else {
        println!(
            "cargo:warning=fluxgate console: 未检测到 npm，跳过前端构建；预构建产物缺位时 /console 不可用"
        );
        return;
    };

    // 锁文件是锁定解析的前提：缺锁直接跳过并告警，不回落自由解析。
    // 有锁时 CI 走 npm ci（全新环境、严格按锁），本地增量走 npm install
    // （lock 满足时为秒级 no-op，避免改一行前端就删 node_modules 全量重装）。
    if !console_dir.join("package-lock.json").is_file() {
        println!(
            "cargo:warning=fluxgate console: package-lock.json 缺失，跳过前端构建（恢复锁文件后重建）"
        );
        return;
    }
    let install_subcommand = if std::env::var("CI").as_deref() == Ok("true") {
        "ci"
    } else {
        "install"
    };
    let status = std::process::Command::new(&npm)
        .arg(install_subcommand)
        .arg("--prefix")
        .arg(&console_dir)
        .arg("--no-audit")
        .arg("--no-fund")
        .status();
    match status {
        Ok(s) if s.success() => {}
        other => {
            println!(
                "cargo:warning=fluxgate console: npm {} 失败（{:?}），跳过前端构建；已有 dist 仍会嵌入",
                install_subcommand, other
            );
            return;
        }
    }

    // npm run build（package.json 内为 tsc -b && vite build）
    let status = std::process::Command::new(&npm)
        .arg("run")
        .arg("build")
        .arg("--prefix")
        .arg(&console_dir)
        .status();
    // 构建失败时不输出 rerun-if-changed：保持 cargo 默认「任意变更即重跑」，
    // 否则收窄后的监视集会让重试永远不触发。成功后监视集与 dist_is_current
    // 的输入集保持同一动态枚举（硬编码清单漏项 = 只改配置不重跑 build.rs）。
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!("cargo:warning=fluxgate console: npm run build 失败（{s}），跳过嵌入更新");
            return;
        }
        Err(e) => {
            println!("cargo:warning=fluxgate console: npm run build 启动失败（{e}），跳过嵌入更新");
            return;
        }
    }
    if let Ok(entries) = std::fs::read_dir(&console_dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_file() && !name.ends_with(".tsbuildinfo") {
                println!("cargo:rerun-if-changed=web/console/{name}");
            }
        }
    }
    println!("cargo:rerun-if-changed=web/console/src");
}
