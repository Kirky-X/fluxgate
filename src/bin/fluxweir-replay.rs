// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `fluxweir-replay` — 离线成本回放 CLI（`cost-replay` feature）。
//!
//! 读既有 `request_logs` 历史，按时间窗重放 affinity/cache_ttl 场景的
//! counterfactual 成本，stdout 出 Markdown（或 `--json`）。纯只读：不开服务、
//! 不写库、不调模型。SQLite 目标以 `mode=ro` 打开且必须已存在——工具绝不新建库文件。
//!
//! 手写参数解析（参数面小，不为单工具拉 clap）：
//! `--db <path|url>`、`--from <unix|RFC3339>`、`--to <unix|RFC3339>`、
//! `--config <path>`（缺省 `config.toml`，取 `[[model_catalog]]` 单价）、
//! `--scenario <affinity|cache_ttl|all>`、`--max-rows <n>`、`--json`。
//! 缺参/参数非法 → stderr 报错 + 退出码 2；运行期失败 → 退出码 1；
//! `--help`/`-h` → stdout 打印用法 + 退出码 0。

#![cfg(all(feature = "cost-replay", feature = "db-sqlite"))]

use fluxweir::database::redact_db_url;
use fluxweir::i18n::{t, t_args};
use fluxweir::utils::replay::{self, ReplayReport};
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
    max_rows: u64,
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
    let mut max_rows = replay::DEFAULT_MAX_ROWS;
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
            "--max-rows" => {
                let v = take_value(argv, &mut i)
                    .ok_or_else(|| t_args("replay-cli-missing-value", &[("arg", arg.into())]))?;
                let parsed =
                    v.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                        t_args("replay-cli-bad-max-rows", &[("value", v.clone())])
                    })?;
                max_rows = parsed;
            }
            "--json" => json = true,
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
        max_rows,
        json,
    })
}

/// 按选择保留场景（legacy 始终来自 affinity 组）。
fn select_scenarios(mut report: ReplayReport, scenario: Scenario) -> ReplayReport {
    report.scenarios.retain(|s| match scenario {
        Scenario::All => true,
        // affinity：legacy 对照（无缓存收益基准）+ 最大档 auto（保 warm 收益上限）
        Scenario::Affinity => s.name == "legacy" || s.name == "auto-3600s",
        // cache_ttl：仅三档 auto，无 legacy
        Scenario::CacheTtl => s.name.starts_with("auto-"),
    });
    report
}

/// 剥除 URL 查询串里既有的 `mode=` 参数（工具强制只读，避免重复/冲突取值）。
fn strip_mode_param(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("mode="))
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

/// 把 `--db` 解析为**只读**连接 URL。
///
/// - 非 SQLite 的 `scheme://` URL 原样保留（各后端无统一只读开关）；
/// - SQLite（裸路径或 `sqlite:` URL）：目标文件**必须已存在**（不存在即报错，
///   绝不借 `mode=rwc` 静默新建空库），并统一强制 `mode=ro`；
/// - `sqlite::memory:` 原样保留（无文件可建，回放将得空结果）。
fn resolve_db_url(db: &str) -> Result<String, String> {
    if !db.starts_with("sqlite:") && db.contains("://") {
        return Ok(db.to_string());
    }
    let path_part = db
        .strip_prefix("sqlite://")
        .or_else(|| db.strip_prefix("sqlite:"))
        .unwrap_or(db);
    let file = path_part.split('?').next().unwrap_or(path_part);
    if file == ":memory:" {
        return Ok(db.to_string());
    }
    if !std::path::Path::new(file).exists() {
        return Err(t_args("replay-cli-db-not-found", &[("path", db.into())]));
    }
    let url = if db.starts_with("sqlite:") {
        db.to_string()
    } else {
        format!("sqlite://{db}")
    };
    let url = strip_mode_param(&url);
    let sep = if url.contains('?') { '&' } else { '?' };
    Ok(format!("{url}{sep}mode=ro"))
}

/// 错误串里的连接 URL 脱敏：dbnexus 报错会回显完整连接串（含 userinfo 口令），
/// 必须以 [`redact_db_url`] 形态落 stderr，避免口令进 CI 日志 / 终端回滚缓冲。
fn redact_error(message: &str, url: &str) -> String {
    message.replace(url, &redact_db_url(url))
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 帮助不是用法错误：stdout + 退出码 0（脚本 `--help` 探测不得被误判失败）。
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return;
    }
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
    // 先做只读 URL 解析：库文件不存在即 fail-fast（与配置是否合法无关）。
    let url = resolve_db_url(&args.db)?;

    // 目录单价正源：既有 config.toml 的 [[model_catalog]]（只读解析）。
    let catalog = fluxweir::config::loader::load_config_from_path(&PathBuf::from(&args.config))
        .map_err(|e| t_args("replay-cli-config-load-failed", &[("error", e.to_string())]))?
        .model_catalog;

    let pool = replay::open_pool(&url).await.map_err(|e| {
        t_args(
            "replay-cli-db-open-failed",
            &[("error", redact_error(&e.to_string(), &url))],
        )
    })?;
    let rows = replay::load_rows(&pool, args.from, args.to, args.max_rows)
        .await
        .map_err(|e| {
            t_args(
                "replay-cli-load-failed",
                &[("error", redact_error(&e.to_string(), &url))],
            )
        })?;

    let report = select_scenarios(
        replay::build_report(&rows, &catalog, args.from, args.to, &args.config),
        args.scenario,
    );
    Ok(if args.json {
        report.render_json()
    } else {
        report.render_markdown()
    })
}
