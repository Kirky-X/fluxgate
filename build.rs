// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 内置控制台前端自动构建：检测 node/npm → 按需构建 web/console → 产出
//! dist/ 供 rust-embed 嵌入（feature `console`）。
//!
//! 容错契约（proposal 承诺）：默认 `cargo build` 不得因缺 node 失败——
//! 工具链缺失/构建失败均降级为 `cargo:warning` 后跳过，仅当 dist 已存在
//! 时嵌入产物仍然可用。
//!
//! 新鲜度采用内容戳而非 mtime：输入快照哈希写入 web/console/.fluxgate-build-stamp，
//! 构建前重算比对——免疫 cp -p 保留时间戳与跨机时钟偏差造成的「假新鲜」。
//! 本地增量路径先 `npm ci --dry-run` 前置校验锁一致性；不同步即拒绝自由解析。

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const STAMP: &str = ".fluxgate-build-stamp";

fn npm_path() -> Option<PathBuf> {
    // PATH 直查 + 常见 Homebrew 位置兜底（macOS GUI 启动 cargo 时 PATH 常不含
    // /opt/homebrew/bin）；Windows 下 npm 实为 npm.cmd，候选名两个都试。
    let candidates: &[&str] = if cfg!(windows) {
        &["npm.cmd", "npm"]
    } else {
        &["npm"]
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in candidates {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    if cfg!(target_os = "macos") {
        for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
            for name in candidates {
                let candidate = Path::new(dir).join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// 收集 web/console 的全部构建输入：src 子树（键为相对路径 "src/..."）递归 +
/// 顶层普通文件。顶层动态枚举，覆盖 vite/tailwind/postcss/tsconfig/package*.json
/// 等现在与将来的构建输入，不靠硬编码清单（漏一项就嵌陈旧 dist）；STAMP 自身
/// 与 *.tsbuildinfo（tsc 产物非输入）排除。任一读失败返回 None——按过期处理：
/// 宁可多建，不嵌陈旧。
fn input_snapshot(console_dir: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![console_dir.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(console_dir)
                    .ok()?
                    .to_string_lossy()
                    .into_owned();
                files.insert(rel, std::fs::read(&path).ok()?);
            }
        }
    }
    for entry in std::fs::read_dir(console_dir).ok()?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == STAMP || name.ends_with(".tsbuildinfo") {
            continue;
        }
        files.insert(name, std::fs::read(&path).ok()?);
    }
    Some(files)
}

/// 对快照逐条计算内容哈希（BTreeMap 有序遍历，结果确定）。DefaultHasher 跨
/// Rust 版本不保证稳定——哈希变化只会让戳失配触发一次整建，安全方向。
fn snapshot_hash(files: &BTreeMap<String, Vec<u8>>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (rel, content) in files {
        rel.hash(&mut hasher);
        content.hash(&mut hasher);
    }
    hasher.finish()
}

/// dist 新鲜 = dist/index.html 存在 且 输入快照可计算 且 戳文件记录的哈希
/// 与当前输入快照一致。
fn dist_is_current(console_dir: &Path) -> bool {
    if !console_dir.join("dist/index.html").is_file() {
        return false;
    }
    let Some(files) = input_snapshot(console_dir) else {
        return false;
    };
    let Ok(stamp) = std::fs::read_to_string(console_dir.join(STAMP)) else {
        return false;
    };
    stamp.trim() == format!("{:016x}", snapshot_hash(&files))
}

/// 构建成功后写内容戳；写失败打 warning（下次构建整建，安全方向）。
fn stamp_write(console_dir: &Path, hash: u64) {
    if let Err(e) = std::fs::write(console_dir.join(STAMP), format!("{hash:016x}\n")) {
        println!("cargo:warning=fluxgate console: 写构建戳失败（{e}），下次构建将整建");
    }
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
        // 稳态：内容戳命中（输入快照哈希与戳一致），直接复用，不打 npm。
        // 声明与重建成功后同一监视集，保持固定点（见尾部注释）。
        println!("cargo:rerun-if-changed=web/console");
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
    let is_ci = matches!(std::env::var("CI").as_deref(), Ok("true") | Ok("1"));
    if !is_ci {
        // 本地增量前置校验锁一致性：npm ci --dry-run 只比对 package-lock 与
        // package.json 是否同步，不动 node_modules。不同步即拒绝自由解析
        // （npm install 会按 package.json 更新锁，引入不可复现的漂移面）；
        // CI 全新环境本就严格按锁，无需校验。
        let status = std::process::Command::new(&npm)
            .arg("ci")
            .arg("--dry-run")
            .arg("--no-audit")
            .arg("--no-fund")
            .arg("--prefix")
            .arg(&console_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            other => {
                println!(
                    "cargo:warning=fluxgate console: package-lock 与 package.json 不同步（或校验失败，{other:?}），跳过前端构建——请先在 web/console 手动 npm install 并提交锁文件"
                );
                return;
            }
        }
    }
    let install_subcommand = if is_ci { "ci" } else { "install" };
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
    // 构建失败时不声明监视集：保持 cargo 默认「包内任意变更即重跑」，否则
    // 收窄后的监视集会让重试永远不触发。成功后与稳态早退路径声明同一
    // 监视集（固定点）。
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
    // 构建成功后计算输入快照并写内容戳：下次构建凭戳比对判定新鲜。
    // 快照失败（读不到输入）只告警——缺戳下次整建，安全方向。
    match input_snapshot(&console_dir) {
        Some(files) => stamp_write(&console_dir, snapshot_hash(&files)),
        None => println!(
            "cargo:warning=fluxgate console: 无法计算输入快照，跳过写构建戳（下次构建将整建）"
        ),
    }
    // 监视整个 web/console（cargo 对目录递归监视）：动态枚举清单存在「构建
    // 成功后新增的输入文件不在已记录监视集」与「未来 public/ 等顶层目录」
    // 两类缺口，整目录监视一并闭合。代价是 dist/node_modules 变更也触发
    // 重跑——重跑即一次内容戳快照比对，安全方向（宁多重跑不嵌陈旧）。
    println!("cargo:rerun-if-changed=web/console");
}
