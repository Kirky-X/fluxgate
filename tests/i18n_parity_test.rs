// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! i18n 键位守卫：把「en 默认 + zh 对齐」与「引用键必须存在」从人工审计
//! 变成每次 `cargo test` 自动拦截的门禁。
//!
//! 背景：`i18n::t`/`t_args` 的缺键行为是**原样返回键名**（回退链
//! 当前语言束 → en 束 → 键名，见 `crates/llm-gate/src/i18n/catalog.rs`），
//! 因此漏登记不会编译失败、也不会 panic，只把 `vendor-xxx` 这样的裸键吐给
//! 用户——静默失效，必须由测试钉住。
//!
//! 两条断言：
//! 1. 五个 `.ftl` 目录逐文件中 en 与 zh 的消息名集合相等（双向差集为空）；
//! 2. 生产代码（`src/`、`crates/llm-gate/src/`）里 `t("…")`/`t_args("…")`
//!    引用的每个键都在 en 目录登记（`zh` 由第 1 条保证同步存在）。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 目录分桶（与 `EMBEDDED_LOCALES` 的文件清单一致）。
const CATALOGS: &[&str] = &[
    "main",
    "api",
    "vendor",
    "config-protocol-router-middleware",
    "error-database-utils",
];

/// 刻意引用不存在的键：`catalog.rs` 用它断言「缺键返回键名本身」的回落契约。
/// 新增此项必须同步说明理由——否则等于放行一个真实漏登记。
const DECLARED_FIXTURES: &[&str] = &["no-such-key"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 读取 `.ftl` 内的消息名集合：仅取顶格 `name =` 行，注释行（`#`）与缩进的
/// 属性/多行 continuation 自然排除。
fn message_names(path: &Path) -> BTreeSet<String> {
    let content =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
    let mut names = BTreeSet::new();
    for line in content.lines() {
        // 消息定义必为顶格 `name = …`：`#` 注释行与缩进的属性 / 多行
        // continuation 均在此排除（不用闭包内 `?`，避开返回类型推断）。
        if line.len() == line.trim_start().len() && !line.starts_with('#') {
            if let Some((head, _rest)) = line.split_once('=') {
                let name = head.trim_end();
                if !name.is_empty() && is_message_name(name) {
                    names.insert(name.to_string());
                }
            }
        }
    }
    names
}

fn is_message_name(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 递归收集目录下的 `.rs` 文件。
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

/// 从源码中提取 `t("key")` / `t_args("key")` 的键名。
///
/// 边界判定：`t` 前一个字符不得是标识符字符（排除 `emit(`/`fmt(`/`not_t(`
/// 之类），`(` 与 `"` 之间允许空白与换行（跨行调用形如
/// `t_args(\n "key", …)`）。
fn referenced_keys(file: &Path) -> BTreeSet<String> {
    let content =
        fs::read_to_string(file).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", file.display()));
    let bytes = content.as_bytes();
    let mut keys = BTreeSet::new();
    for needle in ["t(", "t_args("] {
        for (idx, _) in content.match_indices(needle) {
            if idx > 0 {
                let prev = bytes[idx - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' {
                    continue;
                }
            }
            let after_paren = &content[idx + needle.len()..];
            let trimmed = after_paren.trim_start();
            let Some(rest) = trimmed.strip_prefix('"') else {
                continue;
            };
            let Some(end) = rest.find('"') else { continue };
            keys.insert(rest[..end].to_string());
        }
    }
    keys
}

#[test]
fn en_and_zh_catalogs_have_identical_message_names() {
    let root = repo_root();
    let mut failures: Vec<String> = Vec::new();
    for catalog in CATALOGS {
        let en = message_names(&root.join(format!("crates/llm-gate/locales/en/{catalog}.ftl")));
        let zh = message_names(&root.join(format!("crates/llm-gate/locales/zh/{catalog}.ftl")));
        let en_only: Vec<_> = en.difference(&zh).cloned().collect();
        let zh_only: Vec<_> = zh.difference(&en).cloned().collect();
        if !en_only.is_empty() || !zh_only.is_empty() {
            failures.push(format!(
                "{catalog}.ftl: en 独有 {en_only:?} / zh 独有 {zh_only:?}"
            ));
        }
        assert!(
            !en.is_empty(),
            "{catalog}.ftl 未解析到任何消息名——解析器与目录格式失配"
        );
    }
    assert!(
        failures.is_empty(),
        "en/zh 键位不对齐（en 是默认语言，zh 必须逐键跟进）:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_referenced_message_key_is_registered_in_en() {
    let root = repo_root();
    let registered: BTreeSet<String> = CATALOGS
        .iter()
        .flat_map(|catalog| {
            message_names(&root.join(format!("crates/llm-gate/locales/en/{catalog}.ftl")))
        })
        .collect();

    let mut sources = Vec::new();
    collect_rs_files(&root.join("src"), &mut sources);
    collect_rs_files(&root.join("crates/llm-gate/src"), &mut sources);

    let mut missing: Vec<String> = Vec::new();
    for file in &sources {
        for key in referenced_keys(file) {
            if !registered.contains(&key) && !DECLARED_FIXTURES.contains(&key.as_str()) {
                missing.push(format!("{} -> {key}", file.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "代码引用了未登记的 i18n 键（运行时会把裸键名吐给用户）:\n{}",
        missing.join("\n")
    );
}

#[test]
fn guard_itself_sees_the_catalog_surface() {
    // 防守卫空转：目录非空 + 至少一个真实键被解析到（键位数由第 1 条断言
    // 的「解析器与目录格式失配」兜住，此处再钉一次引用面非空）。
    let root = repo_root();
    let mut sources = Vec::new();
    collect_rs_files(&root.join("src"), &mut sources);
    let refs: BTreeSet<String> = sources
        .iter()
        .flat_map(|path| referenced_keys(path))
        .filter(|key| !DECLARED_FIXTURES.contains(&key.as_str()))
        .collect();
    assert!(
        !refs.is_empty(),
        "守卫未采集到任何 i18n 引用——扫描逻辑已失效，必须修扫描而不是删断言"
    );
}
