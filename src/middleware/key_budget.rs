// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 网关键成本预算账本（`[key_budgets]`，R-budget-001）：在飞预留-结算回冲。
//!
//! 入口（auth 识别 key 后、路由前）按估成本 `reserve` 预扣，响应后按实际
//! 用量 `settle` 回冲差额——与租户配额（`middleware::quota`）同一「预扣-
//! 回补」形态，但维度独立：预算按入站 key 摘要记账、窗口为 UTC 日历
//! （daily/monthly）。进程内 `std::sync::Mutex` 单锁承载（不引 dashmap：
//! 该依赖只在 cost-tracking 下可用，key_budgets 无 feature 门控）。
//!
//! 语义边界：
//! - 每个 Admitted 预留必须恰好结算一次；未结算预留占用余量直至窗翻转
//!   （与租户配额「断流不回补保持预扣」同方向：宁可保守不少记）。
//! - 结算跨窗（预留窗已翻转）：旧窗预留随窗消亡，实际用量记入当前窗。
//! - 记账进程内，重启归零（预算按窗口重算）；多副本各自记账（spec 披露）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::config::Config;
use crate::middleware::quota_period::Period;
use crate::quota_contract::{KeyBudgetReservation, KeyBudgetSource, KeyBudgetView, ReserveOutcome};

/// 在飞预留总量上限（高基数约束，与 [`crate::middleware::quota`] 的账本
/// 上限同级）：超限先清扫陈旧预留（窗已翻转者），仍超 → `Unavailable`
///（调用方按 fail_open 语义处置——fail-closed 方向不放松约束）。
const MAX_IN_FLIGHT_RESERVATIONS: usize = 10_000;

/// 单 (key, window) 账本格：窗起点（Unix 秒，UTC 日历对齐）+ 占用。
/// `reserved` 为在飞预留合计、`settled` 为已结算实际用量合计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowCell {
    start: i64,
    reserved: u64,
    settled: u64,
}

impl WindowCell {
    fn occupied(&self) -> u64 {
        self.reserved.saturating_add(self.settled)
    }
}

/// 在飞预留条目：预留发生时的逐窗起点（结算时判窗翻转）+ 预扣额。
#[derive(Debug, Clone)]
struct InFlight {
    key_hash: String,
    est: u64,
    /// (window 名, 预留时窗起点)——key 的每条规则窗各一条
    windows: Vec<(String, i64)>,
}

/// 账本内部态（单锁：reserve 的「判定+落账」必须原子）。
#[derive(Default)]
struct Inner {
    /// (key_hash, window 名) → 账本格
    windows: HashMap<(String, String), WindowCell>,
    /// 预留 id → 在飞条目
    in_flight: HashMap<u64, InFlight>,
    /// key_hash → 在飞预留 FIFO（中间件结算路径：无句柄按入序配对；
    /// 聚合守恒——每预留恰一结算时，配对错位不改变总量）
    fifo: HashMap<String, VecDeque<u64>>,
    /// key_hash → 完成 token 滑动均值（输出估成本用；缺席 = 冷启动）
    means: HashMap<String, u64>,
}

/// 网关键预算账本（经 [`crate::middleware::quota::QuotaState`] 承载，
/// 中间件与结算钩共享同一实例）。写侧端口 [`KeyBudgetSource`] 的实现体
/// 在本模块（契约定义在根级 `quota_contract`，与 vendor 预算同缝）。
pub struct KeyBudgetLedger {
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    /// 在飞预留容量上限（生产 = [`MAX_IN_FLIGHT_RESERVATIONS`]；测试可
    /// 收缩为 0 直接触发 Unavailable 分支）
    max_in_flight: usize,
    /// 可注入时钟（窗口翻转测试；生产 = Utc::now）
    now: Mutex<Box<dyn Fn() -> i64 + Send + Sync>>,
}

impl std::fmt::Debug for KeyBudgetLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("KeyBudgetLedger")
            .field("windows", &guard.windows.len())
            .field("in_flight", &guard.in_flight.len())
            .finish()
    }
}

impl Default for KeyBudgetLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// key 的生效预算窗解析单点：`[[key_budgets.rules]]` 中该 key 的全部条目 →
/// (窗名, UTC 日历周期, 限额)。校验层保证 window ∈ {daily, monthly} 且同
/// (hash, window) 唯一；此处 filter_map 容忍 Period 编译失败（防御）。
pub fn key_budget_windows(config: &Config, key_hash: &str) -> Vec<(String, Period, u64)> {
    config
        .key_budgets
        .rules
        .iter()
        .filter(|rule| rule.api_key_hash.trim() == key_hash)
        .filter_map(|rule| {
            Period::from_config("calendar", rule.window.trim(), 0).map(|period| {
                (
                    rule.window.trim().to_string(),
                    period,
                    rule.cost_limit_millicents,
                )
            })
        })
        .collect()
}

impl KeyBudgetLedger {
    pub fn new() -> Self {
        Self::with_now(Box::new(|| chrono::Utc::now().timestamp()))
    }

    /// 可注入时钟构造（窗口翻转测试用；生产走 [`Self::new`]）。
    pub fn with_now(now: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            next_id: AtomicU64::new(1),
            max_in_flight: MAX_IN_FLIGHT_RESERVATIONS,
            now: Mutex::new(now),
        }
    }

    /// 收缩在飞容量构造（fail-open/fail-closed 集成测试注入点：
    /// cap=0 → reserve 恒 Unavailable）。
    pub fn with_capacity(max_in_flight: usize) -> Self {
        Self {
            max_in_flight,
            ..Self::new()
        }
    }

    fn now_secs(&self) -> i64 {
        let now = self.now.lock().unwrap_or_else(|p| p.into_inner());
        (now)()
    }

    /// 预留（入口判定+落账单临界区原子）：任一规则窗余量不足即拒（拒绝
    /// 路径零落账）；全部通过才逐窗落预留并入飞。容量超限先清扫陈旧
    ///（窗已翻转）预留，仍超 → Unavailable。
    pub fn reserve(&self, config: &Config, key_hash: &str, est_millicents: u64) -> ReserveOutcome {
        let rules = key_budget_windows(config, key_hash);
        if rules.is_empty() {
            return ReserveOutcome::NoBudget;
        }
        let now = self.now_secs();
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if guard.in_flight.len() >= self.max_in_flight {
            sweep_stale(&mut guard);
            if guard.in_flight.len() >= self.max_in_flight {
                return ReserveOutcome::Unavailable;
            }
        }
        // 第一遍：翻转收敛 + 逐窗判定
        let mut committed: Vec<(String, i64)> = Vec::with_capacity(rules.len());
        for (name, period, limit) in &rules {
            let start = period.window_start(now, 0).unwrap_or(now);
            let cell = guard
                .windows
                .entry((key_hash.to_string(), name.clone()))
                .or_insert(WindowCell {
                    start,
                    reserved: 0,
                    settled: 0,
                });
            if cell.start != start {
                cell.start = start;
                cell.reserved = 0;
                cell.settled = 0;
            }
            let remaining = limit.saturating_sub(cell.occupied());
            if est_millicents > remaining {
                let secs = (period.reset_at(start, 0).saturating_sub(now)).max(1) as u64;
                return ReserveOutcome::Exceeded {
                    retry_after_secs: secs,
                };
            }
            committed.push((name.clone(), start));
        }
        // 第二遍：统一落预留（同一临界区内，判定与写入原子）
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        for (name, start) in &committed {
            if let Some(cell) = guard.windows.get_mut(&(key_hash.to_string(), name.clone())) {
                if cell.start == *start {
                    cell.reserved = cell.reserved.saturating_add(est_millicents);
                }
            }
        }
        guard.in_flight.insert(
            id,
            InFlight {
                key_hash: key_hash.to_string(),
                est: est_millicents,
                windows: committed,
            },
        );
        guard
            .fifo
            .entry(key_hash.to_string())
            .or_default()
            .push_back(id);
        ReserveOutcome::Admitted(KeyBudgetReservation {
            id,
            key_hash: key_hash.to_string(),
            est_millicents,
        })
    }

    /// 结算核心（回冲差额）：`reservation` 为 Some 时先按句柄回冲在飞
    ///（窗翻转的预留不回冲旧窗——随窗消亡），实际用量记入该 key 的当前
    /// 各窗；None 时按 FIFO 弹最旧在飞（中间件结算路径），队空仍记实际
    /// 用量（无预留不丢账）。完成 token 滑动均值同步更新（EMA α=1/4，
    /// 冷启动首结算锚定）。
    fn settle_inner(
        &self,
        config: &Config,
        key_hash: &str,
        actual_millicents: u64,
        completion_tokens: u64,
        reservation: Option<&KeyBudgetReservation>,
    ) {
        let now = self.now_secs();
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let consumed: Option<InFlight> = match reservation {
            Some(res) => (res.key_hash == key_hash)
                .then(|| guard.in_flight.remove(&res.id))
                .flatten(),
            None => {
                let mut id = None;
                let mut queue = guard.fifo.remove(key_hash).unwrap_or_default();
                while let Some(front) = queue.pop_front() {
                    if guard.in_flight.contains_key(&front) {
                        id = Some(front);
                        break;
                    }
                }
                guard.fifo.insert(key_hash.to_string(), queue);
                id.and_then(|id| guard.in_flight.remove(&id))
            }
        };
        if let Some(entry) = consumed {
            for (name, start) in entry.windows {
                if let Some(cell) = guard.windows.get_mut(&(key_hash.to_string(), name)) {
                    if cell.start == start {
                        cell.reserved = cell.reserved.saturating_sub(entry.est);
                    }
                }
            }
        }
        // 实际用量入当前窗（无规则窗时无处可记——预算已整体关闭）
        for (name, period, _) in key_budget_windows(config, key_hash) {
            let start = period.window_start(now, 0).unwrap_or(now);
            let cell = guard
                .windows
                .entry((key_hash.to_string(), name))
                .or_insert(WindowCell {
                    start,
                    reserved: 0,
                    settled: 0,
                });
            if cell.start != start {
                cell.start = start;
                cell.reserved = 0;
                cell.settled = 0;
            }
            cell.settled = cell.settled.saturating_add(actual_millicents);
        }
        let mean = guard.means.get(key_hash).copied();
        let next = match mean {
            None => completion_tokens,
            Some(m) => (m.saturating_mul(3).saturating_add(completion_tokens)) / 4,
        };
        guard.means.insert(key_hash.to_string(), next);
    }

    /// 结算（显式句柄路径，[`KeyBudgetSource`] 契约面）。
    pub fn settle(
        &self,
        config: &Config,
        key_hash: &str,
        actual_millicents: u64,
        completion_tokens: u64,
        reservation: &KeyBudgetReservation,
    ) {
        self.settle_inner(
            config,
            key_hash,
            actual_millicents,
            completion_tokens,
            Some(reservation),
        );
    }

    /// 结算（FIFO 配对路径，中间件结算点专用）：无句柄按入序配对在飞
    /// 预留；返回是否消费了一个预留（断言/观测用）。
    pub fn settle_fifo(
        &self,
        config: &Config,
        key_hash: &str,
        actual_millicents: u64,
        completion_tokens: u64,
    ) -> bool {
        let before = self.in_flight_count();
        self.settle_inner(config, key_hash, actual_millicents, completion_tokens, None);
        self.in_flight_count() < before
    }

    /// 读侧视图（admin/观测）：该 key 首条规则窗的占用快照（读侧惰性
    /// 翻转，过期窗占用不得外泄——与 [`crate::middleware::quota`] 读侧
    /// 同口径）。无预算规则 → None。
    pub fn view(&self, config: &Config, key_hash: &str) -> Option<KeyBudgetView> {
        let (name, period, limit) = key_budget_windows(config, key_hash).into_iter().next()?;
        let now = self.now_secs();
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let start = period.window_start(now, 0).unwrap_or(now);
        let cell = guard
            .windows
            .entry((key_hash.to_string(), name.clone()))
            .or_insert(WindowCell {
                start,
                reserved: 0,
                settled: 0,
            });
        if cell.start != start {
            cell.start = start;
            cell.reserved = 0;
            cell.settled = 0;
        }
        Some(KeyBudgetView {
            cost_used: cell.occupied(),
            cost_limit: limit,
            reset_secs: (period.reset_at(start, 0).saturating_sub(now)).max(1) as u64,
            window: name,
        })
    }

    /// 该 key 的完成 token 滑动均值（输出腿估成本输入；None = 冷启动，
    /// 调用方取 limit 一半保守值——均值是观测产物，价目与限额单一真源
    /// 在配置面）。
    pub fn completion_mean(&self, key_hash: &str) -> Option<u64> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.means.get(key_hash).copied()
    }

    fn in_flight_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .in_flight
            .len()
    }
}

/// 清扫陈旧在飞（全部规则窗均已翻转者——其预留已随窗消亡、settle 不会
/// 再回冲）。O(n) 仅在超容量时发生（摊还）。
fn sweep_stale(guard: &mut Inner) {
    let stale: Vec<u64> = guard
        .in_flight
        .iter()
        .filter(|(_, entry)| {
            entry.windows.iter().all(|(name, start)| {
                guard
                    .windows
                    .get(&(entry.key_hash.clone(), name.clone()))
                    .is_some_and(|cell| cell.start != *start)
            })
        })
        .map(|(id, _)| *id)
        .collect();
    for id in stale {
        guard.in_flight.remove(&id);
    }
}

impl KeyBudgetSource for KeyBudgetLedger {
    fn reserve(&self, config: &Config, key_hash: &str, est_millicents: u64) -> ReserveOutcome {
        KeyBudgetLedger::reserve(self, config, key_hash, est_millicents)
    }

    fn settle(
        &self,
        config: &Config,
        key_hash: &str,
        actual_millicents: u64,
        reservation: &KeyBudgetReservation,
    ) {
        KeyBudgetLedger::settle(self, config, key_hash, actual_millicents, 0, reservation)
    }

    fn view(&self, config: &Config, key_hash: &str) -> Option<KeyBudgetView> {
        KeyBudgetLedger::view(self, config, key_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthSection, DatabaseConfig, KeyBudgetRule, KeyBudgetsSection, ServerConfig,
    };

    /// sha256("test-good-key")（与集成测试同值）。
    const KEY_HASH: &str = "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";
    const OTHER_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn rule(window: &str, limit: u64) -> KeyBudgetRule {
        KeyBudgetRule {
            api_key_hash: KEY_HASH.into(),
            cost_limit_millicents: limit,
            window: window.into(),
            fail_open: false,
        }
    }

    /// 最小 Config（账本单测只读 key_budgets 面）。
    fn config(rules: Vec<KeyBudgetRule>) -> Config {
        crate::config::Config {
            masking: Default::default(),
            web_search: Default::default(),
            rate_limit: Default::default(),
            otlp: Default::default(),
            vendor_budgets: Vec::new(),
            key_budgets: KeyBudgetsSection { rules },
            usage_insights: Default::default(),
            catalog_sync: Default::default(),
            subscription: Default::default(),
            subscription_accounts: Vec::new(),
            metrics: Default::default(),
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: Some(AuthSection {
                enabled: true,
                api_keys: vec![],
                header: Default::default(),
                keys: vec![],
                oidc_rules: None,
            }),
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
        }
    }

    fn ledger_at(now: i64) -> KeyBudgetLedger {
        KeyBudgetLedger::with_now(Box::new(move || now))
    }

    fn view_used(ledger: &KeyBudgetLedger, config: &Config, hash: &str) -> Option<u64> {
        ledger.view(config, hash).map(|v| v.cost_used)
    }

    /// reserve→settle 回冲：占用 = 预留额，结算后 = 实际额。
    #[test]
    fn reserve_then_settle_reconciles_delta() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let res = match ledger.reserve(&config, KEY_HASH, 600) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(600));
        ledger.settle(&config, KEY_HASH, 250, 42, &res);
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(250));
        assert_eq!(ledger.in_flight_count(), 0);
    }

    /// est=0 的零预留也要结算：实际用量入窗（无预留不丢账）。
    #[test]
    fn zero_estimate_reservation_still_settles_actual() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let res = match ledger.reserve(&config, KEY_HASH, 0) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("零预留也应 Admitted，得 {other:?}"),
        };
        ledger.settle(&config, KEY_HASH, 77, 10, &res);
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(77));
    }

    /// 并发预留合计不超限：limit=100、4×30 → 恰 3 Admitted + 1 Exceeded，
    /// 拒绝路径零落账。
    #[test]
    fn concurrent_reserves_never_overshoot_limit() {
        let config = config(vec![rule("daily", 100)]);
        let ledger = std::sync::Arc::new(ledger_at(1_700_000_000));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let ledger = ledger.clone();
                let config = config.clone();
                std::thread::spawn(move || ledger.reserve(&config, KEY_HASH, 30))
            })
            .collect();
        let outcomes: Vec<ReserveOutcome> = handles
            .into_iter()
            .map(|h| h.join().expect("线程不 panic"))
            .collect();
        let admitted = outcomes
            .iter()
            .filter(|o| matches!(o, ReserveOutcome::Admitted(_)))
            .count();
        let exceeded = outcomes
            .iter()
            .filter(|o| matches!(o, ReserveOutcome::Exceeded { .. }))
            .count();
        assert_eq!(admitted, 3, "3×30=90 ≤ 100");
        assert_eq!(exceeded, 1, "第 4 笔 120 > 100 必拒");
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(90));
        assert_eq!(
            ledger.view(&config, KEY_HASH).map(|v| v.cost_limit),
            Some(100)
        );
    }

    /// 超限产物带 Retry-After（窗重置余量，≥1s），拒绝路径零落账。
    #[test]
    fn exceeded_carries_retry_after() {
        let config = config(vec![rule("daily", 50)]);
        let ledger = ledger_at(1_700_000_000);
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 51),
            ReserveOutcome::Exceeded { retry_after_secs } if retry_after_secs >= 1
        ));
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(0));
    }

    /// 窗口翻转：日窗跨 UTC 零点后占用归零；跨窗结算记入新窗、旧窗预留
    /// 不污染新窗。
    #[test]
    fn window_flip_resets_occupancy_and_lands_settle_in_new_window() {
        // 1_700_000_000.rem_euclid(86_400) = 28_000 → 当日窗起点 1_699_971_200
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let res = match ledger.reserve(&config, KEY_HASH, 400) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(400));

        // 时钟拨到次日 10:00：占用归零（翻转惰性收敛）
        let next_day = 1_700_000_000i64 - 1_700_000_000i64.rem_euclid(86_400) + 86_400 + 36_000;
        {
            let mut now = ledger.now.lock().unwrap();
            *now = Box::new(move || next_day);
        }
        assert_eq!(
            view_used(&ledger, &config, KEY_HASH),
            Some(0),
            "跨日翻转后旧窗占用不得外泄"
        );
        // 旧窗预留的回冲被跳过（随窗消亡），实际用量入新窗
        ledger.settle(&config, KEY_HASH, 120, 30, &res);
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(120));
        assert_eq!(ledger.in_flight_count(), 0);
    }

    /// 无预算规则 → NoBudget；view 返回 None；他 key 不受本 key 规则影响。
    #[test]
    fn no_budget_rule_passes_through() {
        let empty = config(vec![]);
        let ledger = ledger_at(1_700_000_000);
        assert_eq!(
            ledger.reserve(&empty, KEY_HASH, 10),
            ReserveOutcome::NoBudget
        );
        assert!(ledger.view(&empty, KEY_HASH).is_none());
        let config = config(vec![rule("daily", 100)]);
        assert_eq!(
            ledger.reserve(&config, OTHER_HASH, 10),
            ReserveOutcome::NoBudget
        );
    }

    /// FIFO 结算配对：两预留按入序回冲，聚合守恒；队空仍记实际用量。
    #[test]
    fn fifo_settle_pairs_in_order() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 100),
            ReserveOutcome::Admitted(_)
        ));
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 200),
            ReserveOutcome::Admitted(_)
        ));
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(300));
        assert!(
            ledger.settle_fifo(&config, KEY_HASH, 40, 5),
            "弹出 est=100 预留"
        );
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(240));
        assert!(
            ledger.settle_fifo(&config, KEY_HASH, 180, 20),
            "弹出 est=200 预留"
        );
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(220));
        assert!(
            !ledger.settle_fifo(&config, KEY_HASH, 10, 1),
            "队空无预留可弹"
        );
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(230));
    }

    /// 完成均值：冷启动 None，首结算锚定，后续 EMA(1/4)。
    #[test]
    fn completion_mean_ema_anchors_then_smooths() {
        let config = config(vec![rule("daily", 1_000_000)]);
        let ledger = ledger_at(1_700_000_000);
        assert_eq!(ledger.completion_mean(KEY_HASH), None, "冷启动无均值");
        let res = match ledger.reserve(&config, KEY_HASH, 10) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        ledger.settle(&config, KEY_HASH, 10, 400, &res);
        assert_eq!(ledger.completion_mean(KEY_HASH), Some(400));
        let res = match ledger.reserve(&config, KEY_HASH, 10) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        ledger.settle(&config, KEY_HASH, 10, 800, &res);
        assert_eq!(ledger.completion_mean(KEY_HASH), Some((400 * 3 + 800) / 4));
    }

    /// Debug 快照只暴露窗口数与在飞数（admin 观测面，不含 key 原文）。
    #[test]
    fn debug_snapshot_reports_ledger_shape() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        assert_eq!(
            format!("{ledger:?}"),
            "KeyBudgetLedger { windows: 0, in_flight: 0 }"
        );
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 10),
            ReserveOutcome::Admitted(_)
        ));
        let rendered = format!("{ledger:?}");
        assert!(rendered.contains("windows: 1"), "{rendered}");
        assert!(rendered.contains("in_flight: 1"), "{rendered}");
    }

    /// 容量 0：reserve 恒 Unavailable（fail-closed 注入形态），零落账。
    #[test]
    fn zero_capacity_rejects_with_unavailable() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = KeyBudgetLedger::with_capacity(0);
        assert_eq!(
            ledger.reserve(&config, KEY_HASH, 1),
            ReserveOutcome::Unavailable
        );
        assert_eq!(ledger.in_flight_count(), 0);
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(0));
    }

    /// 预留判定前窗已翻转：旧窗占用不得继续压额（判定前清零）。
    #[test]
    fn reserve_after_window_flip_resets_stale_cell() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 900),
            ReserveOutcome::Admitted(_)
        ));
        {
            let mut now = ledger.now.lock().unwrap();
            *now = Box::new(move || 1_700_000_000 + 86_400);
        }
        assert!(
            matches!(
                ledger.reserve(&config, KEY_HASH, 900),
                ReserveOutcome::Admitted(_)
            ),
            "新窗起点必须清零旧窗 900 占用后再判定"
        );
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(900));
    }

    /// FIFO 配对跳过「已按句柄结算」残留的陈旧 id（in_flight 已移除、队列未清）。
    #[test]
    fn fifo_settle_skips_ids_already_settled_by_handle() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let first = match ledger.reserve(&config, KEY_HASH, 100) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 200),
            ReserveOutcome::Admitted(_)
        ));
        ledger.settle(&config, KEY_HASH, 100, 1, &first);
        assert_eq!(ledger.in_flight_count(), 1);
        assert!(
            ledger.settle_fifo(&config, KEY_HASH, 200, 1),
            "残留 id 跳过后必须配对 est=200 的在飞预留"
        );
        assert_eq!(ledger.in_flight_count(), 0);
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(300));
    }

    /// 结算时窗已翻转（读侧尚未收敛）：结算内联清零旧窗后记入实际用量。
    #[test]
    fn settle_after_window_flip_resets_stale_cell() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let res = match ledger.reserve(&config, KEY_HASH, 500) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        {
            let mut now = ledger.now.lock().unwrap();
            *now = Box::new(move || 1_700_000_000 + 86_400);
        }
        ledger.settle(&config, KEY_HASH, 120, 3, &res);
        assert_eq!(
            view_used(&ledger, &config, KEY_HASH),
            Some(120),
            "旧窗 500 预留随窗消亡，实际 120 记入新窗"
        );
    }

    /// 容量触顶：窗已翻转的在飞预留被清扫，容量回收后新预留准入。
    #[test]
    fn stale_sweep_reclaims_capacity_after_window_flip() {
        let config = config(vec![rule("daily", 1_000)]);
        let mut ledger = ledger_at(1_700_000_000);
        ledger.max_in_flight = 1;
        assert!(matches!(
            ledger.reserve(&config, KEY_HASH, 10),
            ReserveOutcome::Admitted(_)
        ));
        assert_eq!(
            ledger.reserve(&config, KEY_HASH, 10),
            ReserveOutcome::Unavailable,
            "同窗预留非陈旧，容量 1 已满必拒"
        );
        {
            let mut now = ledger.now.lock().unwrap();
            *now = Box::new(move || 1_700_000_000 + 86_400);
        }
        // 读侧先收敛旧窗（sweep 以账本格起点判陈旧）
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(0));
        assert!(
            matches!(
                ledger.reserve(&config, KEY_HASH, 10),
                ReserveOutcome::Admitted(_)
            ),
            "陈旧预留清扫后容量必须回收"
        );
        assert_eq!(ledger.in_flight_count(), 1, "只剩新窗预留");
        assert_eq!(view_used(&ledger, &config, KEY_HASH), Some(10));
    }

    /// 契约端口（`KeyBudgetSource`）委托同一账本：预留-结算-视图三面一致。
    #[test]
    fn key_budget_source_trait_ports_delegate() {
        let config = config(vec![rule("daily", 1_000)]);
        let ledger = ledger_at(1_700_000_000);
        let res = match KeyBudgetSource::reserve(&ledger, &config, KEY_HASH, 300) {
            ReserveOutcome::Admitted(res) => res,
            other => panic!("预期 Admitted，得 {other:?}"),
        };
        assert_eq!(
            KeyBudgetSource::view(&ledger, &config, KEY_HASH).map(|v| v.cost_used),
            Some(300)
        );
        KeyBudgetSource::settle(&ledger, &config, KEY_HASH, 80, &res);
        assert_eq!(
            KeyBudgetSource::view(&ledger, &config, KEY_HASH).map(|v| v.cost_used),
            Some(80)
        );
    }
}
