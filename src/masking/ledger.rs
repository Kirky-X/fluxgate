// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 掩码审计台账（feature `masking`；`[masking].ledger.enabled` 运行时门，
//! 默认关）：掩码命中的**明细事实源**——按天 JSONL 追加
//! `{ts, kind, finder, placeholder, hint}`，hint = 值首字符 + 字节长度。
//! **值与占位符→原值映射不落盘**（反向映射仅内存，见 `placeholder` 模块）。
//!
//! 落盘：`~/.fluxweir/mask_ledger/ledger-YYYYMMDD.jsonl`（UTC 日界）。写入
//! 走 O_APPEND 打开 + 单次 `write_all`（同进程写入串行于内部互斥锁；POSIX
//! append 模式下单次 write 追加位置原子）。每日首写触发裁剪：先删超出
//! `retention_days` 的日文件，再在总量超 `max_total_bytes` 时从最旧日文件
//! 开始删除。台账是观测旁路：任何 IO 失败 warn 降级，绝不影响掩码主链路。
//!
//! 读取面（`read_day`）供 `GET /admin/masking/ledger` 做 per-kind 聚合与
//! 明细分页；`masking_stats` 的 24h 窗口聚合不重复建设，台账是明细事实源。

use crate::config::ConfigManager;
use chrono::{NaiveDate, SecondsFormat, Utc};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 台账目录名（home 下）：`~/.fluxweir/mask_ledger/`。
pub const LEDGER_DIR_NAME: &str = "mask_ledger";

/// 单条台账记录（写入面；生产路径由 `gates.rs` 掩码命中回填）。
pub(crate) struct LedgerRecord {
    /// 敏感值种类（`MaskKind::as_str()` 词汇）
    pub kind: String,
    /// 检测来源（inklog pattern 名 / custom_rule_N / word）
    pub finder: String,
    /// 替换产生的占位符（Warn 动作未替换为空串）
    pub placeholder: String,
    /// 值首字符（hint；仅此一字符，不携带完整值）
    pub first: char,
    /// 值字节长度
    pub len: usize,
}

/// 按天文件的打开状态（open_day 与 file 成对；跨日重开）。
struct LedgerState {
    open_day: Option<NaiveDate>,
    file: Option<File>,
}

/// 掩码审计台账。`dir = None`（home 不可得）时整体停用（构造期 warn，
/// record 恒零行为）。enabled/retention/max 全部热读 `[masking.ledger]`。
pub struct MaskingLedger {
    dir: Option<PathBuf>,
    config: Arc<ConfigManager>,
    state: Mutex<LedgerState>,
}

impl std::fmt::Debug for MaskingLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskingLedger")
            .field("dir", &self.dir)
            .field("enabled", &self.enabled())
            .finish_non_exhaustive()
    }
}

impl MaskingLedger {
    /// 生产构造：数据目录取 `~/.fluxweir/mask_ledger/`；home 不可得 →
    /// 停用并 warn（观测旁路不因此失败）。
    pub fn from_config(config: Arc<ConfigManager>) -> Self {
        let dir = match std::env::home_dir() {
            Some(home) => Some(home.join(".fluxweir").join(LEDGER_DIR_NAME)),
            None => {
                tracing::warn!(
                    "{}",
                    crate::i18n::t_args(
                        "masking-ledger-io-failed",
                        &[
                            ("op", "init".to_string()),
                            (
                                "detail",
                                "home dir unavailable; ledger disabled".to_string()
                            ),
                        ],
                    ),
                );
                None
            }
        };
        Self::with_dir(dir, config)
    }

    /// 显式目录构造（测试与嵌入面；`None` = 停用）。
    pub fn with_dir(dir: Option<PathBuf>, config: Arc<ConfigManager>) -> Self {
        Self {
            dir,
            config,
            state: Mutex::new(LedgerState {
                open_day: None,
                file: None,
            }),
        }
    }

    /// 台账开关（`[masking].ledger.enabled` 热读）。
    pub fn enabled(&self) -> bool {
        self.config.read().masking.ledger.enabled
    }

    /// 追加一批记录（enabled=false 或停用零行为）。生产走本入口（now 取
    /// 当前 UTC）；测试用 [`Self::record_with_now`] 注入时钟。
    pub(crate) fn record(&self, records: &[LedgerRecord]) {
        if records.is_empty() {
            return;
        }
        self.record_with_now(records, Utc::now());
    }

    /// 注入时钟的追加核心（测试面）：日界判定与 ts 全部来自 `now`。
    pub(crate) fn record_with_now(&self, records: &[LedgerRecord], now: chrono::DateTime<Utc>) {
        if !self.enabled() || self.dir.is_none() {
            return;
        }
        if let Err(e) = self.append_locked(records, now) {
            tracing::warn!(
                "{}",
                crate::i18n::t_args(
                    "masking-ledger-io-failed",
                    &[("op", "append".to_string()), ("detail", e.to_string()),],
                ),
            );
        }
    }

    /// 锁内追加：跨日（或首写）重开文件并触发裁剪，再逐条成行写入。
    fn append_locked(
        &self,
        records: &[LedgerRecord],
        now: chrono::DateTime<Utc>,
    ) -> std::io::Result<()> {
        let dir = self.dir.as_deref().expect("dir checked by caller");
        let day = now.date_naive();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.open_day != Some(day) {
            state.file = None;
            std::fs::create_dir_all(dir)?;
            state.file = Some(
                OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(day_file_path(dir, day))?,
            );
            state.open_day = Some(day);
            self.trim_locked(dir, now);
        }
        let file = state.file.as_mut().expect("opened above");
        let ts = now.to_rfc3339_opts(SecondsFormat::Millis, true);
        for record in records {
            let line = serde_json::json!({
                "ts": ts,
                "kind": record.kind,
                "finder": record.finder,
                "placeholder": record.placeholder,
                "hint": {"first": record.first, "len": record.len},
            });
            // 单次 write_all 一行含换行（O_APPEND 下追加位置原子——多进程
            // 同文件追加时整行不交错的原子性前提）
            let mut row = line.to_string();
            row.push('\n');
            file.write_all(row.as_bytes())?;
        }
        Ok(())
    }

    /// 每日首写裁剪（锁内调用）：retention 天数裁剪 → 总量裁剪（最旧优先）。
    /// 删除失败不中断（观测旁路不反压主链路），仅 debug 披露。
    fn trim_locked(&self, dir: &Path, now: chrono::DateTime<Utc>) {
        let section = &self.config.read().masking.ledger;
        let cutoff =
            now.date_naive() - chrono::Duration::days(i64::from(section.retention_days - 1));
        let days = match day_files(dir) {
            Ok(days) => days,
            Err(_) => return,
        };
        let mut kept: Vec<(NaiveDate, PathBuf)> = Vec::new();
        for (day, path) in days {
            if day < cutoff {
                if std::fs::remove_file(&path).is_ok() {
                    tracing::debug!(day = %day.format("%Y%m%d"), "masking ledger day file expired");
                }
            } else {
                kept.push((day, path));
            }
        }
        let mut total: u64 = kept
            .iter()
            .filter_map(|(_, path)| std::fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum();
        if total <= section.max_total_bytes {
            return;
        }
        for (day, path) in &kept {
            if total <= section.max_total_bytes {
                break;
            }
            let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
                tracing::debug!(
                    day = %day.format("%Y%m%d"),
                    "masking ledger day file trimmed (max_total_bytes)"
                );
            }
        }
    }

    /// 读取某日台账（admin 面）：逐行解析为 JSON 对象；文件缺失 → 空表。
    /// 行解析失败跳过并计数（畸形行不中断读取面）。
    pub(crate) fn read_day(&self, day: NaiveDate) -> std::io::Result<Vec<serde_json::Value>> {
        let Some(dir) = &self.dir else {
            return Ok(Vec::new());
        };
        let path = day_file_path(dir, day);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                lines.push(value);
            }
        }
        Ok(lines)
    }
}

/// 日文件路径：`<dir>/ledger-YYYYMMDD.jsonl`。
fn day_file_path(dir: &Path, day: NaiveDate) -> PathBuf {
    dir.join(format!("ledger-{}.jsonl", day.format("%Y%m%d")))
}

/// 扫描目录中的台账日文件（解析文件名成 (day, path)，按日升序；不认识的
/// 文件名忽略——只管理自有命名面）。
fn day_files(dir: &Path) -> std::io::Result<Vec<(NaiveDate, PathBuf)>> {
    let mut days = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("ledger-") else {
            continue;
        };
        let Some(stem) = rest.strip_suffix(".jsonl") else {
            continue;
        };
        if stem.len() == 8 && stem.chars().all(|c| c.is_ascii_digit()) {
            if let Some(day) = NaiveDate::from_ymd_opt(
                stem[..4].parse().unwrap_or(0),
                stem[4..6].parse().unwrap_or(0),
                stem[6..8].parse().unwrap_or(0),
            ) {
                days.push((day, entry.path()));
            }
        }
    }
    days.sort_by_key(|(day, _)| *day);
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MaskingLedgerSection;

    /// 测试构造：独立目录 + 指定台账配置。
    fn ledger_with(section: MaskingLedgerSection, dir: &Path) -> MaskingLedger {
        let mut config = crate::config::parse_config_content(
            Path::new("ledger-test.toml"),
            "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n[database]\npath = \":memory:\"\n",
        )
        .expect("样例配置解析");
        config.masking.ledger = section;
        MaskingLedger::with_dir(
            Some(dir.to_path_buf()),
            Arc::new(crate::config::ConfigManager::new(config)),
        )
    }

    fn record(kind: &str, value: &str) -> LedgerRecord {
        LedgerRecord {
            kind: kind.to_string(),
            finder: "openai_sk".to_string(),
            placeholder: crate::masking::placeholder_for(
                crate::masking::MaskKind::parse(kind).expect("kind"),
                value,
            ),
            first: value.chars().next().expect("non-empty"),
            len: value.len(),
        }
    }

    #[test]
    fn disabled_by_default_zero_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ledger_with(MaskingLedgerSection::default(), dir.path());
        assert!(!ledger.enabled());
        ledger.record_with_now(&[record("api_key", "sk-test-value")], Utc::now());
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("dir").count(),
            0,
            "默认关必须零落盘"
        );
    }

    #[test]
    fn writes_jsonl_per_day_without_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ledger_with(
            MaskingLedgerSection {
                enabled: true,
                ..Default::default()
            },
            dir.path(),
        );
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        ledger.record_with_now(&[record("api_key", secret)], Utc::now());
        let file = dir
            .path()
            .join(format!("ledger-{}.jsonl", Utc::now().format("%Y%m%d")));
        assert!(file.exists(), "日文件必须按名创建");
        let content = std::fs::read_to_string(&file).expect("read");
        assert!(!content.contains(secret), "原始值不得落盘: {content}");
        let line: serde_json::Value = serde_json::from_str(content.trim()).expect("jsonl");
        assert_eq!(line["kind"], "api_key");
        assert_eq!(line["finder"], "openai_sk");
        assert_eq!(line["hint"]["first"], "s");
        assert_eq!(line["hint"]["len"], secret.len());
        assert!(line["placeholder"]
            .as_str()
            .expect("placeholder")
            .starts_with("{{FLUX_APIKEY_"));
        // 读取面回读同一条
        let read_back = ledger.read_day(Utc::now().date_naive()).expect("read day");
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0]["hint"]["len"], secret.len());
    }

    #[test]
    fn retention_trims_expired_day_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ledger_with(
            MaskingLedgerSection {
                enabled: true,
                retention_days: 3,
                ..Default::default()
            },
            dir.path(),
        );
        let now = Utc::now();
        for days_ago in [10i64, 5, 1] {
            let day = now.date_naive() - chrono::Duration::days(days_ago);
            std::fs::create_dir_all(dir.path()).expect("dir");
            std::fs::write(day_file_path(dir.path(), day), "{\"kind\":\"api_key\"}\n")
                .expect("seed old file");
        }
        // 首写触发裁剪：10 天前与 5 天前超 3 天窗删除，1 天前保留
        ledger.record_with_now(&[record("api_key", "sk-x")], now);
        let remaining: Vec<_> = day_files(dir.path())
            .expect("list")
            .into_iter()
            .map(|(day, _)| day)
            .collect();
        assert_eq!(
            remaining,
            vec![
                now.date_naive() - chrono::Duration::days(1),
                now.date_naive()
            ],
            "超期日文件必须裁剪，窗口内保留"
        );
    }

    #[test]
    fn total_bytes_trims_oldest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ledger_with(
            MaskingLedgerSection {
                enabled: true,
                retention_days: 30,
                max_total_bytes: 4096,
            },
            dir.path(),
        );
        let now = Utc::now();
        for days_ago in [2i64, 1] {
            let day = now.date_naive() - chrono::Duration::days(days_ago);
            std::fs::create_dir_all(dir.path()).expect("dir");
            std::fs::write(day_file_path(dir.path(), day), format!("{:0<3000}\n", "{}"))
                .expect("seed big file");
        }
        // 两文件合计 ~6002B 超 4096B：最旧（2 天前）删除即回落限内，
        // 1 天前文件与当日新写文件保留
        ledger.record_with_now(&[record("api_key", "sk-x")], now);
        let remaining: Vec<_> = day_files(dir.path())
            .expect("list")
            .into_iter()
            .map(|(day, _)| day)
            .collect();
        assert_eq!(
            remaining,
            vec![
                now.date_naive() - chrono::Duration::days(1),
                now.date_naive()
            ]
        );
    }

    /// 同日多次写不重复触发裁剪重开；跨日重开新文件。
    #[test]
    fn same_day_reuses_handle_new_day_rotates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ledger_with(
            MaskingLedgerSection {
                enabled: true,
                ..Default::default()
            },
            dir.path(),
        );
        let now = Utc::now();
        ledger.record_with_now(&[record("api_key", "sk-a")], now);
        ledger.record_with_now(&[record("token", "tok-b")], now);
        let tomorrow = now + chrono::Duration::days(1);
        ledger.record_with_now(&[record("email", "a@b.co")], tomorrow);
        let lines_today = ledger.read_day(now.date_naive()).expect("read");
        let lines_tomorrow = ledger.read_day(tomorrow.date_naive()).expect("read");
        assert_eq!(lines_today.len(), 2, "同日两条合并一文件");
        assert_eq!(lines_tomorrow.len(), 1, "跨日旋转新文件");
        assert_eq!(lines_today[1]["kind"], "token");
    }
}
