// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 注释卫生守卫：防止流程戳记（工单编号、评审编号、外部设计文档引用）回流到
//! 源码注释。按仓库约定，任务/评审的追踪信息只写 commit message，不写源码注释
//! （见 docs/CONTRIBUTING.md）。本守卫扫描 `src` / `tests` / `examples` / `benches`
//! 的 `.rs` 注释行与两处 `Cargo.toml` 注释行，命中任一禁用模式即红灯。

use std::fs;
use std::path::{Path, PathBuf};

/// 递归收集目录下所有 `.rs` 文件。
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = dir.read_dir() else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// 判断一行注释文本是否含流程戳记；命中返回禁用模式名。
/// 仅检测机器可判定、无语义歧义的工单码，避免误伤合法注释。
fn stamp_violation(text: &str) -> Option<&'static str> {
    if text.contains("recId=") {
        return Some("recId=<n>");
    }
    if text.contains("design.md") {
        return Some("design.md 引用");
    }
    const REVIEW_CODES: [&str; 4] = ["评审 major", "评审 minor", "评审修正", "评审 修正"];
    for kw in REVIEW_CODES {
        if text.contains(kw) {
            return Some("评审编号");
        }
    }
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        // WP + 数字：工作包编号
        if chars[i] == 'W'
            && i + 2 < chars.len()
            && chars[i + 1] == 'P'
            && chars[i + 2].is_ascii_digit()
        {
            return Some("WP<n>");
        }
        // 全角括号内的字母 + 数字：G/O/W 前缀工单码（排除 T——T0 等为时间基准变量）
        if chars[i] == '（'
            && i + 2 < chars.len()
            && matches!(chars[i + 1], 'G' | 'O' | 'W')
            && chars[i + 2].is_ascii_digit()
        {
            return Some("（<work-code><n>）");
        }
    }
    None
}

#[test]
fn source_comments_have_no_process_stamps() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for sub in ["src", "tests", "examples", "benches"] {
        collect_rs(&root.join(sub), &mut files);
    }

    let mut violations = Vec::new();
    for file in &files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("//") {
                continue; // 只检注释行，跳过字符串字面量与代码
            }
            if let Some(pat) = stamp_violation(trimmed) {
                violations.push(format!("{}:{} → <{}>", file.display(), idx + 1, pat));
            }
        }
    }

    for rel in ["Cargo.toml", "examples/Cargo.toml"] {
        let path = root.join(rel);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('#') {
                continue;
            }
            if let Some(pat) = stamp_violation(trimmed) {
                violations.push(format!("{}:{} → <{}>", path.display(), idx + 1, pat));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "注释中发现流程戳记（追踪信息应写进 commit message，不写进源码注释）：\n{}",
        violations.join("\n")
    );
}
