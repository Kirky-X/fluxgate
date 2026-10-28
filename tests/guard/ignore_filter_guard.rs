// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `#[ignore]` 命名守卫：CI server-backend 作业以子串过滤器
//! `--test database_test server -- --ignored` 执行服务端方言用例，该契约
//! 要求 database_test 内所有 `#[ignore]` 条目的完整路径必含 `server`。
//! 未来新增 server-only 用例若命名不含 `server`，CI 会**静默漏跑**（零
//! 命中仍 exit 0，无任何报错）——本守卫把该口头约定升格为每次
//! `cargo test` 的机械校验，与 ci.yml 作业内的 `--list` 命中数断言互补
//!（守卫拦「新用例名字漏出过滤器」，作业断言拦「过滤器本身空匹配」）。

use std::fs;
use std::path::PathBuf;

fn database_test_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/database/database_test.rs");
    fs::read_to_string(&path).expect("tests/database/database_test.rs 必须存在")
}

/// 提取 `#[ignore]` 属性紧随其后的条目名（`mod X` / `fn X` / `async fn X`，
/// 含 `pub ` 变体）。属性与条目声明之间允许穿插其他属性行（如 `#[tokio::test]`）
/// 与注释；声明行无法识别时立即 panic——静默跳过会让单条漏检无任何断言
/// 可暴露，只有全部落空才被 `!names.is_empty()` 兜住。
fn ignored_item_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[ignore") {
            pending = true;
            continue;
        }
        if !pending {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        match item_name(trimmed) {
            Some(name) => names.push(name),
            None => panic!(
                "#[ignore] 后的声明行无法解析（跨行签名或未收录的前缀变体），\
                 静默跳过等于守卫漏检：{trimmed}"
            ),
        }
        pending = false;
    }
    names
}

/// 条目声明行 → 条目名（行内首段标识符）。前缀表长前缀在前：`pub ` 变体
/// 先于裸关键字，否则 `pub fn` 会漏配全部前缀。新增可见性/修饰符变体时
/// 在此表补列。
fn item_name(line: &str) -> Option<String> {
    [
        "pub async fn ",
        "pub fn ",
        "pub mod ",
        "async fn ",
        "mod ",
        "fn ",
    ]
    .iter()
    .find_map(|kw| line.strip_prefix(kw))
    .map(|rest| {
        rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

#[test]
fn item_name_recognizes_pub_variants() {
    assert_eq!(
        item_name("pub async fn v7_and_upsert_run_on_server_backend() {").as_deref(),
        Some("v7_and_upsert_run_on_server_backend")
    );
    assert_eq!(item_name("pub fn plain() {").as_deref(), Some("plain"));
    assert_eq!(
        item_name("pub mod server_side {").as_deref(),
        Some("server_side")
    );
    assert_eq!(item_name("async fn bare() {").as_deref(), Some("bare"));
    assert_eq!(item_name("mod nested {").as_deref(), Some("nested"));
}

#[test]
#[should_panic(expected = "声明行无法解析")]
fn unparsable_declaration_line_panics_instead_of_silent_drop() {
    ignored_item_names("#[ignore = \"需要实例\"]\npub(crate) async fn hidden() {}\n");
}

#[test]
fn every_ignored_database_test_contains_server_in_name() {
    let names = ignored_item_names(&database_test_source());
    assert!(
        !names.is_empty(),
        "database_test.rs 无任何 #[ignore] 用例：server-backend 作业将空跑，\
         过滤器契约失效（若确已全部移除，须同步收缩 CI 作业与本守卫）"
    );
    let violations: Vec<String> = names
        .iter()
        .filter(|name| !name.contains("server"))
        .map(|name| {
            format!(
                "{name}: #[ignore] 用例命名必含 `server`，否则 CI server-backend \
                 作业的 `server` 子串过滤器会静默漏跑该用例"
            )
        })
        .collect();
    assert!(
        violations.is_empty(),
        "database_test.rs 存在漏出 server 过滤器的 #[ignore] 用例：\n{}",
        violations.join("\n")
    );
}
