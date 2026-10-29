// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `fluxgate-replay` — 离线成本回放 CLI（`cost-replay` feature）。
//!
//! 读既有 `request_logs` 历史，按时间窗重放 affinity/cache_ttl 场景的
//! counterfactual 成本，stdout 出 Markdown（或 `--json`）。纯只读：不开服务、
//! 不写库、不调模型。
//!
//! 手写参数解析（参数面小，不为单工具拉 clap）：
//! `--db <path|url>`、`--from <unix|RFC3339>`、`--to <unix|RFC3339>`、
//! `--config <path>`（缺省 `config.toml`，取 `[[model_catalog]]` 单价）、
//! `--scenario <affinity|cache_ttl|all>`、`--json`。缺参 → stderr 报错 + 退出码 2。

#![cfg(all(feature = "cost-replay", feature = "db-sqlite"))]

use fluxgate::config::DatabaseConfig;
use fluxgate::database::resolve_database_url;
use fluxgate::i18n::{t, t_args};
use fluxgate::utils::replay::{self, ReplayReport};
use std::path::PathBuf;

/// 场景选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// legacy（无缓存收益）+ 单档 auto 对照。
    Affinity,
    /// {300,900,3600} 三档对比。
    CacheTtl,
    /// 全部（默认）。
    All,
}

struct Args {
    db: String,
    from: i64,
    to: i64,
    config: String,
    scenario: Scenario,
    json: bool,
}

fn usage() -> String {
    t("replay-cli-usage")
}

/// 解析 `key value` 形态的取值；缺值回 `None`。
fn take_value(args: &[String], i: &mut usize) -> Option<String> {
    *i += 1;
    args.get(*i).cloned()
}

/// 时间解析：unix 秒整数或 RFC3339 时间戳。
fn parse_time(raw: &str) -> Option<i64> {
    if let Ok(secs) = raw.parse::<i64>() {
        return Some(secs);
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp())
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut db: Option<String> = None;
    let mut from: Option<i64> = None;
    let mut to: Option<i64> = None;
    let mut config = "config.toml".to_string();
    let mut scenario = Scenario::All;
    let mut json = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "--db" => {
                db =
                    Some(take_value(argv, &mut i).ok_or_else(|| {
                        t_args("replay-cli-missing-value", &[("arg", arg.into())])
                    })?);
            }
            "--from" => {
                let v = take_value(argv, &mut i)
                    .ok_or_else(|| t_args("replay-cli-missing-value", &[("arg", arg.into())]))?;
                from = Some(
                    parse_time(&v)
                        .ok_or_else(|| t_args("replay-cli-bad-time", &[("value", v.clone())]))?,
                );
            }
            "--to" => {
                let v = take_value(argv, &mut i)
                    .ok_or_else(|| t_args("replay-cli-missing-value", &[("arg", arg.into())]))?;
                to = Some(
                    parse_time(&v)
                        .ok_or_else(|| t_args("replay-cli-bad-time", &[("value", v.clone())]))?,
                );
            }
            "--config" => {
                config = take_value(argv, &mut i)
                    .ok_or_else(|| t_args("replay-cli-missing-value", &[("arg", arg.into())]))?;
            }
            "--scenario" => {
                let v = take_value(argv, &mut i)
                    .ok_or_else(|| t_args("replay-cli-missing-value", &[("arg", arg.into())]))?;
                scenario = match v.as_str() {
                    "affinity" => Scenario::Affinity,
                    "cache_ttl" => Scenario::CacheTtl,
                    "all" => Scenario::All,
                    other => {
                        return Err(t_args(
                            "replay-cli-bad-scenario",
                            &[("value", other.into())],
                        ));
                    }
                };
            }
            "--json" => json = true,
            "--help" | "-h" => return Err(usage()),
            other => return Err(t_args("replay-cli-unknown-arg", &[("arg", other.into())])),
        }
        i += 1;
    }

    let db = db.ok_or_else(|| t_args("replay-cli-missing-arg", &[("arg", "--db".into())]))?;
    let from = from.ok_or_else(|| t_args("replay-cli-missing-arg", &[("arg", "--from".into())]))?;
    let to = to.ok_or_else(|| t_args("replay-cli-missing-arg", &[("arg", "--to".into())]))?;
    if to < from {
        return Err(t("replay-cli-bad-window"));
    }

    Ok(Args {
        db,
        from,
        to,
        config,
        scenario,
        json,
    })
}

/// 按选择保留场景（legacy 始终来自 affinity 组）。
fn select_scenarios(mut report: ReplayReport, scenario: Scenario) -> ReplayReport {
    report.scenarios.retain(|s| match scenario {
        Scenario::All => true,
        // affinity：legacy 对照 + 最大档 auto（保 warm 收益上限）
        Scenario::Affinity => s.name == "auto-3600s",
        // cache_ttl：仅三档 auto，无 legacy
        Scenario::CacheTtl => s.name.starts_with("auto-"),
    });
    report
}

/// 把 `--db` 解析为完整连接 URL：含 scheme 视为 URL，否则按 sqlite 文件路径。
fn resolve_db_url(db: &str) -> String {
    if db.contains("://") {
        db.to_string()
    } else {
        resolve_database_url(&DatabaseConfig {
            path: db.to_string(),
            pool_size: 1,
            url: String::new(),
        })
        .unwrap_or_else(|_| db.to_string())
    }
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    match run(&args).await {
        Ok(output) => println!("{output}"),
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    }
}

async fn run(args: &Args) -> Result<String, String> {
    // 目录单价正源：既有 config.toml 的 [[model_catalog]]（只读解析）。
    let catalog = fluxgate::config::loader::load_config_from_path(&PathBuf::from(&args.config))
        .map_err(|e| t_args("replay-cli-config-load-failed", &[("error", e.to_string())]))?
        .model_catalog;

    let pool = replay::open_pool(&resolve_db_url(&args.db))
        .await
        .map_err(|e| t_args("replay-cli-db-open-failed", &[("error", e.to_string())]))?;
    let rows = replay::load_rows(&pool, args.from, args.to)
        .await
        .map_err(|e| t_args("replay-cli-load-failed", &[("error", e.to_string())]))?;

    let report = select_scenarios(
        replay::build_report(&rows, &catalog, args.from, args.to),
        args.scenario,
    );
    Ok(if args.json {
        report.render_json()
    } else {
        report.render_markdown()
    })
}
