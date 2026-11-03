// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 版权头守卫：src/ tests/ benches/ examples/ scripts/ build.rs 与
//! web/console 源码（.rs/.ts/.tsx/.js/.py/.sh/.css，git 跟踪面）必须恰好
//! 含一行 `Copyright (c) 2026 Kirky.X🌠`——既防缺失，也防脚本化补头时
//! 重复叠加。生成物（dist/、node_modules/、target/）不扫。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MARK: &str = "Copyright (c) 2026 Kirky.X🌠";
const EXTS: [&str; 7] = ["rs", "ts", "tsx", "js", "py", "sh", "css"];
const SKIP_DIRS: [&str; 4] = ["node_modules", "dist", "target", ".git"];

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files"])
        .output()
        .expect("git ls-files should run (guard requires a git workdir)");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn scan(dir: &Path, tracked: &[String], out: &mut Vec<PathBuf>) {
    let Ok(rd) = dir.read_dir() else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                scan(&path, tracked, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTS.contains(&e))
        {
            // 只约束 git 跟踪面：编辑器草稿/未跟踪临时文件不进守卫
            if tracked.iter().any(|t| Path::new(t) == path) {
                out.push(path);
            }
        }
    }
}

#[test]
fn copyright_header_present_exactly_once() {
    let tracked = tracked_files();
    let mut files = Vec::new();
    for root in [
        "src",
        "tests",
        "benches",
        "examples",
        "scripts",
        "web/console",
    ] {
        scan(Path::new(root), &tracked, &mut files);
    }
    if Path::new("build.rs").is_file() {
        files.push(PathBuf::from("build.rs"));
    }
    assert!(!files.is_empty(), "guard scan set must not be empty");

    let mut violations = Vec::new();
    for f in &files {
        let content = fs::read_to_string(f).unwrap_or_else(|e| panic!("read {f:?}: {e}"));
        let count = content.matches(MARK).count();
        if count != 1 {
            violations.push(format!("{}: {} 处版权头（须恰好 1）", f.display(), count));
        }
    }
    assert!(
        violations.is_empty(),
        "版权头缺失/重复：\n{}",
        violations.join("\n")
    );
}
