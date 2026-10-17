// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 死代码标记守卫：把「`src/` 生产代码不得残留裸 `#[allow(dead_code)]` /
//! `#[allow(unused…)]`」从人工审计变成每次 `cargo test` 自动拦截的门禁。
//!
//! 背景：`dead-code-complexity-cleanup` 治理确立的目标形态是
//! `#[cfg_attr(not(feature = "…"), allow(dead_code))]`——特性缺位时符号
//! 不参与编译，豁免理所当然；特性开启时若仍有未用符号，编译器 `dead_code`
//! lint 会直接报警。裸 `#[allow(dead_code)]` 恰恰会**掩盖**「代码活着却没
//! 接线」的真实状态，必须禁止，只留特性门控形态。
//!
//! 扫描边界：仅 `src/`（本仓产品代码面）。`tests/`、`examples/`、
//! `crates/llm-gate/` 不在域内——测试共享 helper 的 `#![allow(dead_code)]`
//! 是常态，另有文档注释说明理由。`#[allow(clippy::…)]` 形态不受本守卫
//! 约束（其语义与死代码无关）。

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 递归收集目录下的 `.rs` 文件（与 i18n 键位守卫同款遍历）。
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
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

/// 一行是否为「裸 allow(dead_code)/allow(unused)」违例：命中敏感串但不带
/// `cfg_attr`（特性门控豁免形态），且非注释 / 文档注释行。
fn is_bare_allow(line: &str) -> bool {
    let trimmed = line.trim_start();
    // 跳过注释与文档注释行——说明性文字里的 allow(dead_code) 不是属性。
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return false;
    }
    let mentions = line.contains("allow(dead_code)") || line.contains("allow(unused");
    mentions && !line.contains("cfg_attr")
}

#[test]
fn src_has_no_bare_dead_code_or_unused_allows() {
    let src_dir = repo_root().join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(
        !files.is_empty(),
        "src/ 未收集到任何 .rs 文件，遍历逻辑可能失效"
    );

    let mut violations = Vec::new();
    for file in &files {
        let content = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", file.display()));
        for (lineno, line) in content.lines().enumerate() {
            if is_bare_allow(line) {
                violations.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(repo_root()).unwrap_or(file).display(),
                    lineno + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "src/ 存在裸 #[allow(dead_code)] / #[allow(unused…)]：应改为 \
         #[cfg_attr(not(feature = \"…\"), allow(…))] 特性门控形态，或接线消除未用符号。\n{}",
        violations.join("\n")
    );
}
