// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 全局指标收集
//!
//! 线程安全的请求计数、延迟累计与错误率统计，按厂商维度记录。
//! 数据通过原子操作写入，快照通过 RwLock 读出，适用于 hot path。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// EWMA 平滑系数 α=0.2 — 新样本权重 20%，兼顾灵敏度与抗抖动
/// （100→200→400ms 的序列 ewma 逐次 100→120→176，新值权重大于历史均值）。
/// 后仅用于错误率道（延迟道已迁移 Peak-EWMA，见 [`peak_ewma_alpha`]）。
const EWMA_ALPHA: f64 = 0.2;

/// Peak-EWMA 默认时间常数 τ（ms）— 与 `[routing].ewma_tau_ms` 缺省
/// 同值（Kong latency.lua DECAY_TIME=10s 同值）。
pub const DEFAULT_EWMA_TAU_MS: u64 = 10_000;

/// 流量份额滑窗参数 — 60s 窗、10 桶环形（每桶 6s）。
/// 窗口推进由写入驱动（惰性），无需后台线程。
pub const TRAFFIC_WINDOW: Duration = Duration::from_secs(60);
pub const TRAFFIC_BUCKETS: usize = 10;

/// 滑动窗口份额计数器 — 60s 窗、10 桶环形（每桶 6s）。
/// 以**绝对时刻**换算桶序（不丢亚秒残差，亚秒写入间隔下窗口照常轮转）；
/// 推进时惰性清零过期桶；读侧以全体 max(last_seen) 为逻辑当前时刻——
/// 停止写入的 vendor 旧桶随全局时间衰减出窗。
/// 测试经 record_traffic_at 注入假时钟做确定性断言
pub(crate) struct SlidingCounter {
    bucket_secs: u64,
    buckets: Vec<u64>,
    /// 桶序换算的 t0 锚点（首样本时刻）；None = 尚无样本
    base: Option<Instant>,
    last_abs_bucket: Option<u64>,
    last_seen: Instant,
}

impl SlidingCounter {
    fn new(now: Instant) -> Self {
        Self {
            bucket_secs: (TRAFFIC_WINDOW.as_secs() / TRAFFIC_BUCKETS as u64).max(1),
            buckets: vec![0; TRAFFIC_BUCKETS],
            base: None,
            last_abs_bucket: None,
            last_seen: now,
        }
    }

    /// 绝对桶序换算并惰性清零过期桶。时钟回退（now < base）时桶序
    /// 饱和为 last（防御：滑窗要求单调时钟）。
    fn advance_to(&mut self, now: Instant) {
        let (base, last) = match (self.base, self.last_abs_bucket) {
            (Some(base), Some(last)) => (base, last),
            _ => return, // 首样本前无桶可清
        };
        let raw = now.saturating_duration_since(base).as_secs() / self.bucket_secs;
        let abs = raw.max(last);
        let n = self.buckets.len() as u64;
        if abs > last {
            // 清零 (last, abs] 环形区间；跨度超过一整圈时全清（min(n)）
            let span = (abs - last).min(n);
            for i in 1..=span {
                let idx = ((last + i) % n) as usize;
                self.buckets[idx] = 0;
            }
        }
        self.last_abs_bucket = Some(abs);
        self.last_seen = now;
    }

    fn record_at(&mut self, now: Instant) {
        if self.base.is_none() {
            // 首样本：建立 t0 锚点，写入桶 0
            self.base = Some(now);
            self.last_abs_bucket = Some(0);
            self.buckets[0] += 1;
            self.last_seen = now;
            return;
        }
        self.advance_to(now);
        let n = self.buckets.len() as u64;
        let abs = self.last_abs_bucket.unwrap_or(0);
        self.buckets[(abs % n) as usize] += 1;
    }

    /// `pub(crate)` 访问器 — 失败率滑窗（本模块
    /// `VendorFailureWindowPair`）与流量份额（`traffic_shares`）共用。
    pub(crate) fn window_total(&self) -> u64 {
        self.buckets.iter().sum()
    }
}

/// 每 vendor 一对失败率滑窗 — total 记全部请求结局（纯观测，
/// 常开），failures 仅失败（含 429，见 manager 记账点注释）记入。
/// 复用 [`SlidingCounter`]（60s 窗 / 10 桶，与流量份额同参），写入由
/// [`GlobalMetrics::record_vendor_outcome_at`] 驱动（惰性推进，无后台线程）。
struct VendorFailureWindowPair {
    total: SlidingCounter,
    failures: SlidingCounter,
}

/// Peak-EWMA 动态平滑系数 — α_dyn = 1 − exp(−Δt/τ)（Kong
/// latency.lua:34-41 同式）。久未采样（Δt ≫ τ）→ α→1 快速反映恢复；
/// 高频采样（Δt ≪ τ）→ α≈Δt/τ 平滑抗噪。τ=0 视为极限「完全采新样本」。
fn peak_ewma_alpha(dt_ms: f64, tau_ms: u64) -> f64 {
    if tau_ms == 0 {
        return 1.0;
    }
    1.0 - (-dt_ms / tau_ms as f64).exp()
}

/// 单道 Peak-EWMA 更新 — 首样本直接种子，后续按 α_dyn 混合。
/// `last_at` **每道独立**：worker 完成分支 queue/e2e 两道同刻落账，若共享
/// 时间锚，第二道 Δt=0 → α=0 会整笔漏记。
fn update_lane_ewma(
    ewma: &mut f64,
    last_at: &mut Option<Instant>,
    sample_ms: f64,
    now: Instant,
    tau_ms: u64,
) {
    match *last_at {
        None => *ewma = sample_ms,
        Some(t) => {
            let dt_ms = now.saturating_duration_since(t).as_secs_f64() * 1000.0;
            let alpha = peak_ewma_alpha(dt_ms, tau_ms);
            *ewma = alpha * sample_ms + (1.0 - alpha) * *ewma;
        }
    }
    *last_at = Some(now);
}

/// 全局指标 — 所有 VendorManager 实例共享一份
pub struct GlobalMetrics {
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    total_duration_ns: AtomicU64,
    per_vendor: RwLock<HashMap<String, VendorMetrics>>,
    /// 会话亲和命中计数（亲和 boost 选中）
    affinity_hits: AtomicU64,
    /// 会话亲和未命中计数（无绑定，或绑定 vendor 已不在合格集合）
    affinity_misses: AtomicU64,
    /// 每 vendor 一只滑窗份额计数器（60s / 10 桶）
    traffic: RwLock<HashMap<String, SlidingCounter>>,
    /// 每 vendor 失败率滑窗（total/failures，60s / 10 桶）。
    /// 窗口计数常开（纯观测）；软降权消费由 `[resilience].failure_rate_gate`
    /// 门控（默认关）。
    vendor_windows: RwLock<HashMap<String, VendorFailureWindowPair>>,
    /// Peak-EWMA 时间常数 τ（ms）。生产由 VendorManager 落账前从
    /// `[routing].ewma_tau_ms` 热同步（[`Self::set_ewma_tau_ms`]）；缺省
    /// [`DEFAULT_EWMA_TAU_MS`]。
    ewma_tau_ms: AtomicU64,
    /// 重试放大的重试维度计数 — 重试 attempt 总数（manager
    /// 重试循环 attempt+=1 时记）。此前全仓重试维度计数为 0（grep 实证）。
    retried_attempts: AtomicU64,
    /// 失败请求计数（失败率 f 与放大比 r 的分母来源之一）。
    /// 与 `failed_requests`（record_request 驱动、含透传/流式）独立：本计数
    /// 只在 manager 记账点的失败分支累加，语义 =「进入过重试候选路径的
    /// 失败」。
    failed_requests_before_retry: AtomicU64,
    /// 难度定档计数（[routing.difficulty]）：融合 / 纯规则 / 缓存命中 /
    /// 分类失败 / 特征退化 / 熔断开与恢复；tier 分布三维 + 延迟累计。
    /// 定档未接线时恒 0
    difficulty_fusion: AtomicU64,
    difficulty_rule_only: AtomicU64,
    difficulty_cache_hit: AtomicU64,
    difficulty_classifier_failed: AtomicU64,
    difficulty_features_degraded: AtomicU64,
    difficulty_breaker_opened: AtomicU64,
    difficulty_breaker_recovered: AtomicU64,
    difficulty_tier_low: AtomicU64,
    difficulty_tier_mid: AtomicU64,
    difficulty_tier_high: AtomicU64,
    difficulty_latency_ns_total: AtomicU64,
    difficulty_latency_count: AtomicU64,
}

impl GlobalMetrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            total_duration_ns: AtomicU64::new(0),
            per_vendor: RwLock::new(HashMap::new()),
            affinity_hits: AtomicU64::new(0),
            affinity_misses: AtomicU64::new(0),
            traffic: RwLock::new(HashMap::new()),
            vendor_windows: RwLock::new(HashMap::new()),
            ewma_tau_ms: AtomicU64::new(DEFAULT_EWMA_TAU_MS),
            retried_attempts: AtomicU64::new(0),
            failed_requests_before_retry: AtomicU64::new(0),
            difficulty_fusion: AtomicU64::new(0),
            difficulty_rule_only: AtomicU64::new(0),
            difficulty_cache_hit: AtomicU64::new(0),
            difficulty_classifier_failed: AtomicU64::new(0),
            difficulty_features_degraded: AtomicU64::new(0),
            difficulty_breaker_opened: AtomicU64::new(0),
            difficulty_breaker_recovered: AtomicU64::new(0),
            difficulty_tier_low: AtomicU64::new(0),
            difficulty_tier_mid: AtomicU64::new(0),
            difficulty_tier_high: AtomicU64::new(0),
            difficulty_latency_ns_total: AtomicU64::new(0),
            difficulty_latency_count: AtomicU64::new(0),
        }
    }

    // ---- 难度定档观测（difficulty 模块写、admin/快照读）----

    /// 定档路径计数 + tier 分布 + 延迟（一次调用记齐；classifier_failed /
    /// features_degraded / 熔断迁移由专门入口补记）
    pub fn record_difficulty_decision(
        &self,
        fusion: bool,
        cache_hit: bool,
        tier: crate::config::Tier,
        latency: std::time::Duration,
    ) {
        use crate::config::Tier;
        match cache_hit {
            true => self.difficulty_cache_hit.fetch_add(1, Ordering::Relaxed),
            false if fusion => self.difficulty_fusion.fetch_add(1, Ordering::Relaxed),
            false => self.difficulty_rule_only.fetch_add(1, Ordering::Relaxed),
        };
        match tier {
            Tier::Low => &self.difficulty_tier_low,
            Tier::Mid => &self.difficulty_tier_mid,
            Tier::High => &self.difficulty_tier_high,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.difficulty_latency_ns_total
            .fetch_add(latency.as_nanos() as u64, Ordering::Relaxed);
        self.difficulty_latency_count
            .fetch_add(1, Ordering::Relaxed);
    }

    /// O2：厂商维度缓存命中记账（entry 不存在则惰性创建，与 record_request 同构）
    pub fn record_vendor_cache(&self, vendor: &str, cache_read: u64, input_total: u64) {
        if let Ok(mut guard) = self.per_vendor.write() {
            guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor))
                .record_cache_tokens(cache_read, input_total);
        }
    }

    pub fn record_difficulty_classifier_failed(&self) {
        self.difficulty_classifier_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_difficulty_features_degraded(&self) {
        self.difficulty_features_degraded
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_difficulty_breaker_opened(&self) {
        self.difficulty_breaker_opened
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_difficulty_breaker_recovered(&self) {
        self.difficulty_breaker_recovered
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 热同步 Peak-EWMA 时间常数 τ（ms）。VendorManager 在每次
    /// 延迟道落账前调用，POST /admin/config 更新 `[routing].ewma_tau_ms`
    /// 后下一次采样即用新 τ。
    pub fn set_ewma_tau_ms(&self, tau_ms: u64) {
        self.ewma_tau_ms.store(tau_ms, Ordering::Relaxed);
    }

    /// 记录一次请求结果。`duration` 通常由 `Instant::now()` 差值计算。
    pub fn record_request(&self, vendor: &str, success: bool, duration: std::time::Duration) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
        self.total_duration_ns
            .fetch_add(duration.as_nanos() as u64, Ordering::Relaxed);
        self.record_traffic_at(vendor, Instant::now());

        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.requests += 1;
            if success {
                entry.successes += 1;
            } else {
                entry.failures += 1;
            }
            entry.total_duration_ns = entry
                .total_duration_ns
                .saturating_add(duration.as_nanos() as u64);
            entry.last_request_at = Some(now_unix_ts());

            // EWMA 更新 — ewma = α×x + (1-α)×ewma，替换全生命期
            // 均值（total_duration_ns/requests）的钝化问题：后者对近期劣化
            // 无感，EWMA 对延迟/错误率的近期变化指数收敛。字段在 per_vendor
            // 写锁内更新，无需原子（快照读走同一把锁）。
            //
            // 延迟 EWMA 迁出本函数 — 本方法入参 duration
            // 的口径因调用路径而异（worker=排队+执行全程 / 流式=send→响应头 /
            // 透传=全程），混流汇入同一 ewma_latency_ms 正是本次修复的缺陷。
            // 本方法此后只喂错误率道；延迟信号由 record_e2e / record_ttft /
            // record_queue 按口径分道喂入。
            let err_sample = if success { 0.0 } else { 1.0 };
            if entry.ewma_samples == 0 {
                entry.ewma_error_rate = err_sample;
            } else {
                entry.ewma_error_rate =
                    EWMA_ALPHA * err_sample + (1.0 - EWMA_ALPHA) * entry.ewma_error_rate;
            }
            entry.ewma_samples += 1;
        }
    }

    /// 记录流式 TTFT（Time-To-First-Token）样本。
    ///
    /// 口径 = send 发起 → 上游响应头返回（严格首 token 口径网关侧不可得，
    /// 以响应头近似——config.toml `[routing]` 注释如实标注）。`now` 注入便于
    /// 测试驱动 Peak-EWMA 的时间衰减（record_traffic_at 先例）。
    pub fn record_ttft(&self, vendor: &str, duration: std::time::Duration, now: Instant) {
        let tau_ms = self.ewma_tau_ms.load(Ordering::Relaxed);
        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.ttft_samples += 1;
            let ms = duration.as_secs_f64() * 1000.0;
            update_lane_ewma(
                &mut entry.ewma_ttft_ms,
                &mut entry.last_ttft_at,
                ms,
                now,
                tau_ms,
            );
        }
    }

    /// 记录非流式端到端延迟样本（口径 = enqueued_at → 完成含排队）。
    /// 同时更新 `ewma_latency_ms` 兼容输出（后其语义收窄为 e2e
    /// 别名——此前混流四路口径，/admin/metrics 外部消费方仍读该字段）。
    pub fn record_e2e(&self, vendor: &str, duration: std::time::Duration, now: Instant) {
        let tau_ms = self.ewma_tau_ms.load(Ordering::Relaxed);
        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.e2e_samples += 1;
            let ms = duration.as_secs_f64() * 1000.0;
            update_lane_ewma(
                &mut entry.ewma_e2e_ms,
                &mut entry.last_e2e_at,
                ms,
                now,
                tau_ms,
            );
            // 兼容别名：与 ewma_e2e_ms 同值直拷（共享时间锚会令第二次
            // Peak-EWMA 更新 Δt=0 → α=0 整笔漏记，别名无独立平滑状态）
            entry.ewma_latency_ms = entry.ewma_e2e_ms;
        }
    }

    /// 记录排队段延迟样本（口径 = enqueued_at → worker 取走
    /// dequeued_at，含 RPS/并发票据等待前的队列滞留）。纯观测道。
    pub fn record_queue(&self, vendor: &str, duration: std::time::Duration, now: Instant) {
        let tau_ms = self.ewma_tau_ms.load(Ordering::Relaxed);
        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.queue_samples += 1;
            let ms = duration.as_secs_f64() * 1000.0;
            update_lane_ewma(
                &mut entry.ewma_queue_ms,
                &mut entry.last_queue_at,
                ms,
                now,
                tau_ms,
            );
        }
    }

    /// 记录一次错误（不增加请求计数，仅记录最近错误信息）
    pub fn record_error(&self, vendor: &str, error: &str) {
        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.last_error = Some(error.to_string());
        }
    }

    /// 以指定时刻触碰 vendor 的流量滑窗当前桶。
    ///
    /// 生产由 [`Self::record_request`] 以真实时钟调用；`pub` 是为了测试
    /// 注入假时钟做确定性滑窗推进断言（tests 见本文件 `#[cfg(test)]`）。
    pub fn record_traffic_at(&self, vendor: &str, now: Instant) {
        if let Ok(mut guard) = self.traffic.write() {
            guard
                .entry(vendor.to_string())
                .or_insert_with(|| SlidingCounter::new(now))
                .record_at(now);
        }
    }

    /// 以指定时刻记录 vendor 一次请求结局到失败率滑窗。
    /// success 记入 total；失败（含 429/RateLimited，由记账点判定）同时
    /// 记入 failures。`now` 注入与 `record_traffic_at` 同先例——测试驱动
    /// 确定性滑窗推进。存取失败静默降级（观测不得影响执行路径）。
    pub fn record_vendor_outcome_at(&self, vendor: &str, success: bool, now: Instant) {
        if let Ok(mut guard) = self.vendor_windows.write() {
            let pair = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorFailureWindowPair {
                    total: SlidingCounter::new(now),
                    failures: SlidingCounter::new(now),
                });
            pair.total.record_at(now);
            if !success {
                pair.failures.record_at(now);
            }
        }
    }

    /// vendor 的失败率滑窗快照 `(窗内总数, 窗内失败数)`。
    /// 无样本 vendor 返回 (0, 0)。读侧先把全部 counter 推进到全体
    /// `max(last_seen)`（[`Self::traffic_shares`] 同先例）——vendor 停止
    /// 写入后其旧失败随全局时间衰减出窗，不滞留失败率。
    pub fn vendor_failure_window(&self, vendor: &str) -> (u64, u64) {
        let mut guard = match self.vendor_windows.write() {
            Ok(g) => g,
            Err(_) => return (0, 0),
        };
        let now = match guard
            .values()
            .map(|p| p.total.last_seen.max(p.failures.last_seen))
            .max()
        {
            Some(t) => t,
            None => return (0, 0),
        };
        let Some(pair) = guard.get_mut(vendor) else {
            return (0, 0);
        };
        pair.total.advance_to(now);
        pair.failures.advance_to(now);
        (pair.total.window_total(), pair.failures.window_total())
    }

    /// 重试 attempt 计数（manager 重试循环 attempt+=1 时记）
    pub fn record_retry_attempt(&self) {
        self.retried_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// 失败请求计数（放大比 r 的分母）
    pub fn record_failed_request_before_retry(&self) {
        self.failed_requests_before_retry
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 重试 attempt 计数读数
    pub fn retried_attempts(&self) -> u64 {
        self.retried_attempts.load(Ordering::Relaxed)
    }

    /// 失败请求计数读数
    pub fn failed_requests_before_retry(&self) -> u64 {
        self.failed_requests_before_retry.load(Ordering::Relaxed)
    }

    /// 流量滑窗内总请求数 — 守门器的 λ 估计分母（窗 60s，
    /// λ = 本值 / 60）。读侧推进语义与 [`Self::traffic_shares`] 一致
    ///（全体 `max(last_seen)` 为逻辑当前时刻）。
    pub fn traffic_window_total(&self) -> u64 {
        let mut guard = match self.traffic.write() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        let now = match guard.values().map(|c| c.last_seen).max() {
            Some(t) => t,
            None => return 0,
        };
        guard
            .values_mut()
            .map(|counter| {
                counter.advance_to(now);
                counter.window_total()
            })
            .sum()
    }

    /// 重试放大守门器的观测视图 `(r, f, λ)`：r = 重试/失败请求数（无失败
    /// → None）；f = 全生命期失败率；λ = 入站速率估计（滑窗总量/窗宽）
    pub fn amplification_view(&self) -> Option<(f64, f64, f64)> {
        let failed = self.failed_requests_before_retry();
        if failed == 0 {
            return None;
        }
        let r = self.retried_attempts() as f64 / failed as f64;
        let lambda = self.traffic_window_total() as f64 / TRAFFIC_WINDOW.as_secs() as f64;
        Some((r, self.error_rate(), lambda))
    }

    /// 亲和命中计数（router 亲和 boost 选中时调用）
    pub fn record_affinity_hit(&self) {
        self.affinity_hits.fetch_add(1, Ordering::Relaxed);
    }
    /// 亲和未命中计数（无绑定 / 绑定 vendor 已不在合格集合）
    pub fn record_affinity_miss(&self) {
        self.affinity_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// 亲和命中率 = hits / (hits + misses)；无样本返回 0.0
    pub fn affinity_hit_rate(&self) -> f64 {
        let hits = self.affinity_hits.load(Ordering::Relaxed);
        let misses = self.affinity_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            return 0.0;
        }
        hits as f64 / total as f64
    }

    /// 滑窗内各 vendor 的流量份额 `[(vendor_id, share)]`：只含窗内有流量
    /// 的 vendor，总和恒 1.0；读侧推进到全体 max(last_seen)，停止写入的
    /// vendor 旧桶随全局时间衰减出窗
    pub fn traffic_shares(&self) -> Vec<(String, f64)> {
        let mut guard = match self.traffic.write() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let now = match guard.values().map(|c| c.last_seen).max() {
            Some(t) => t,
            None => return Vec::new(),
        };
        for counter in guard.values_mut() {
            counter.advance_to(now);
        }
        let totals: Vec<(String, u64)> = guard
            .iter()
            .map(|(vendor, counter)| (vendor.clone(), counter.window_total()))
            .filter(|(_, total)| *total > 0)
            .collect();
        let grand: u64 = totals.iter().map(|(_, t)| *t).sum();
        if grand == 0 {
            return Vec::new();
        }
        let grand = grand as f64;
        totals
            .into_iter()
            .map(|(vendor, total)| (vendor, total as f64 / grand))
            .collect()
    }

    /// top-1 流量占比 — 最大 vendor 份额；空窗返回 0.0
    pub fn top1_share(&self) -> f64 {
        self.traffic_shares()
            .iter()
            .map(|(_, share)| *share)
            .fold(0.0_f64, f64::max)
    }

    /// 流量份额 Shannon 熵，除以 ln V 归一到 `[0,1]`。
    ///
    /// H = −Σ p·ln p / ln V。V=1（单 vendor 独占）与空窗均定义为 0——
    /// 前者是完全集中的极限（防 ln 1 = 0 除零），后者无流量无分布。
    pub fn share_entropy(&self) -> f64 {
        let shares = self.traffic_shares();
        let v = shares.len();
        if v <= 1 {
            return 0.0;
        }
        let h: f64 = -shares.iter().map(|(_, p)| p * p.ln()).sum::<f64>();
        h / (v as f64).ln()
    }

    /// 路由崩溃告警钩子 — top-1 占比 > 0.9 且窗内活跃 vendor > 2。
    ///
    /// 对应「When Routing Collapses」失败模式：调权后流量收敛单模型而无
    /// 告警。活跃 vendor ≤ 2 的高占比是正常的二选一路由，不告警；空窗
    /// （无流量）不算崩溃。
    pub fn should_alert(&self) -> bool {
        let active = self
            .traffic_shares()
            .iter()
            .filter(|(_, share)| *share > 0.0)
            .count();
        active > 2 && self.top1_share() > 0.9
    }

    /// 错误率 (0.0 - 1.0)，无请求时返回 0.0
    pub fn error_rate(&self) -> f64 {
        let total = self.total_requests.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let failed = self.failed_requests.load(Ordering::Relaxed);
        failed as f64 / total as f64
    }

    /// vendor 的 EWMA 延迟（ms）与错误率快照 — 路由权重第 4 维输入。
    /// 延迟只由 record_e2e 喂入（口径分道）；无 e2e 数据返回 None
    ///（Router 走中性因子不惩罚冷启动；纯流式 vendor 若返回 Some(0.0)
    /// 会被 latency_factor/Po2C 恒判优，舰队整体倾斜——判定必须与
    /// vendor_ewma_e2e 同源 e2e_samples > 0）
    pub fn vendor_ewma(&self, vendor: &str) -> Option<(f64, f64)> {
        let guard = self.per_vendor.read().ok()?;
        let entry = guard.get(vendor)?;
        (entry.e2e_samples > 0).then_some((entry.ewma_latency_ms, entry.ewma_error_rate))
    }

    /// vendor 的 60s 流量窗总数 — LPS 前瞻的到达率输入（α = total/60），
    /// 与软降权/流量份额共用计数器；读取时惰性推进，无记录返回 0
    pub fn vendor_window_total(&self, vendor: &str) -> u64 {
        let mut guard = match self.traffic.write() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        let now = match guard.values().map(|c| c.last_seen).max() {
            Some(t) => t,
            None => return 0,
        };
        match guard.get_mut(vendor) {
            Some(counter) => {
                counter.advance_to(now);
                counter.window_total()
            }
            None => 0,
        }
    }

    /// vendor 的 e2e 道 EWMA（ms）— 仅该道有样本时返回 Some。
    /// min+buffer 候选集（weighted_select_with）的「有延迟数据」判定口径：
    /// 只被 record_request 记过账（如纯流式失败）的 vendor 不算延迟数据。
    pub fn vendor_ewma_e2e(&self, vendor: &str) -> Option<f64> {
        let guard = self.per_vendor.read().ok()?;
        let entry = guard.get(vendor)?;
        (entry.e2e_samples > 0).then_some(entry.ewma_e2e_ms)
    }

    /// 生成可序列化快照
    ///
    /// 快照时若 [`Self::should_alert`] 为 true（top-1 占比 > 0.9
    /// 且活跃 vendor > 2），记 tracing::warn ——「路由崩溃」告警钩子，
    /// 对应论文 When Routing Collapses 的调权后流量收敛单模型无告警问题。
    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.total_requests.load(Ordering::Relaxed);
        let success = self.successful_requests.load(Ordering::Relaxed);
        let failed = self.failed_requests.load(Ordering::Relaxed);
        let total_ns = self.total_duration_ns.load(Ordering::Relaxed);

        let avg_duration_ms = if total == 0 {
            0.0
        } else {
            (total_ns as f64 / total as f64) / 1_000_000.0
        };

        let vendors = self
            .per_vendor
            .read()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default();

        let top1_share = self.top1_share();
        let share_entropy = self.share_entropy();
        if self.should_alert() {
            tracing::warn!(
                top1_share,
                share_entropy,
                "{}",
                crate::i18n::t("utils-metrics-concentration")
            );
        }

        let affinity_hits = self.affinity_hits.load(Ordering::Relaxed);
        let affinity_misses = self.affinity_misses.load(Ordering::Relaxed);

        let difficulty_latency_count = self.difficulty_latency_count.load(Ordering::Relaxed);
        let difficulty_latency_ns_total = self.difficulty_latency_ns_total.load(Ordering::Relaxed);

        MetricsSnapshot {
            total_requests: total,
            successful_requests: success,
            failed_requests: failed,
            error_rate: self.error_rate(),
            avg_duration_ms,
            vendors,
            affinity_hits,
            affinity_misses,
            affinity_hit_rate: self.affinity_hit_rate(),
            top1_share,
            share_entropy,
            difficulty: DifficultyMetricsSnapshot {
                fusion: self.difficulty_fusion.load(Ordering::Relaxed),
                rule_only: self.difficulty_rule_only.load(Ordering::Relaxed),
                cache_hit: self.difficulty_cache_hit.load(Ordering::Relaxed),
                classifier_failed: self.difficulty_classifier_failed.load(Ordering::Relaxed),
                features_degraded: self.difficulty_features_degraded.load(Ordering::Relaxed),
                breaker_opened: self.difficulty_breaker_opened.load(Ordering::Relaxed),
                breaker_recovered: self.difficulty_breaker_recovered.load(Ordering::Relaxed),
                tier_low: self.difficulty_tier_low.load(Ordering::Relaxed),
                tier_mid: self.difficulty_tier_mid.load(Ordering::Relaxed),
                tier_high: self.difficulty_tier_high.load(Ordering::Relaxed),
                avg_latency_ms: difficulty_latency_ns_total
                    .checked_div(difficulty_latency_count)
                    .map_or(0.0, |ns| ns as f64 / 1_000_000.0),
            },
        }
    }

    /// 重置所有指标（主要用于测试）
    pub fn reset(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        self.successful_requests.store(0, Ordering::Relaxed);
        self.failed_requests.store(0, Ordering::Relaxed);
        self.total_duration_ns.store(0, Ordering::Relaxed);
        self.affinity_hits.store(0, Ordering::Relaxed);
        self.affinity_misses.store(0, Ordering::Relaxed);
        // 重试维度计数同步清零
        self.retried_attempts.store(0, Ordering::Relaxed);
        self.failed_requests_before_retry
            .store(0, Ordering::Relaxed);
        if let Ok(mut guard) = self.per_vendor.write() {
            guard.clear();
        }
        // 滑窗份额计数同步清空
        if let Ok(mut guard) = self.traffic.write() {
            guard.clear();
        }
        // 失败率滑窗同步清空
        if let Ok(mut guard) = self.vendor_windows.write() {
            guard.clear();
        }
        self.difficulty_fusion.store(0, Ordering::Relaxed);
        self.difficulty_rule_only.store(0, Ordering::Relaxed);
        self.difficulty_cache_hit.store(0, Ordering::Relaxed);
        self.difficulty_classifier_failed
            .store(0, Ordering::Relaxed);
        self.difficulty_features_degraded
            .store(0, Ordering::Relaxed);
        self.difficulty_breaker_opened.store(0, Ordering::Relaxed);
        self.difficulty_breaker_recovered
            .store(0, Ordering::Relaxed);
        self.difficulty_tier_low.store(0, Ordering::Relaxed);
        self.difficulty_tier_mid.store(0, Ordering::Relaxed);
        self.difficulty_tier_high.store(0, Ordering::Relaxed);
        self.difficulty_latency_ns_total.store(0, Ordering::Relaxed);
        self.difficulty_latency_count.store(0, Ordering::Relaxed);
    }
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 单个厂商的指标
#[derive(Debug, Clone, Serialize)]
pub struct VendorMetrics {
    pub vendor: String,
    /// O2 缓存命中率观测：厂商维度缓存读 token 与输入总 token 累计
    pub cache_read_tokens_sum: u64,
    pub cache_input_tokens_sum: u64,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_duration_ns: u64,
    pub last_request_at: Option<i64>,
    pub last_error: Option<String>,
    /// EWMA 延迟（ms）— 近期延迟的指数滑动平均。
    /// **语义变化（changelog 声明）** 从此只由 record_e2e 喂入
    /// （e2e 别名，Peak-EWMA 平滑），不再混流 TTFT/排队口径的样本。
    pub ewma_latency_ms: f64,
    /// EWMA 错误率（`[0,1]`，α=0.2，成功=0/失败=1 的滑动平均）
    pub ewma_error_rate: f64,
    /// EWMA 样本数（0 = 尚无观测，ewma 字段无意义）— record_request 驱动
    ///（后对齐错误率道的样本数）
    pub ewma_samples: u64,
    /// 流式 TTFT EWMA（ms）— 口径 = send → 上游响应头（严格首
    /// token 网关侧不可得，响应头近似）
    pub ewma_ttft_ms: f64,
    /// 非流式端到端 EWMA（ms）— 口径 = enqueued_at → 完成（含排队）
    pub ewma_e2e_ms: f64,
    /// 排队段 EWMA（ms）— 口径 = enqueued_at → worker 取走
    pub ewma_queue_ms: f64,
    /// 三道各自的样本数（0 = 该道尚无观测）
    pub ttft_samples: u64,
    pub e2e_samples: u64,
    pub queue_samples: u64,
    /// Peak-EWMA 每道时间锚（Δt = now − last_*_at → α_dyn）。
    /// 私有观测态，不进 /admin/metrics 序列化。
    #[serde(skip)]
    last_ttft_at: Option<Instant>,
    #[serde(skip)]
    last_e2e_at: Option<Instant>,
    #[serde(skip)]
    last_queue_at: Option<Instant>,
}

impl VendorMetrics {
    fn new() -> Self {
        Self {
            vendor: String::new(),
            cache_read_tokens_sum: 0,
            cache_input_tokens_sum: 0,
            requests: 0,
            successes: 0,
            failures: 0,
            total_duration_ns: 0,
            last_request_at: None,
            last_error: None,
            ewma_latency_ms: 0.0,
            ewma_error_rate: 0.0,
            ewma_samples: 0,
            ewma_ttft_ms: 0.0,
            ewma_e2e_ms: 0.0,
            ewma_queue_ms: 0.0,
            ttft_samples: 0,
            e2e_samples: 0,
            queue_samples: 0,
            last_ttft_at: None,
            last_e2e_at: None,
            last_queue_at: None,
        }
    }

    fn with_vendor(vendor: &str) -> Self {
        let mut m = Self::new();
        m.vendor = vendor.to_string();
        m
    }

    /// O2：厂商维度缓存命中记账（prompt = 输入总量口径）
    pub fn record_cache_tokens(&mut self, cache_read: u64, input_total: u64) {
        self.cache_read_tokens_sum = self.cache_read_tokens_sum.saturating_add(cache_read);
        self.cache_input_tokens_sum = self.cache_input_tokens_sum.saturating_add(input_total);
    }

    /// 缓存命中率 ∈ \[0,1\]；无输入样本时 0（除零安全）
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.cache_input_tokens_sum == 0 {
            return 0.0;
        }
        self.cache_read_tokens_sum as f64 / self.cache_input_tokens_sum as f64
    }

    pub fn avg_duration_ms(&self) -> f64 {
        if self.requests == 0 {
            return 0.0;
        }
        (self.total_duration_ns as f64 / self.requests as f64) / 1_000_000.0
    }
}

/// 指标快照 — 可序列化返回给客户端
///
/// 新增字段：`affinity_hits`/`affinity_misses`/`affinity_hit_rate`
///（会话亲和命中率）与 `top1_share`/`share_entropy`（流量分布熵监控），
/// /admin/metrics 序列化自动带出。
#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    /// 难度定档观测子块（定档未接线时全零）
    pub difficulty: DifficultyMetricsSnapshot,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub error_rate: f64,
    pub avg_duration_ms: f64,
    pub vendors: Vec<VendorMetrics>,
    /// 会话亲和命中次数
    pub affinity_hits: u64,
    /// 会话亲和未命中次数
    pub affinity_misses: u64,
    /// 亲和命中率（`[0,1]`，无样本 0.0）
    pub affinity_hit_rate: f64,
    /// 滑窗 top-1 vendor 流量占比（`[0,1]`，空窗 0.0）
    pub top1_share: f64,
    /// 流量份额 Shannon 熵（归一 `[0,1]`；V≤1 为 0）
    pub share_entropy: f64,
}

/// 难度定档观测子块（/admin/metrics 快照字段）
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct DifficultyMetricsSnapshot {
    pub fusion: u64,
    pub rule_only: u64,
    pub cache_hit: u64,
    pub classifier_failed: u64,
    pub features_degraded: u64,
    pub breaker_opened: u64,
    pub breaker_recovered: u64,
    pub tier_low: u64,
    pub tier_mid: u64,
    pub tier_high: u64,
    /// 定档决策平均延迟（ms，纯路径含缓存查写；分类器耗时应另看上游）
    pub avg_latency_ms: f64,
}

fn now_unix_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_empty_metrics_snapshot() {
        let m = GlobalMetrics::new();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert_eq!(snap.successful_requests, 0);
        assert_eq!(snap.failed_requests, 0);
        assert_eq!(snap.error_rate, 0.0);
        assert_eq!(snap.avg_duration_ms, 0.0);
        assert!(snap.vendors.is_empty());
    }

    #[test]
    fn test_record_success_updates_counters() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(150));
        m.record_request("openai", true, Duration::from_millis(250));

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 2);
        assert_eq!(snap.successful_requests, 2);
        assert_eq!(snap.failed_requests, 0);
        assert_eq!(snap.error_rate, 0.0);
        // avg = (150 + 250) / 2 = 200ms
        assert!((snap.avg_duration_ms - 200.0).abs() < 1e-3);
    }

    #[test]
    fn test_record_failure_updates_error_rate() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("openai", false, Duration::from_millis(50));
        m.record_request("openai", false, Duration::from_millis(50));

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 3);
        assert_eq!(snap.successful_requests, 1);
        assert_eq!(snap.failed_requests, 2);
        assert!((snap.error_rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_per_vendor_metrics_tracked_separately() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("openai", true, Duration::from_millis(200));
        m.record_request("anthropic", false, Duration::from_millis(300));

        let snap = m.snapshot();
        assert_eq!(snap.vendors.len(), 2);

        let openai = snap
            .vendors
            .iter()
            .find(|v| v.vendor == "openai")
            .expect("openai metrics present");
        assert_eq!(openai.requests, 2);
        assert_eq!(openai.successes, 2);
        assert!((openai.avg_duration_ms() - 150.0).abs() < 1e-3);

        let anthropic = snap
            .vendors
            .iter()
            .find(|v| v.vendor == "anthropic")
            .expect("anthropic metrics present");
        assert_eq!(anthropic.requests, 1);
        assert_eq!(anthropic.failures, 1);
    }

    #[test]
    fn test_record_error_does_not_increment_request_count() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_error("openai", "timeout");

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.vendors.len(), 1);
        assert_eq!(snap.vendors[0].last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn test_reset_clears_all_state() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("anthropic", false, Duration::from_millis(50));
        assert_eq!(m.snapshot().total_requests, 2);

        m.reset();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert!(snap.vendors.is_empty());
    }

    #[test]
    fn test_concurrent_writes_are_safe() {
        let m = std::sync::Arc::new(GlobalMetrics::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let m_clone = m.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    m_clone.record_request("openai", i % 3 != 0, Duration::from_millis(10));
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 1000);
        assert_eq!(snap.successful_requests + snap.failed_requests, 1000);
    }

    #[test]
    fn test_error_rate_zero_when_no_requests() {
        let m = GlobalMetrics::new();
        assert_eq!(m.error_rate(), 0.0);
    }

    #[test]
    fn test_vendor_metrics_avg_duration_zero_when_no_requests() {
        let v = VendorMetrics::new();
        assert_eq!(v.avg_duration_ms(), 0.0);
    }

    // ============ tests：EWMA 延迟/错误率 ============

    /// 连续 100→200→400ms：ewma 逐次上升且新值权重大于全生命期均值；
    /// 等价 α=0.2 的采样间隔由 Peak-EWMA 反解 Δt ≈ 2231.4ms（now 注入）
    #[test]
    fn test_ewma_latency_rises_and_weights_recent_samples() {
        let m = GlobalMetrics::new();
        let tau = DEFAULT_EWMA_TAU_MS as f64;
        // α_dyn = 0.2 的时间步长：Δt = −τ·ln(0.8)
        let step = Duration::from_secs_f64(-tau * (0.8f64.ln()) / 1000.0);
        let t0 = Instant::now();

        m.record_e2e("v", Duration::from_millis(100), t0);
        let (ewma, _) = m.vendor_ewma("v").expect("ewma after first record");
        assert!(
            (ewma - 100.0).abs() < 1e-9,
            "first sample seeds ewma: {ewma}"
        );

        m.record_e2e("v", Duration::from_millis(200), t0 + step);
        let (ewma, _) = m.vendor_ewma("v").unwrap();
        // α×200 + (1-α)×100 = 40 + 80 = 120
        assert!((ewma - 120.0).abs() < 1e-6, "second sample: {ewma}");

        m.record_e2e("v", Duration::from_millis(400), t0 + step + step);
        let (ewma, _) = m.vendor_ewma("v").unwrap();
        // α×400 + (1-α)×120 = 80 + 96 = 176（< 全生命期均值 233.3 → 近期样本
        // 按指数衰减，不被旧值平均拖平）
        assert!((ewma - 176.0).abs() < 1e-5, "third sample: {ewma}");
        assert!(
            ewma < (100.0 + 200.0 + 400.0) / 3.0,
            "ewma must weight recent samples more than the all-time mean"
        );
    }

    /// 失败样本推高 ewma_error_rate，成功样本拉回 — α 平滑语义。
    ///
    /// 评审 major 修复后错误率改经快照字段读取（vendor_ewma 现以 e2e
    /// 样本判定 Some/None，纯 record_request 历史不再返回元组）。
    #[test]
    fn test_ewma_error_rate_tracks_recent_failures() {
        let m = GlobalMetrics::new();
        m.record_request("v", false, Duration::from_millis(10));
        let err = m.snapshot().vendors[0].ewma_error_rate;
        assert!(
            (err - 1.0).abs() < 1e-9,
            "first failure seeds error ewma: {err}"
        );

        m.record_request("v", true, Duration::from_millis(10));
        let err = m.snapshot().vendors[0].ewma_error_rate;
        // α×0 + (1-α)×1 = 0.8
        assert!((err - 0.8).abs() < 1e-9, "success decays error ewma: {err}");
    }

    /// 无观测的 vendor 返回 None（路由对冷启动厂商走中性因子，不惩罚）。
    /// 窗口访问器 — 手灌 n 个样本返回 n（60s 滑窗内）；
    /// 未记录 vendor 返回 0。
    #[test]
    fn vendor_window_total_matches_samples() {
        let m = GlobalMetrics::new();
        assert_eq!(m.vendor_window_total("v"), 0);
        for _ in 0..5 {
            m.record_traffic_at("v", Instant::now());
        }
        assert_eq!(m.vendor_window_total("v"), 5);
        assert_eq!(m.vendor_window_total("other"), 0);
    }

    #[test]
    fn test_vendor_ewma_none_without_samples() {
        let m = GlobalMetrics::new();
        assert!(m.vendor_ewma("cold-vendor").is_none());
    }

    /// 评审 major 回归：vendor_ewma 以 e2e 样本判定 Some/None——纯流式
    /// vendor（生产只有 record_request + record_ttft 落账，流式失败仅
    /// record_request）不得以 0.0 初始 ewma_latency_ms 冒充「延迟数据」，
    /// 否则 w_latency>0 时 latency_factor=ref/(ref+0)=1.0 恒满分、Po2C
    /// latency_score=0.0 恒胜，混部舰队整体倾斜到流式 vendor。
    #[test]
    fn vendor_ewma_gated_on_e2e_samples() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        // 纯流式形态：成败记账 + TTFT 道，无任何 e2e 落账
        m.record_request("v", false, Duration::from_millis(120));
        m.record_ttft("v", Duration::from_millis(50), t0);
        m.record_request("v", true, Duration::from_millis(80));

        assert!(
            m.vendor_ewma("v").is_none(),
            "无 e2e 样本必须返回 None（0.0 初始值不是延迟数据）"
        );
        assert!(m.vendor_ewma_e2e("v").is_none());
        // 错误道观测数据仍在快照（未被 vendor_ewma 的 None 吞掉）：
        // 失败种子 1.0 + 一次成功 → 0.2×0 + 0.8×1 = 0.8
        assert!(
            (m.snapshot().vendors[0].ewma_error_rate - 0.8).abs() < 1e-9,
            "错误率道不受延迟道 None 判定影响"
        );

        // 首笔 e2e 落账后恢复 Some（别名 == e2e 值）
        m.record_e2e("v", Duration::from_millis(200), t0 + Duration::from_secs(1));
        let (latency, _) = m.vendor_ewma("v").expect("e2e 样本后恢复 Some");
        assert!((latency - 200.0).abs() < 1e-9);
    }

    // ============ tests：口径分道 + Peak-EWMA ============

    /// 三口径互不混流：同一 vendor 分别记 ttft/e2e/queue → 三字段独立，
    /// 各道样本数独立；后续 ttft 采样不动 e2e/queue。
    #[test]
    fn latency_lanes_are_independent() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_ttft("v", Duration::from_millis(50), t0);
        m.record_e2e("v", Duration::from_millis(200), t0);
        m.record_queue("v", Duration::from_millis(10), t0);

        let v = &m.snapshot().vendors[0];
        assert!((v.ewma_ttft_ms - 50.0).abs() < 1e-9, "ttft 首样本种子");
        assert!((v.ewma_e2e_ms - 200.0).abs() < 1e-9, "e2e 首样本种子");
        assert!((v.ewma_queue_ms - 10.0).abs() < 1e-9, "queue 首样本种子");
        assert_eq!(v.ttft_samples, 1);
        assert_eq!(v.e2e_samples, 1);
        assert_eq!(v.queue_samples, 1);

        // 后续 ttft 采样只动 ttft 道
        m.record_ttft(
            "v",
            Duration::from_millis(150),
            t0 + Duration::from_secs(60),
        );
        let v = &m.snapshot().vendors[0];
        assert!(
            (v.ewma_ttft_ms - 50.0).abs() > 1.0 && v.ewma_ttft_ms > 100.0,
            "久隔 60s 的 ttft 样本权重趋 1: {}",
            v.ewma_ttft_ms
        );
        assert!(
            (v.ewma_e2e_ms - 200.0).abs() < 1e-9,
            "e2e 不受 ttft 采样影响"
        );
        assert!(
            (v.ewma_queue_ms - 10.0).abs() < 1e-9,
            "queue 不受 ttft 采样影响"
        );
        assert_eq!(v.ttft_samples, 2);
        assert_eq!(v.e2e_samples, 1);
        assert_eq!(v.queue_samples, 1);
    }

    /// Peak-EWMA 时间衰减（注入 now 断言精确值）：同 vendor 样本间隔 60s
    /// → α = 1−exp(−60000/10000) = 1−e⁻⁶，旧值权重 e⁻⁶ ≈ 0.25%——新样本
    /// 权重趋 1（快速反映恢复）；密集采样（Δt≪τ）→ α≈Δt/τ 平滑抗噪。
    #[test]
    fn peak_ewma_decays_with_time_gap_and_smooths_dense_samples() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_e2e("v", Duration::from_millis(100), t0);

        // 间隔 60s（=6τ）后采样 1000ms：ewma = 1000 − 900·e⁻⁶ ≈ 997.77
        let t1 = t0 + Duration::from_secs(60);
        m.record_e2e("v", Duration::from_millis(1000), t1);
        let e = m.vendor_ewma("v").unwrap().0;
        let expected = 1000.0 - 900.0 * (-6.0f64).exp();
        assert!(
            (e - expected).abs() < 1e-6,
            "60s 间隔后旧值权重必须趋 0: {e} vs {expected}"
        );
        assert!(e > 990.0, "久未采样后新样本权重趋 1: {e}");

        // 密集采样（10ms ≪ τ=10s）：α = 1−e^(-0.001) ≈ 0.001，5000ms 只挪 ~5ms
        let alpha = 1.0 - (-0.001f64).exp();
        m.record_e2e(
            "v",
            Duration::from_millis(5000),
            t1 + Duration::from_millis(10),
        );
        let e2 = m.vendor_ewma("v").unwrap().0;
        let expected2 = alpha * 5000.0 + (1.0 - alpha) * expected;
        assert!(
            (e2 - expected2).abs() < 1e-6,
            "密集采样平滑: {e2} vs {expected2}"
        );
        assert!(
            e2 < expected + 5.0,
            "密集采样下单个离群样本只能小幅移动 ewma: {e2} (prev {expected})"
        );
    }

    /// τ 热同步：set_ewma_tau_ms 后下一次采样用新 τ（60s 间隔、τ=1s →
    /// α = 1−e⁻⁶⁰ ≈ 1，旧值权重≈0）；τ=0 为极限「完全采新样本」。
    #[test]
    fn ewma_tau_hot_sync_changes_decay_rate() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_e2e("v", Duration::from_millis(100), t0);

        m.set_ewma_tau_ms(1_000); // τ=1s，60s 间隔 = 60τ
        m.record_e2e(
            "v",
            Duration::from_millis(1000),
            t0 + Duration::from_secs(60),
        );
        let e = m.vendor_ewma("v").unwrap().0;
        assert!(
            (e - 1000.0).abs() < 1e-9,
            "τ=1s 下 60s 间隔旧值权重 exp(-60)≈0: {e}"
        );

        m.set_ewma_tau_ms(0); // 极限：完全采新样本
        m.record_e2e(
            "v",
            Duration::from_millis(42),
            t0 + Duration::from_secs(120),
        );
        let e = m.vendor_ewma("v").unwrap().0;
        assert!((e - 42.0).abs() < 1e-9, "τ=0 极限语义：样本直接替换: {e}");
    }

    /// 分道锁定：record_request 不再喂延迟道（口径分道本体），错误率道
    /// 仍由它驱动；e2e 道样本数为 0 时 vendor_ewma_e2e 返回 None。
    ///
    /// 评审 major 修复后错误率改经快照字段读取（vendor_ewma 的 Some/None
    /// 以 e2e 样本判定，纯 record_request 历史返回 None 而非 (0.0, err)）。
    #[test]
    fn record_request_no_longer_feeds_latency_lanes() {
        let m = GlobalMetrics::new();
        m.record_request("v", true, Duration::from_millis(123));
        let v = &m.snapshot().vendors[0];
        assert_eq!(
            v.ewma_latency_ms, 0.0,
            "record_request 的 duration 口径因路径而异，recId=8 后不再喂延迟 EWMA"
        );
        assert_eq!(v.e2e_samples, 0);
        assert!(m.vendor_ewma_e2e("v").is_none(), "无 e2e 样本 → 无延迟数据");
        assert!(
            m.vendor_ewma("v").is_none(),
            "vendor_ewma 同判定：无 e2e 样本返回 None（评审 major）"
        );
        // 错误率道不受影响（快照字段照常更新）
        assert!((v.ewma_error_rate - 0.0).abs() < 1e-9);
        m.record_request("v", false, Duration::from_millis(10));
        let err = m.snapshot().vendors[0].ewma_error_rate;
        assert!((err - 0.2).abs() < 1e-9, "错误率道仍由 record_request 喂");

        // e2e 道有样本后 vendor_ewma_e2e 才返回 Some
        m.record_e2e("v", Duration::from_millis(80), Instant::now());
        assert!((m.vendor_ewma_e2e("v").unwrap() - 80.0).abs() < 1e-9);
    }

    /// ewma_latency_ms 兼容别名：record_e2e 同时喂两字段（值恒一致），
    /// /admin/metrics 外部消费方读数不变（语义收窄为 e2e 已由 changelog 声明）。
    #[test]
    fn ewma_latency_ms_is_e2e_alias() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_e2e("v", Duration::from_millis(120), t0);
        m.record_e2e(
            "v",
            Duration::from_millis(280),
            t0 + Duration::from_millis(2231),
        );
        let v = &m.snapshot().vendors[0];
        assert!(
            (v.ewma_latency_ms - v.ewma_e2e_ms).abs() < 1e-12,
            "别名字段必须与 e2e 道逐位一致: {} vs {}",
            v.ewma_latency_ms,
            v.ewma_e2e_ms
        );
        // 别名不随 ttft/queue 道变动
        m.record_ttft(
            "v",
            Duration::from_millis(999),
            t0 + Duration::from_secs(60),
        );
        m.record_queue(
            "v",
            Duration::from_millis(999),
            t0 + Duration::from_secs(60),
        );
        let v = &m.snapshot().vendors[0];
        assert!((v.ewma_latency_ms - v.ewma_e2e_ms).abs() < 1e-12);
    }

    // ============ tests：top-1 占比与流量份额熵 ============

    /// ① 3 vendor 均分 → entropy≈1.0、top1≈0.333（归一熵的均匀分布基准）。
    #[test]
    fn test_three_vendor_even_split_full_entropy() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for vendor in ["a", "b", "c"] {
            for _ in 0..10 {
                m.record_traffic_at(vendor, t0);
            }
        }
        let shares = m.traffic_shares();
        assert_eq!(shares.len(), 3);
        for (_, share) in &shares {
            assert!((share - 1.0 / 3.0).abs() < 1e-9);
        }
        assert!((m.top1_share() - 1.0 / 3.0).abs() < 1e-9);
        assert!(
            (m.share_entropy() - 1.0).abs() < 1e-9,
            "3 路均分的归一熵必须是 1.0，实际 {}",
            m.share_entropy()
        );
        assert!(!m.should_alert(), "均分分布不触发告警");
    }

    /// ② 单 vendor 独占 → entropy==0、top1==1.0（V=1 时熵定义为 0，防除零）。
    #[test]
    fn test_single_vendor_monopoly_zero_entropy() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for _ in 0..7 {
            m.record_traffic_at("only", t0);
        }
        assert!((m.top1_share() - 1.0).abs() < 1e-9);
        assert_eq!(m.share_entropy(), 0.0, "V=1 定义熵为 0");
    }

    /// ③ 空窗口 → top1==0 且 should_alert()==false（无流量不算崩溃，语义锁定）。
    #[test]
    fn test_empty_window_zero_top1_no_alert() {
        let m = GlobalMetrics::new();
        assert_eq!(m.top1_share(), 0.0);
        assert_eq!(m.share_entropy(), 0.0);
        assert!(m.traffic_shares().is_empty());
        assert!(!m.should_alert());
    }

    /// ④ top1=0.95、活跃 vendor=3 → should_alert()==true（崩溃告警阈值）。
    #[test]
    fn test_concentrated_three_vendor_alerts() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        // 95 / 3 / 2 → top1 = 0.95 > 0.9，活跃 vendor = 3 > 2
        for _ in 0..95 {
            m.record_traffic_at("dominant", t0);
        }
        for _ in 0..3 {
            m.record_traffic_at("minor-a", t0);
        }
        for _ in 0..2 {
            m.record_traffic_at("minor-b", t0);
        }
        assert!((m.top1_share() - 0.95).abs() < 1e-9);
        assert!(m.should_alert(), "top1=0.95 且 3 vendor 活跃必须告警");

        // 对照：同样 0.95 但只有 2 个活跃 vendor → 不告警（二选一正常路由）
        let m2 = GlobalMetrics::new();
        for _ in 0..95 {
            m2.record_traffic_at("dominant", t0);
        }
        for _ in 0..5 {
            m2.record_traffic_at("minor", t0);
        }
        assert!((m2.top1_share() - 0.95).abs() < 1e-9);
        assert!(!m2.should_alert(), "活跃 vendor ≤ 2 的高占比属正常二选一");
    }

    /// ⑤ 滑窗推进：只灌前 30s 的量、推进假时钟 60s+ → 旧桶全部过期衰减
    /// 出窗（惰性清零语义），份额让位给新流量。
    #[test]
    fn test_sliding_window_advances_and_decays_old_traffic() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        // vendor-a：先灌 t=0s 的 10 次（桶 0），再灌 t=30s 的 10 次（桶 5）
        // —— 滑窗要求单调时钟，分阶段顺序灌入
        for _ in 0..10 {
            m.record_traffic_at("a", t0);
        }
        for _ in 0..10 {
            m.record_traffic_at("a", t0 + Duration::from_secs(30));
        }
        assert!((m.top1_share() - 1.0).abs() < 1e-9);
        assert_eq!(m.traffic_shares()[0].0, "a");

        // 推进到 t=65s：vendor-b 灌 10。桶宽 6s，t=65s 落 abs 桶 10（环回桶 0）；
        // a 最后写入 abs 桶 5，清零区间 (5, 10] = 桶 6,7,8,9,0 —— a 的两个桶
        //（0 与 5）中桶 0 被清，桶 5 保留？不：a 的 t=30s 落 abs 桶 5 = 环形桶 5。
        // (5, 10] 不含 5 —— 但 t=0s 的桶 0（abs 0 → 环 0）被清。
        let t65 = t0 + Duration::from_secs(65);
        for _ in 0..10 {
            m.record_traffic_at("b", t65);
        }
        let shares: Vec<(String, f64)> = m.traffic_shares();
        let share_of = |id: &str| {
            shares
                .iter()
                .find(|(v, _)| v == id)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        // a 剩桶 5 的 10 次，b 新增 10 次 → 各 0.5（t=0 的量已衰减出窗）
        assert!(
            (share_of("a") - 0.5).abs() < 1e-9 && (share_of("b") - 0.5).abs() < 1e-9,
            "t=0 的 10 次应衰减出窗，a/b 各半：{shares:?}"
        );

        // 再推进到 t=130s 只灌 b：a 最后写入 abs 桶 5，当前 abs=21，跨度 16 > 10
        // → 超一整圈全清，a 全部过期 → 份额归零。
        let t130 = t0 + Duration::from_secs(130);
        for _ in 0..10 {
            m.record_traffic_at("b", t130);
        }
        let shares: Vec<(String, f64)> = m.traffic_shares();
        assert_eq!(shares.len(), 1, "a 的旧流量应全部出窗: {shares:?}");
        assert_eq!(shares[0].0, "b");
        assert!((m.top1_share() - 1.0).abs() < 1e-9);
    }

    /// 回归：亚秒写入间隔下窗口必须照常轮转（旧实现的增量推进残差被截断
    /// 丢弃会冻结窗口退化为全生命期累计，窗内 == 总写入）
    #[test]
    fn sliding_counter_rotates_under_subsecond_writes() {
        let t0 = Instant::now();
        let mut c = SlidingCounter::new(t0);
        let mut t = t0;
        let mut total = 0u64;
        while t < t0 + Duration::from_secs(61) {
            c.record_at(t);
            total += 1;
            t += Duration::from_millis(500);
        }
        assert_eq!(total, 122, "0.5s 步进 61s = 122 次写入");
        assert!(
            c.window_total() < total,
            "亚秒写入间隔下窗口必须轮转（窗内 {} < 总写入 {}；两者相等即窗口冻结退化）",
            c.window_total(),
            total
        );
        // 精确语义：abs 0（t<6s）的 12 条随推进清零，其余 110 条在窗内
        //（t=60/60.5 的 2 条落 abs 10 = 环 0，本就含在总数内）
        assert_eq!(c.window_total(), 110, "窗内计数须与轮转语义逐位一致");
    }

    /// 评审实证场景（major#1 的多 vendor 版本）：b 亚秒写 [0,30s)、a 亚秒
    /// 写 [30,60s)、a 续写 [60,90s)，读锚 t≈89.5s 时 b 的 60s 前流量全部
    /// 衰减出窗（份额归零）；a 持续写入保持份额 1.0。
    #[test]
    fn subsecond_scenario_b_decays_once_global_clock_advances() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        let mut t = t0;
        while t < t0 + Duration::from_secs(90) {
            if t < t0 + Duration::from_secs(30) {
                m.record_traffic_at("b", t);
                m.record_traffic_at("b", t + Duration::from_millis(250));
            } else {
                m.record_traffic_at("a", t);
                m.record_traffic_at("a", t + Duration::from_millis(250));
            }
            t += Duration::from_millis(500);
        }
        let shares = m.traffic_shares();
        let share_of = |id: &str| {
            shares
                .iter()
                .find(|(v, _)| v == id)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        assert!(
            share_of("b") == 0.0,
            "b 最后写入 t≈30s，读锚 t≈89.5s 时距其绝对桶序已跨 10+ 桶，必须全部出窗: {shares:?}"
        );
        assert!(
            (share_of("a") - 1.0).abs() < 1e-9,
            "持续写入的 a 独占份额: {shares:?}"
        );
        assert_eq!(m.top1_share(), 1.0);
    }

    /// affinity hit/miss 计数与 hit_rate；reset 同步清零。
    #[test]
    fn test_affinity_hit_rate_and_reset() {
        let m = GlobalMetrics::new();
        assert_eq!(m.affinity_hit_rate(), 0.0, "无样本 hit_rate=0.0");

        m.record_affinity_hit();
        m.record_affinity_hit();
        m.record_affinity_miss();
        assert!((m.affinity_hit_rate() - 2.0 / 3.0).abs() < 1e-9);

        m.reset();
        assert_eq!(m.affinity_hit_rate(), 0.0);
        let snap = m.snapshot();
        assert_eq!(snap.affinity_hits, 0);
        assert_eq!(snap.affinity_misses, 0);
        // reset 同步清滑窗
        assert!(m.traffic_shares().is_empty());
    }

    // ============ tests：失败率滑窗 ============

    /// 窗口失败率基础口径：注入 now 连记 3 成功 3 失败 → (6, 3)。
    #[test]
    fn vendor_failure_window_counts_successes_and_failures() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            m.record_vendor_outcome_at("v", true, t0);
        }
        for _ in 0..3 {
            m.record_vendor_outcome_at("v", false, t0);
        }
        assert_eq!(
            m.vendor_failure_window("v"),
            (6, 3),
            "total 记全部结局、failures 只记失败"
        );
        // 无样本 vendor 返回 (0, 0)
        assert_eq!(m.vendor_failure_window("cold"), (0, 0));
        // reset 同步清失败率滑窗
        m.reset();
        assert_eq!(m.vendor_failure_window("v"), (0, 0));
    }

    /// 滑窗推进：vendor-a 在 t=0 全失败，另一 vendor 在 t=65s 写入推进
    /// 全局读锚 → a 的旧桶全部衰减出窗（失败率不滞留）。
    #[test]
    fn vendor_failure_window_decays_with_global_clock() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for _ in 0..6 {
            m.record_vendor_outcome_at("a", false, t0);
        }
        assert_eq!(m.vendor_failure_window("a"), (6, 6));

        // t=65s：vendor-b 写入（滑窗要求单调时钟，全局读锚被推到 t=65s）
        for _ in 0..2 {
            m.record_vendor_outcome_at("b", true, t0 + Duration::from_secs(65));
        }
        // a 最后写入 t=0（abs 桶 0），读锚 t=65s（abs 桶 10）→ 跨度 10 =
        // 一整圈全清 → (0, 0)；b 自己 (2, 0)
        assert_eq!(
            m.vendor_failure_window("a"),
            (0, 0),
            "旧失败必须随全局时间出窗"
        );
        assert_eq!(m.vendor_failure_window("b"), (2, 0));
    }

    /// 429 感知对照断言：RateLimited 计入失败率滑窗
    ///（failure window 视角是失败），而 CB 分类器对其无感
    ///（error_map.rs GateErrorClassifier 的既有语义——独立计数、
    /// 不影响 CB 自身的 failure_count）。
    #[cfg(feature = "resilience")]
    #[test]
    fn rate_limited_counts_in_failure_window_but_not_circuit_breaker() {
        use crate::vendor::resilience::error_map::GateErrorClassifier;
        use limiteron::circuit::FailureClassifier as _;

        let classifier = GateErrorClassifier;
        assert!(
            !classifier.is_counted_as_failure(&crate::error::GateError::RateLimited {
                vendor: "v".into()
            }),
            "CB 分类器对 RateLimited 必须无感（既有语义，对照基线）"
        );

        // 失败率滑窗独立计数：429 按失败入窗（记账点把 RateLimited 记为
        // success=false，与本 API 的失败语义一致）
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_vendor_outcome_at("v", true, t0);
        m.record_vendor_outcome_at("v", false, t0); // 一次 429
        assert_eq!(
            m.vendor_failure_window("v"),
            (2, 1),
            "429 必须计入失败率滑窗（绕过 CB 分类器，独立计数）"
        );
    }

    // ============ tests：重试维度计数与守门器视图 ============

    /// 模拟 3 次重试：retried_attempts==3；失败进重试候选路径计数独立
    /// 累计；reset 同步清零。
    #[test]
    fn retry_dimension_counters_track_and_reset() {
        let m = GlobalMetrics::new();
        assert_eq!(m.retried_attempts(), 0);
        assert_eq!(m.failed_requests_before_retry(), 0);
        for _ in 0..3 {
            m.record_retry_attempt();
        }
        m.record_failed_request_before_retry();
        m.record_failed_request_before_retry();
        assert_eq!(m.retried_attempts(), 3, "3 次 attempt+=1 各记一笔");
        assert_eq!(m.failed_requests_before_retry(), 2);
        m.reset();
        assert_eq!(m.retried_attempts(), 0);
        assert_eq!(m.failed_requests_before_retry(), 0);
    }

    /// 守门器视图：(r, f, λ) 数值断言 —— r=retried/failed_before_retry、
    /// f=全生命期失败率（record_request 驱动）、λ=流量窗总量/60；
    /// 无失败（分母 0）→ None。
    #[test]
    fn amplification_view_returns_guard_inputs() {
        let m = GlobalMetrics::new();
        assert!(m.amplification_view().is_none(), "无失败 → 无 r 可言");

        for _ in 0..6 {
            m.record_request("v", false, Duration::from_millis(1));
        }
        m.record_retry_attempt();
        m.record_retry_attempt();
        for _ in 0..3 {
            m.record_failed_request_before_retry();
        }
        let (r, f, lambda) = m.amplification_view().expect("有失败必须返回 Some");
        assert!((r - 2.0 / 3.0).abs() < 1e-9, "r=2/3: {r}");
        assert!((f - 1.0).abs() < 1e-9, "f=6/6: {f}");
        assert!((lambda - 0.1).abs() < 1e-9, "λ=6/60: {lambda}");
    }
}
