// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 定档分类器熔断降级状态机（F6）：closed →（连续失败达阈值）→ open →
//!（探测间隔到）→ 半开探测 → 成功回 closed / 失败回 open。纯状态机 +
//! 调用方注入时钟（`Instant` 入参），确定性可测。降级不抛错——调用方
//! 据 [`Availability`] 决定是否调用分类器，失败请求一律退纯规则。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// 分类器可调用
    Available,
    /// 熔断 open 到期，放行一次半开探测
    Probe,
    /// open 期：不要调用分类器，直接纯规则
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

/// 熔断状态机。阈值与探测间隔从配置构造（热读由调用方决定是否重建）
pub struct Guard {
    state: Mutex<State>,
    consecutive_failures: Mutex<u32>,
    /// 热读参数（orchestrator 每请求从 [routing.difficulty].classifier 刷新）
    failure_threshold: AtomicU32,
    probe_interval_secs: AtomicU64,
}

impl Guard {
    /// F3 场景（引擎加载失败）用 `Guard::always_open`：初始即 open，
    /// 不探测（probe 恒 Unavailable），直到进程重启重新加载
    pub fn new(failure_threshold: u32, probe_interval_secs: u64) -> Self {
        Self {
            state: Mutex::new(State::Closed),
            consecutive_failures: Mutex::new(0),
            failure_threshold: AtomicU32::new(failure_threshold.max(1)),
            probe_interval_secs: AtomicU64::new(probe_interval_secs),
        }
    }

    pub fn always_open() -> Self {
        Self {
            state: Mutex::new(State::Open {
                opened_at: Instant::now(),
            }),
            consecutive_failures: Mutex::new(0),
            failure_threshold: AtomicU32::new(1),
            // u64::MAX 秒 = 实际不探测（F3 常驻降级语义）
            probe_interval_secs: AtomicU64::new(u64::MAX),
        }
    }

    /// 热更参数（每次定档前由 orchestrator 从配置刷新；原子写，无锁）
    pub fn update_params(&self, failure_threshold: u32, probe_interval_secs: u64) {
        self.failure_threshold
            .store(failure_threshold.max(1), Ordering::Relaxed);
        self.probe_interval_secs
            .store(probe_interval_secs, Ordering::Relaxed);
    }

    /// 请求面前查询：closed 放行；open 到期转半开放行一次探测；
    /// 其余拒绝。迁移在查询时惰性发生（无后台任务）
    pub fn check(&self, now: Instant) -> Availability {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            State::Closed => Availability::Available,
            State::Open { opened_at } => {
                let probe_interval =
                    Duration::from_secs(self.probe_interval_secs.load(Ordering::Relaxed));
                if now.duration_since(opened_at) >= probe_interval {
                    *state = State::HalfOpen;
                    Availability::Probe
                } else {
                    Availability::Unavailable
                }
            }
            State::HalfOpen => Availability::Probe,
        }
    }

    /// 探测/调用成功：回 closed、清零计数。返回 true 表示发生了
    /// open→closed 恢复迁移（供调用方记恢复事件）
    pub fn record_success(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let recovered = matches!(*state, State::Open { .. } | State::HalfOpen);
        *state = State::Closed;
        *self
            .consecutive_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = 0;
        recovered
    }

    /// 调用/探测失败：closed 累计计数达阈值转 open；半开失败直接回 open。
    /// 返回 true 表示发生了 →open 迁移（供调用方记熔断事件）
    pub fn record_failure(&self, now: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            State::HalfOpen => {
                *state = State::Open { opened_at: now };
                true
            }
            State::Open { .. } => false,
            State::Closed => {
                let mut failures = self
                    .consecutive_failures
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *failures += 1;
                if *failures >= self.failure_threshold.load(Ordering::Relaxed) {
                    *state = State::Open { opened_at: now };
                    *failures = 0;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// 当前是否处于降级态（观测面：/admin/route/difficulty-explain 的
    /// classifier.breaker 字段按需读取；无独立 gauge——事件计数 + 快照读足够）
    pub fn is_degraded(&self, now: Instant) -> bool {
        self.check(now) == Availability::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> Guard {
        Guard::new(3, 30)
    }

    #[test]
    fn closed_below_threshold_stays_available() {
        let g = guard();
        let now = Instant::now();
        assert_eq!(g.check(now), Availability::Available);
        assert!(!g.record_failure(now));
        assert!(!g.record_failure(now));
        assert_eq!(g.check(now), Availability::Available, "未达阈值仍放行");
        assert!(!g.is_degraded(now));
    }

    #[test]
    fn threshold_crossing_opens_and_refuses() {
        let g = guard();
        let now = Instant::now();
        g.record_failure(now);
        g.record_failure(now);
        assert!(g.record_failure(now), "第 3 次失败触发 →open 迁移");
        assert_eq!(g.check(now), Availability::Unavailable, "open 期拒绝");
        assert!(g.is_degraded(now));
    }

    #[test]
    fn probe_after_interval_success_recovers() {
        let g = guard();
        let t0 = Instant::now();
        for _ in 0..3 {
            g.record_failure(t0);
        }
        let t1 = t0 + Duration::from_secs(29);
        assert_eq!(g.check(t1), Availability::Unavailable, "间隔未到不探测");
        let t2 = t0 + Duration::from_secs(30);
        assert_eq!(g.check(t2), Availability::Probe, "到期放行探测");
        assert!(g.record_success(), "探测成功 →open 恢复迁移");
        assert_eq!(g.check(t2), Availability::Available);
    }

    #[test]
    fn probe_failure_reopens() {
        let g = guard();
        let t0 = Instant::now();
        for _ in 0..3 {
            g.record_failure(t0);
        }
        let t1 = t0 + Duration::from_secs(30);
        assert_eq!(g.check(t1), Availability::Probe);
        assert!(g.record_failure(t1), "探测失败 →回 open 迁移");
        assert_eq!(g.check(t1), Availability::Unavailable);
    }

    #[test]
    fn success_resets_failure_streak() {
        let g = guard();
        let now = Instant::now();
        g.record_failure(now);
        g.record_failure(now);
        assert!(!g.record_success());
        assert!(!g.record_failure(now), "清零后重新累计：第 1 次");
        assert!(!g.record_failure(now), "第 2 次");
        assert!(g.record_failure(now), "第 3 次达阈值 → open");
    }

    #[test]
    fn always_open_never_probes() {
        let g = Guard::always_open();
        let now = Instant::now();
        assert_eq!(g.check(now), Availability::Unavailable);
        // Duration::MAX 探测间隔：即时钟大幅前进也不放行探测
        let later = now + Duration::from_secs(3600 * 24 * 365);
        assert_eq!(g.check(later), Availability::Unavailable);
        assert_eq!(g.check(later), Availability::Unavailable);
    }

    #[test]
    fn params_hot_update_takes_effect() {
        let g = Guard::new(3, 3600);
        let now = Instant::now();
        g.record_failure(now);
        g.record_failure(now);
        // 阈值热调低：2 次失败即 open
        g.update_params(2, 30);
        assert!(g.record_failure(now), "热更后的阈值立即生效");
        assert_eq!(g.check(now), Availability::Unavailable);
    }
}
