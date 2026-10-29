// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! cost-replay 集成测试：CLI 子进程跑 fixture 库断言退出码与报告关键字段，
//! 以及确定性回归（同输入两跑 byte-equal，无随机无时钟）。
//!
//! 覆盖：
//! - 退出码：缺参 → 2；正常 → 0。
//! - 报告关键字段：窗口、baseline、场景行、假设披露。
//! - `--json` 机器可读且与 Markdown 数值一致。
//! - `--scenario` 过滤。

#![cfg(all(feature = "cost-replay", feature = "db-sqlite"))]

use fluxgate::database::{Database, RequestLog};
use std::process::Command;

/// 写一条 fixture 请求日志。
#[allow(clippy::too_many_arguments)]
fn log(
    request_id: &str,
    ts: i64,
    session: Option<&str>,
    vendor: &str,
    model: &str,
    prompt: i32,
    completion: i32,
    cache_write: i64,
    cost: f64,
) -> RequestLog {
    RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: ts,
        endpoint: "/v1/chat/completions".to_string(),
        input_format: "openai".to_string(),
        abstract_model: None,
        messages: "[]".to_string(),
        actual_vendor: vendor.to_string(),
        actual_model: model.to_string(),
        routing_strategy: "weighted".to_string(),
        status_code: Some(200),
        response: None,
        error_message: None,
        prompt_tokens: Some(prompt),
        completion_tokens: Some(completion),
        total_tokens: Some(prompt + completion),
        duration_ms: Some(1),
        cost_usd: Some(cost),
        session_key: session.map(str::to_string),
        context_window_tokens: None,
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(cache_write),
    }
}

/// fixture 库 + 目录配置：返回 (db url, config 路径)。
async fn fixture() -> (String, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("replay.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let db = Database::new(&url, 1).await.expect("fixture db");
    for l in [
        log(
            "r1",
            1_000,
            Some("s1"),
            "v1",
            "claude-sonnet-4",
            1_000_000,
            0,
            0,
            3.0,
        ),
        log(
            "r2",
            1_100,
            Some("s1"),
            "v1",
            "claude-sonnet-4",
            1_000_000,
            0,
            0,
            3.0,
        ),
        log(
            "r3",
            1_200,
            None,
            "v2",
            "claude-sonnet-4",
            1_000_000,
            0,
            0,
            3.0,
        ),
    ] {
        db.repository().log_request(&l).await.expect("seed");
    }

    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
vendors = []

[server]
host = "127.0.0.1"
port = 18099

[database]
path = ":memory:"

[[model_catalog]]
model_id = "claude-sonnet-4"
context_window = 200000
input_price = 3.0
output_price = 15.0
cache_read_price = 0.3
cache_creation_price = 3.75
"#,
    )
    .expect("write config");

    (url, config_path, dir)
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_fluxgate-replay")
}

/// 正常路径：退出码 0，报告含窗口/基线/场景/假设关键字段。
#[tokio::test]
async fn cli_reports_replay_scenarios() {
    let (url, config, _dir) = fixture().await;
    let out = Command::new(bin())
        .args([
            "--db",
            &url,
            "--from",
            "0",
            "--to",
            "99999",
            "--config",
            config.to_str().unwrap(),
        ])
        .output()
        .expect("spawn replay bin");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# Cost replay report"), "{stdout}");
    assert!(stdout.contains("window: 0..99999 (3 requests)"), "{stdout}");
    assert!(stdout.contains("| baseline |"), "{stdout}");
    assert!(stdout.contains("| legacy |"), "{stdout}");
    assert!(stdout.contains("| auto-300s |"), "{stdout}");
    assert!(stdout.contains("| auto-900s |"), "{stdout}");
    assert!(stdout.contains("| auto-3600s |"), "{stdout}");
    assert!(stdout.contains("## assumptions"), "{stdout}");
    assert!(
        stdout.contains("NULL-session rows have no cache benefit"),
        "{stdout}"
    );
    // r2 在 s1 会话内、同 vendor、间隔 100 < TTL → auto 命中一行
    // total = r1 miss 3.0 + r2 hit 0.3 + r3(NULL) miss 3.0 = 6.3 USD
    assert!(stdout.contains("| auto-3600s | 6.300000 |"), "{stdout}");
}

/// 缺参：退出码 2。
#[tokio::test]
async fn cli_missing_args_exit_code_2() {
    let out = Command::new(bin())
        .arg("--from")
        .arg("0")
        .output()
        .expect("spawn replay bin");
    assert_eq!(out.status.code(), Some(2), "缺 --db 必须退出码 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing required argument"), "{stderr}");
}

/// `--json` 机器可读，且与 Markdown 数值同源。
#[tokio::test]
async fn cli_json_matches_markdown() {
    let (url, config, _dir) = fixture().await;
    let json_out = Command::new(bin())
        .args([
            "--db",
            &url,
            "--from",
            "0",
            "--to",
            "99999",
            "--config",
            config.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn");
    assert_eq!(json_out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&json_out.stdout).expect("valid json");
    assert_eq!(json["window"]["requests"], 3);
    // baseline = 3 × 3.0 USD = 9.0 USD = 9_000_000 µ$
    assert_eq!(json["baseline"]["total_micro_usd"], 9_000_000);
    assert_eq!(json["assumptions"].as_array().unwrap().len(), 3);
}

/// `--scenario cache_ttl` 只保留三档 auto，不含 legacy。
#[tokio::test]
async fn cli_scenario_filter() {
    let (url, config, _dir) = fixture().await;
    let out = Command::new(bin())
        .args([
            "--db",
            &url,
            "--from",
            "0",
            "--to",
            "99999",
            "--config",
            config.to_str().unwrap(),
            "--scenario",
            "cache_ttl",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("| legacy |"), "{stdout}");
    assert!(stdout.contains("| auto-300s |"), "{stdout}");
}

/// 确定性：同输入两跑 stdout byte-equal。
#[tokio::test]
async fn cli_output_is_deterministic() {
    let (url, config, _dir) = fixture().await;
    let run = || {
        Command::new(bin())
            .args([
                "--db",
                &url,
                "--from",
                "0",
                "--to",
                "99999",
                "--config",
                config.to_str().unwrap(),
            ])
            .output()
            .expect("spawn")
    };
    let a = run();
    let b = run();
    assert_eq!(a.status.code(), Some(0));
    assert_eq!(a.stdout, b.stdout, "同输入两跑必须 byte-equal");
}
