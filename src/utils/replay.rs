// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 离线成本回放引擎（`cost-replay` feature）：按时间窗读 `request_logs` 逐请求
//! 历史，对场景（affinity 缓存保 warm / cache_ttl 档位）重放 counterfactual 成本。
//!
//! 纯显式代码（规则 7）：无随机、无时钟依赖（时间全取日志 `timestamp`），
//! 同一输入同一输出。只读——不开服务、不写库；不调模型。
//!
//! 计价正源是 `[[model_catalog]]` 四档单价（每 1M tokens，币种随条目），
//! 单位换算与 `catalog-sync` 的 [`crate::config::unit`] 单源。

use crate::config::model_catalog::ModelCatalogEntry;
use crate::config::unit::{source_price_to_catalog_price, MICROS_PER_UNIT};
use crate::error::FluxgateError;
use dbnexus::DbPool;
use sea_orm::{ConnectionTrait, QueryResult, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// TTL 扫描档位（秒）——`cache_ttl` 场景的三档对比。
pub const TTL_SWEEP: [i64; 3] = [300, 900, 3600];

/// 报告尾部固定披露的三条假设（口径透明，避免误读）。
pub const ASSUMPTIONS: [&str; 3] = [
    "NULL-session rows have no cache benefit (each such row is its own session).",
    "Upstream cache behavior is approximated by the TTL window (same session + same vendor within TTL).",
    "Prices use the current [[model_catalog]]; historical unit prices are not recoverable.",
];

/// 回放行——`request_logs` 的时间窗投影（装载与回放共用的中间契约）。
///
/// `session_key` 为 `None` 表示该请求无会话语义：回放时**每行自成会话**
///（见 [`session_key_of`]），历史无缓存收益。
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayRow {
    /// Unix 秒（`request_logs.timestamp`）。
    pub ts: i64,
    pub session_key: Option<String>,
    pub vendor_id: String,
    pub model_id: String,
    /// 输入 token（`prompt_tokens`，四档计价中的 input 档基数）。
    pub prompt_tokens: u64,
    /// 上游报告的缓存读 token。
    pub cache_read: u64,
    /// 上游报告的缓存写 token。
    pub cache_write: u64,
    pub completion_tokens: u64,
    /// 落库时的逐请求成本（USD 道）；空表示未计价（无 usage / 未登记），
    /// 基线改按目录现价重算。
    pub logged_cost_usd: Option<f64>,
}

/// 会话身份——回放分组的键。
///
/// `Explicit` 是日志记录的 `session_key`；`Isolated` 是 NULL 会话行按序号的
/// 独立身份（`ordinal` 取该行在装载结果中的下标），保证 NULL 行之间不会互认
/// 缓存命中。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionKey {
    Explicit(String),
    Isolated(u64),
}

/// 行的会话身份：有 `session_key` 取其值；NULL 按 `ordinal` 独立。
pub fn session_key_of(row: &ReplayRow, ordinal: u64) -> SessionKey {
    match &row.session_key {
        Some(key) => SessionKey::Explicit(key.clone()),
        None => SessionKey::Isolated(ordinal),
    }
}

/// 打开只读回放连接池（**不建库、不建表、不跑迁移**）。
///
/// 与 `Database::new` 的差异在这一点：回放对象是既有库，工具不产生任何
/// 写操作（design：不开服务、不写库）。调用方给出完整 URL
/// （`sqlite://…`、`postgres://…`、`mysql://…`）。
pub async fn open_pool(url: &str) -> Result<DbPool, FluxgateError> {
    let config = dbnexus::DbConfig {
        url: url.to_string(),
        pool_config: dbnexus::PoolConfig {
            max_connections: 1,
            min_connections: 1,
            ..Default::default()
        },
        admin_role: "admin".to_string(),
        ..Default::default()
    };
    Ok(DbPool::with_config(config).await?)
}

/// 回放装载 SQL（只读 SELECT）。
///
/// 占位符统一 `?`，执行前经 [`crate::database::raw_statement`] 按连接后端方言化
/// （postgres → `$n`；sqlite/mysql 原样）。列名与类型在 sqlite/postgres/mysql
/// 三方言下一致（`usage-observability` V5 迁移后的四列），故单条 SQL 三端通用。
const LOAD_ROWS_SQL: &str = "SELECT timestamp, session_key, actual_vendor, actual_model, \
     prompt_tokens, cache_read_tokens, cache_write_tokens, completion_tokens, cost_usd \
     FROM request_logs WHERE timestamp >= ? AND timestamp <= ? \
     ORDER BY timestamp ASC, id ASC";

/// 装载时间窗内的 `request_logs` 行（按 `timestamp` 升序；同刻按 `id` 稳定）。
///
/// 只读 SELECT，占位符统一 `?` 并经 [`crate::database::raw_statement`] 按连接
/// 后端方言化（postgres → `$n`；sqlite/mysql 原样）；列名与类型在
/// sqlite/postgres/mysql 三方言下一致（`usage-observability` V5 迁移后的四列）。
pub async fn load_rows(pool: &DbPool, from: i64, to: i64) -> Result<Vec<ReplayRow>, FluxgateError> {
    let session = pool.get_session("admin").await?;
    let conn = session.connection()?;
    let stmt = crate::database::raw_statement(
        conn.get_database_backend(),
        LOAD_ROWS_SQL,
        vec![Value::BigInt(Some(from)), Value::BigInt(Some(to))],
    );
    let rows = conn
        .query_all_raw(stmt)
        .await
        .map_err(dbnexus::DbError::from)?;
    Ok(rows.iter().map(map_replay_row).collect())
}

fn map_replay_row(row: &QueryResult) -> ReplayRow {
    let u64_of = |col: &str| -> u64 {
        row.try_get::<Option<i64>>("", col)
            .ok()
            .flatten()
            .unwrap_or(0)
            .max(0) as u64
    };
    ReplayRow {
        ts: row.try_get("", "timestamp").unwrap_or_default(),
        session_key: row
            .try_get::<Option<String>>("", "session_key")
            .unwrap_or(None),
        vendor_id: row.try_get("", "actual_vendor").unwrap_or_default(),
        model_id: row.try_get("", "actual_model").unwrap_or_default(),
        prompt_tokens: u64_of("prompt_tokens"),
        cache_read: u64_of("cache_read_tokens"),
        cache_write: u64_of("cache_write_tokens"),
        completion_tokens: u64_of("completion_tokens"),
        logged_cost_usd: row.try_get::<Option<f64>>("", "cost_usd").unwrap_or(None),
    }
}

/// 四档价（currency / 1M tokens）→ 该 token 数的微单位金额（10⁻⁶ currency）。
///
/// `catalog-sync` 的 [`crate::config::unit`] 恒等映射：`3.0 USD/1M tokens ≡
/// 3.0 µ$/token`，故 `tokens × price` 即微单位总额——换算经 [`MICROS_PER_UNIT`]
/// 显式表达，避免与单源漂移。
fn tier_micro(tokens: u64, price_per_million: f64) -> i64 {
    let price_per_token = source_price_to_catalog_price(price_per_million) / MICROS_PER_UNIT;
    (tokens as f64 * price_per_token * MICROS_PER_UNIT).round() as i64
}

/// USD → 微单位（10⁻⁶ USD），四舍五入（确定性）。
fn usd_to_micro(usd: f64) -> i64 {
    (usd * MICROS_PER_UNIT).round() as i64
}

/// 单请求计价结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricedRequest {
    /// 微单位金额（10⁻⁶ currency）。
    pub micro_usd: i64,
    /// 模型未登记在目录：已回退（按 logged 成本或 0），需在报告中计数披露。
    pub uncataloged: bool,
}

/// 单请求计价（显式实现，规则 7）。
///
/// - 命中（`cache_hit`）：input 按 `cache_read_price` 计、写按
///   `cache_creation_price` 计、出按 `output_price` 计；
/// - 未命中：input 按 `input_price` 计、出按 `output_price` 计（全价）。
/// - 目录缺模型：回退到该行 logged 成本（无则 0），并置 `uncataloged=true`。
pub fn price(row: &ReplayRow, catalog: &[ModelCatalogEntry], cache_hit: bool) -> PricedRequest {
    let Some(entry) = lookup_pricing_entry(catalog, &row.model_id) else {
        return PricedRequest {
            micro_usd: row.logged_cost_usd.map(usd_to_micro).unwrap_or(0),
            uncataloged: true,
        };
    };
    let micro_usd = if cache_hit {
        tier_micro(row.prompt_tokens, entry.cache_read_price)
            + tier_micro(row.cache_write, entry.cache_creation_price)
            + tier_micro(row.completion_tokens, entry.output_price)
    } else {
        tier_micro(row.prompt_tokens, entry.input_price)
            + tier_micro(row.completion_tokens, entry.output_price)
    };
    PricedRequest {
        micro_usd,
        uncataloged: false,
    }
}

/// 目录精确命中 → 短名前缀回退（多命中取字典序最小，确定性）。
/// 与 `vendor::cost::lookup_pricing_entry` 同语义（计价查找的裁决域）。
fn lookup_pricing_entry<'a>(
    catalog: &'a [ModelCatalogEntry],
    model_id: &str,
) -> Option<&'a ModelCatalogEntry> {
    if let Some(entry) = catalog.iter().find(|e| e.model_id == model_id) {
        return Some(entry);
    }
    catalog
        .iter()
        .filter(|e| e.model_id.starts_with(model_id))
        .min_by(|a, b| a.model_id.cmp(&b.model_id))
}

/// 单场景成本（`affinity`/`cache_ttl` 的一档或基线）。
///
/// 金额以微单位（10⁻⁶ currency）累加，渲染时才转 USD——整数累计保证
/// 同输入逐位可复现（规则 9 的确定性断言）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScenarioCost {
    /// 场景名（`baseline` / `legacy` / `auto-<ttl>s`）。
    pub name: String,
    pub total_micro_usd: i64,
    pub requests: u64,
    pub sessions: u64,
    pub cache_hits: u64,
    /// 目录缺模型而回退计价的行数。
    pub uncataloged: u64,
    /// 仅基线：无 logged 成本、改按现价重算的行数（口径标注）。
    pub recomputed: u64,
    /// 按 vendor 的微单位总额（键升序，确定性）。
    pub by_vendor: Vec<(String, i64)>,
    /// 按 model 的微单位总额（键升序，确定性）。
    pub by_model: Vec<(String, i64)>,
}

#[derive(Default)]
struct Acc {
    total: i64,
    requests: u64,
    cache_hits: u64,
    uncataloged: u64,
    recomputed: u64,
    sessions: HashSet<SessionKey>,
    by_vendor: BTreeMap<String, i64>,
    by_model: BTreeMap<String, i64>,
}

impl Acc {
    fn add(&mut self, row: &ReplayRow, ordinal: u64, micro: i64, hit: bool) {
        self.total += micro;
        self.requests += 1;
        if hit {
            self.cache_hits += 1;
        }
        self.sessions.insert(session_key_of(row, ordinal));
        *self.by_vendor.entry(row.vendor_id.clone()).or_insert(0) += micro;
        *self.by_model.entry(row.model_id.clone()).or_insert(0) += micro;
    }

    fn finish(self, name: impl Into<String>) -> ScenarioCost {
        ScenarioCost {
            name: name.into(),
            total_micro_usd: self.total,
            requests: self.requests,
            sessions: self.sessions.len() as u64,
            cache_hits: self.cache_hits,
            uncataloged: self.uncataloged,
            recomputed: self.recomputed,
            by_vendor: self.by_vendor.into_iter().collect(),
            by_model: self.by_model.into_iter().collect(),
        }
    }
}

/// 基线（as-logged）：`cost_usd` 非空直接用；空则按目录现价（未命中口径）重算，
/// 并计数 `recomputed`（报告据此标注口径）。
pub fn baseline(rows: &[ReplayRow], catalog: &[ModelCatalogEntry]) -> ScenarioCost {
    let mut acc = Acc::default();
    for (i, row) in rows.iter().enumerate() {
        let micro = match row.logged_cost_usd {
            Some(usd) => usd_to_micro(usd),
            None => {
                acc.recomputed += 1;
                let priced = price(row, catalog, false);
                if priced.uncataloged {
                    acc.uncataloged += 1;
                }
                priced.micro_usd
            }
        };
        acc.add(row, i as u64, micro, false);
    }
    acc.finish("baseline")
}

/// affinity 场景回放：逐会话维护 `last_vendor/last_ts`，同 vendor 且间隔
/// `< ttl_secs` 的请求按缓存命中计价（input 走 `cache_read_price`），其余全价。
///
/// `ttl_secs == 0` 时任何间隔都不命中——即 legacy（现状，无缓存收益假设）。
pub fn replay_affinity(
    rows: &[ReplayRow],
    catalog: &[ModelCatalogEntry],
    ttl_secs: i64,
) -> ScenarioCost {
    let mut acc = Acc::default();
    let mut last: HashMap<SessionKey, (String, i64)> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        let ordinal = i as u64;
        let key = session_key_of(row, ordinal);
        let hit = match last.get(&key) {
            Some((vendor, ts)) => {
                ttl_secs > 0 && *vendor == row.vendor_id && (row.ts - *ts) < ttl_secs
            }
            None => false,
        };
        let priced = price(row, catalog, hit);
        if priced.uncataloged {
            acc.uncataloged += 1;
        }
        acc.add(row, ordinal, priced.micro_usd, hit);
        last.insert(key, (row.vendor_id.clone(), row.ts));
    }
    let name = if ttl_secs > 0 {
        format!("auto-{ttl_secs}s")
    } else {
        "legacy".to_string()
    };
    acc.finish(name)
}

/// affinity 场景全集：legacy（无缓存收益）+ [`TTL_SWEEP`] 三档 auto。
pub fn affinity_scenarios(rows: &[ReplayRow], catalog: &[ModelCatalogEntry]) -> Vec<ScenarioCost> {
    let mut out = vec![replay_affinity(rows, catalog, 0)];
    out.extend(
        TTL_SWEEP
            .iter()
            .map(|ttl| replay_affinity(rows, catalog, *ttl)),
    );
    out
}

/// 回放报告：窗口、基线、场景集与固定假设披露。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReplayReport {
    pub from: i64,
    pub to: i64,
    pub requests: u64,
    pub baseline: ScenarioCost,
    pub scenarios: Vec<ScenarioCost>,
}

/// 组装报告（确定性：无时钟、无随机，仅由入参推导）。
pub fn build_report(
    rows: &[ReplayRow],
    catalog: &[ModelCatalogEntry],
    from: i64,
    to: i64,
) -> ReplayReport {
    ReplayReport {
        from,
        to,
        requests: rows.len() as u64,
        baseline: baseline(rows, catalog),
        scenarios: affinity_scenarios(rows, catalog),
    }
}

fn usd(micro: i64) -> String {
    format!("{:.6}", micro as f64 / MICROS_PER_UNIT)
}

fn hit_rate(hits: u64, requests: u64) -> String {
    if requests == 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", hits as f64 / requests as f64 * 100.0)
    }
}

impl ReplayReport {
    /// Markdown 渲染：场景汇总表 + per-vendor/model 分解 + 固定三假设披露。
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Cost replay report\n\n");
        out.push_str(&format!(
            "window: {}..{} ({} requests)\n\n",
            self.from, self.to, self.requests
        ));

        out.push_str(
            "| scenario | total (USD) | diff vs baseline (USD) | sessions | requests | cache hits | hit rate | uncataloged |\n",
        );
        out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
        let base = self.baseline.total_micro_usd;
        for s in std::iter::once(&self.baseline).chain(self.scenarios.iter()) {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                s.name,
                usd(s.total_micro_usd),
                usd(s.total_micro_usd - base),
                s.sessions,
                s.requests,
                s.cache_hits,
                hit_rate(s.cache_hits, s.requests),
                s.uncataloged,
            ));
        }

        out.push_str("\n## by vendor\n\n| scenario | vendor | USD |\n| --- | --- | --- |\n");
        for s in std::iter::once(&self.baseline).chain(self.scenarios.iter()) {
            for (vendor, micro) in &s.by_vendor {
                out.push_str(&format!("| {} | {} | {} |\n", s.name, vendor, usd(*micro)));
            }
        }

        out.push_str("\n## by model\n\n| scenario | model | USD |\n| --- | --- | --- |\n");
        for s in std::iter::once(&self.baseline).chain(self.scenarios.iter()) {
            for (model, micro) in &s.by_model {
                out.push_str(&format!("| {} | {} | {} |\n", s.name, model, usd(*micro)));
            }
        }

        out.push_str("\n## assumptions\n\n");
        for a in ASSUMPTIONS {
            out.push_str(&format!("- {a}\n"));
        }

        if self.baseline.recomputed > 0 {
            out.push_str(&format!(
                "\n> baseline caliber: {} row(s) had no logged cost and were recomputed at current catalog prices.\n",
                self.baseline.recomputed
            ));
        }
        if self.baseline.uncataloged > 0 {
            out.push_str(&format!(
                "\n> {} row(s) referenced models absent from [[model_catalog]] and fell back to logged cost.\n",
                self.baseline.uncataloged
            ));
        }
        out
    }

    /// JSON 渲染：与 Markdown 同源数据结构，数值逐位一致。
    pub fn render_json(&self) -> String {
        let value = serde_json::json!({
            "window": { "from": self.from, "to": self.to, "requests": self.requests },
            "baseline": serde_json::to_value(&self.baseline).unwrap_or(serde_json::Value::Null),
            "scenarios": serde_json::to_value(&self.scenarios).unwrap_or(serde_json::Value::Null),
            "assumptions": ASSUMPTIONS,
            "baseline_recomputed_rows": self.baseline.recomputed,
            "baseline_uncataloged_rows": self.baseline.uncataloged,
        });
        serde_json::to_string_pretty(&value).unwrap_or_default()
    }
}

#[cfg(all(test, feature = "cost-replay"))]
mod tests {
    use super::*;

    /// 目录条目构造（四档价，每 1M tokens）。
    fn entry(model_id: &str, input: f64, output: f64, read: f64, write: f64) -> ModelCatalogEntry {
        ModelCatalogEntry {
            model_id: model_id.to_string(),
            context_window: 200_000,
            rerank_price_per_1k_docs: 0.0,
            input_price: input,
            output_price: output,
            cache_read_price: read,
            cache_creation_price: write,
            long_context_input_price: 0.0,
            long_context_threshold_tokens: 0,
            currency: crate::config::PricingCurrency::Usd,
            max_output_tokens: None,
            supports_tools: false,
            supports_thinking: false,
            cache_breakpoints: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        ts: i64,
        session: Option<&str>,
        vendor: &str,
        model: &str,
        prompt: u64,
        cache_read: u64,
        cache_write: u64,
        completion: u64,
    ) -> ReplayRow {
        ReplayRow {
            ts,
            session_key: session.map(str::to_string),
            vendor_id: vendor.to_string(),
            model_id: model.to_string(),
            prompt_tokens: prompt,
            cache_read,
            cache_write,
            completion_tokens: completion,
            logged_cost_usd: None,
        }
    }

    /// claude-sonnet 四档：input 3.0、output 15.0、cache_read 0.3、cache_write 3.75
    /// （每 1M tokens）。
    fn sonnet() -> ModelCatalogEntry {
        entry("claude-sonnet-4", 3.0, 15.0, 0.3, 3.75)
    }

    /// 命中：input 走 cache_read 价 + 写走 cache_write 价 + 出走 output 价；
    /// 未命中：input+output 全价；缺模型回退 logged 成本并计数 uncataloged。
    #[test]
    fn cache_hit_prices_read_tier_and_miss_prices_full() {
        let catalog = vec![sonnet()];
        // 1M prompt + 1M completion，1M cache_write
        let r = row(
            0,
            Some("s"),
            "v",
            "claude-sonnet-4",
            1_000_000,
            0,
            1_000_000,
            1_000_000,
        );

        // 未命中：3.0 (input) + 15.0 (output) = 18.0 USD → 18_000_000 µ$
        let miss = price(&r, &catalog, false);
        assert_eq!(miss.micro_usd, 18_000_000, "未命中=input+output 全价");
        assert!(!miss.uncataloged);

        // 命中：0.3 (read) + 3.75 (write) + 15.0 (output) = 19.05 USD → 19_050_000 µ$
        let hit = price(&r, &catalog, true);
        assert_eq!(
            hit.micro_usd, 19_050_000,
            "命中=cache_read+cache_write+output 价"
        );
        assert!(!hit.uncataloged);

        // 缺模型回退：无 logged → 0，且计数
        let mut r2 = row(0, Some("s"), "v", "unregistered-model", 1_000_000, 0, 0, 0);
        r2.logged_cost_usd = Some(0.5);
        let fb = price(&r2, &catalog, false);
        assert!(fb.uncataloged, "未登记模型必须计数 uncataloged");
        assert_eq!(fb.micro_usd, 500_000, "回退到 logged 成本");

        // 无 logged 且未登记 → 0（不伪装免费为其它值）
        let r3 = row(0, Some("s"), "v", "unregistered-model", 1_000_000, 0, 0, 0);
        assert_eq!(price(&r3, &catalog, false).micro_usd, 0);
    }

    /// affinity：间隔 < TTL 计缓存价、超窗全价、换 vendor 全价。
    #[test]
    fn same_vendor_within_ttl_gets_cache_price() {
        let catalog = vec![sonnet()];
        // 同会话同 vendor：t=0 与 t=100（窗内）、t=2000（超 300s 窗）、
        // t=2100 换 vendor（不同 vendor 不命中）
        let rows = vec![
            row(0, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(100, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(2000, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(2100, Some("s"), "v2", "claude-sonnet-4", 1_000_000, 0, 0, 0),
        ];
        let c = replay_affinity(&rows, &catalog, 300);
        // 命中仅第 2 行：0.3 (read) → 300_000 µ$
        // 其余三行全价 3.0 → 3_000_000 µ$ 各
        assert_eq!(c.cache_hits, 1, "仅窗内同 vendor 一行命中");
        assert_eq!(c.total_micro_usd, 300_000 + 3 * 3_000_000);
        assert_eq!(c.name, "auto-300s");

        // ttl=0：legacy，无任何命中
        let legacy = replay_affinity(&rows, &catalog, 0);
        assert_eq!(legacy.cache_hits, 0);
        assert_eq!(legacy.total_micro_usd, 4 * 3_000_000);
        assert_eq!(legacy.name, "legacy");
    }

    /// NULL 会话行各自独立：即便同 vendor、时间相邻也不命中。
    #[test]
    fn null_session_rows_never_hit() {
        let catalog = vec![sonnet()];
        let rows = vec![
            row(0, None, "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(10, None, "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(20, None, "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
        ];
        let c = replay_affinity(&rows, &catalog, 3600);
        assert_eq!(c.cache_hits, 0, "NULL 会话无缓存收益");
        assert_eq!(c.total_micro_usd, 3 * 3_000_000);
        assert_eq!(c.sessions, 3, "每行自成会话");
    }

    /// TTL 扫描 {300,900,3600}：档位序列固定、alfinity 越宽命中越多（总额单调不增）。
    #[test]
    fn ttl_sweep_reports_monotonic_sessions() {
        let catalog = vec![sonnet()];
        // 同会话同 vendor：间隔分别 200（三档全命中）、500（仅 900/3600 命中）、
        // 1000（仅 3600 命中）
        let rows = vec![
            row(0, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(200, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(700, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(1700, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
        ];
        let scenarios = affinity_scenarios(&rows, &catalog);
        assert_eq!(
            scenarios
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["legacy", "auto-300s", "auto-900s", "auto-3600s"],
            "固定档位"
        );
        // 命中数：legacy 0 / 300 1 / 900 2 / 3600 3
        let hits: Vec<u64> = scenarios.iter().map(|s| s.cache_hits).collect();
        assert_eq!(hits, vec![0, 1, 2, 3]);
        // 总额单调不增（命中越多越省）
        let totals: Vec<i64> = scenarios.iter().map(|s| s.total_micro_usd).collect();
        assert!(
            totals.windows(2).all(|w| w[0] >= w[1]),
            "总额单调不增: {totals:?}"
        );
        // 每档会话仍为 1（同一会话键）
        assert!(scenarios.iter().all(|s| s.sessions == 1));
    }

    /// 基线优先 logged 成本；空则现价重算并计数 recomputed（报告标注口径）。
    #[test]
    fn baseline_prefers_logged_cost_with_fallback_note() {
        let catalog = vec![sonnet()];
        let mut r1 = row(0, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0);
        r1.logged_cost_usd = Some(7.0); // 直接用 logged（7_000_000 µ$）
        let r2 = row(10, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0); // 无 logged → 重算 3.0

        let rows = vec![r1, r2];
        let b = baseline(&rows, &catalog);
        assert_eq!(b.name, "baseline");
        assert_eq!(b.total_micro_usd, 7_000_000 + 3_000_000);
        assert_eq!(b.recomputed, 1, "1 行无 logged 成本");

        let report = build_report(&rows, &catalog, 0, 10);
        let md = report.render_markdown();
        assert!(
            md.contains("baseline caliber: 1 row(s) had no logged cost"),
            "报告必须标注重算口径: {md}"
        );
    }

    /// 报告快照：Markdown 汇总/分解/假设披露 + JSON 同源数值一致。
    #[test]
    fn report_render_snapshot() {
        let catalog = vec![sonnet()];
        let rows = vec![
            row(0, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(100, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
        ];
        let report = build_report(&rows, &catalog, 0, 100);
        let md = report.render_markdown();
        assert!(md.starts_with("# Cost replay report\n"));
        assert!(md.contains("window: 0..100 (2 requests)"));
        assert!(md.contains("| baseline | 6.000000 | 0.000000 | 1 | 2 | 0 | 0.0% | 0 |"));
        assert!(md.contains("| auto-300s | 3.300000 | -2.700000 | 1 | 2 | 1 | 50.0% | 0 |"));
        assert!(md.contains("## by vendor"));
        assert!(md.contains("| baseline | v1 | 6.000000 |"));
        assert!(md.contains("## by model"));
        assert!(md.contains("| auto-300s | claude-sonnet-4 | 3.300000 |"));
        for a in ASSUMPTIONS {
            assert!(md.contains(a), "固定披露缺失: {a}");
        }

        // JSON 与 Markdown 同源数值一致
        let json: serde_json::Value = serde_json::from_str(&report.render_json()).unwrap();
        assert_eq!(json["baseline"]["total_micro_usd"], 6_000_000);
        assert_eq!(json["scenarios"][1]["total_micro_usd"], 3_300_000);
        assert_eq!(json["window"]["requests"], 2);
    }

    /// 装载 SQL 的三方言占位符投影：postgres 翻 `$n`，sqlite/mysql 保留 `?`；
    /// 列集在单条 SQL 内不落方言专属语法（同一 DML 三端通用）。
    #[test]
    fn load_sql_is_dialect_portable() {
        use sea_orm::{DatabaseBackend, Statement};
        let values = vec![Value::BigInt(Some(1)), Value::BigInt(Some(2))];
        let pg: Statement = crate::database::raw_statement(
            DatabaseBackend::Postgres,
            LOAD_ROWS_SQL,
            values.clone(),
        );
        assert!(pg.sql.contains("$1") && pg.sql.contains("$2"), "{}", pg.sql);
        assert!(!pg.sql.contains('?'), "postgres 不得残留 `?`: {}", pg.sql);

        for backend in [DatabaseBackend::Sqlite, DatabaseBackend::MySql] {
            let st: Statement =
                crate::database::raw_statement(backend, LOAD_ROWS_SQL, values.clone());
            assert_eq!(st.sql.matches('?').count(), 2, "{backend:?} 保留两个 `?`");
        }
        // 全列存在于 SELECT 列表（三方言 schema 同列）
        for col in [
            "timestamp",
            "session_key",
            "actual_vendor",
            "actual_model",
            "prompt_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "completion_tokens",
            "cost_usd",
        ] {
            assert!(LOAD_ROWS_SQL.contains(col), "缺少列 {col}");
        }
    }

    /// 确定性：同输入两跑 byte-equal（无随机无时钟）。
    #[test]
    fn render_is_byte_equal_across_runs() {
        let catalog = vec![sonnet()];
        let rows = vec![
            row(0, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
            row(100, Some("s"), "v1", "claude-sonnet-4", 1_000_000, 0, 0, 0),
        ];
        let a = build_report(&rows, &catalog, 0, 100);
        let b = build_report(&rows, &catalog, 0, 100);
        assert_eq!(a, b);
        assert_eq!(a.render_markdown(), b.render_markdown());
        assert_eq!(a.render_json(), b.render_json());
    }

    #[cfg(feature = "db-sqlite")]
    mod loader {
        use super::*;
        use crate::database::{Database, RequestLog};

        #[allow(clippy::too_many_arguments)]
        fn log(
            request_id: &str,
            ts: i64,
            session: Option<&str>,
            vendor: &str,
            model: &str,
            prompt: i32,
            completion: i32,
            cache_read: Option<i64>,
            cache_write: Option<i64>,
            cost: Option<f64>,
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
                cost_usd: cost,
                session_key: session.map(str::to_string),
                context_window_tokens: None,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
            }
        }

        async fn fixture() -> Database {
            Database::new("sqlite::memory:", 1)
                .await
                .expect("in-memory sqlite fixture")
        }

        /// 窗内行按时间升序读入、窗外行排除；NULL 会话每行自成会话（身份不相等）。
        #[tokio::test]
        async fn loads_window_and_null_session_isolated() {
            let db = fixture().await;
            // 窗内：s1@100、NULL@200、NULL@300、s1@400；窗外：s1@5000
            for (rid, ts, session) in [
                ("r1", 100, Some("s1")),
                ("r2", 200, None),
                ("r3", 300, None),
                ("r4", 400, Some("s1")),
                ("r5", 5000, Some("s1")),
            ] {
                db.repository()
                    .log_request(&log(
                        rid,
                        ts,
                        session,
                        "v1",
                        "m1",
                        1000,
                        100,
                        Some(0),
                        Some(0),
                        Some(0.1),
                    ))
                    .await
                    .expect("seed row");
            }

            let rows = load_rows(&db.pool, 100, 400).await.expect("load window");
            assert_eq!(rows.len(), 4, "窗口过滤：仅 100..=400 的四行");
            assert_eq!(
                rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
                vec![100, 200, 300, 400],
                "按 timestamp 升序"
            );
            assert_eq!(rows[0].session_key.as_deref(), Some("s1"));
            assert!(rows[1].session_key.is_none(), "NULL 会话原样保留");

            // NULL 会话各行自成会话：同 vendor/时间相邻也不互认
            let null_keys: Vec<SessionKey> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.session_key.is_none())
                .map(|(i, r)| session_key_of(r, i as u64))
                .collect();
            assert_eq!(null_keys.len(), 2);
            assert_ne!(null_keys[0], null_keys[1], "NULL 会话必须互相隔离");
            // 显式会话键归一到同一身份
            assert_eq!(
                session_key_of(&rows[0], 0),
                session_key_of(&rows[3], 3),
                "同 session_key 必须归同一身份"
            );
        }
    }
}
