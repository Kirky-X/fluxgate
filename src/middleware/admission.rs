// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 拥塞感知优先级准入 — 把 429 的决定权从先到先得交给优先级。
//!
//! 拥塞信号 = Σ in_flight / Σ 并发上限 ≥ `[admission].congestion_threshold` 时，
//! 低于「当前在途最小档」的新请求直接 429 + Retry-After（不排队）；同档/高档照常
//! 放行（同档内保持先到先得）。健康检查在 rate_limit_middleware 头部白名单放行。
//! 默认关闭（`[admission].enabled` 默认 false）；per-tier 在途计数经 guard 的
//! Drop 释放（panic 展开亦不泄漏）。优先级公式见 [`admission_priority`]。

use crate::config::{AdmissionSection, AuthTier, ConfigManager};
use crate::middleware::auth::{extract_api_key, ApiAuthState};
use crate::vendor::VendorManager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// 档位数（dedicated/guaranteed/elastic/spot）
pub const TIER_COUNT: usize = 4;

/// EWMA 定点小数位（值 × 2¹⁶）
const FIXED_POINT: f64 = 65536.0;
/// burst EWMA 平滑系数（到达速率样本）
const BURST_EWMA_ALPHA: f64 = 0.5;
/// debt EWMA 平滑系数（被拒样本 1 / 放行样本 0）
const DEBT_EWMA_ALPHA: f64 = 0.25;
/// per-key 表条目硬上限（auth 关闭时伪造 key 的内存放大守卫）；超限后新 key 退化共享单桶
const MAX_KEY_STATS: usize = 1024;
/// 溢出共享桶的键
const OVERFLOW_KEY: &str = "__overflow__";
/// 拥塞 429 的 Retry-After 秒数（保守重试间隔）
pub const RETRY_AFTER_SECS: u64 = 1;

/// 档位 → 下标（权重降序：dedicated=0 … spot=3）
fn tier_index(tier: AuthTier) -> usize {
    match tier {
        AuthTier::Dedicated => 0,
        AuthTier::Guaranteed => 1,
        AuthTier::Elastic => 2,
        AuthTier::Spot => 3,
    }
}

/// 档位基准权重 — dedicated/guaranteed/elastic/spot =
/// 1000/100/1/0.1（数量级隔离：同拥塞下 spot 永远让位于 dedicated）。
pub fn tier_weight(tier: AuthTier) -> f64 {
    match tier {
        AuthTier::Dedicated => 1000.0,
        AuthTier::Guaranteed => 100.0,
        AuthTier::Elastic => 1.0,
        AuthTier::Spot => 0.1,
    }
}

/// 优先级纯函数：`w_tier·(1+α_slo·ℓ*)⁻¹·(1+α_burst·b)⁻¹·(1+α_debt·d)⁻¹`。
/// ℓ* 恒 0（SLO 缺口无数据源，该项恒 1）；burst/debt 负值按 0（防御）。
pub fn admission_priority(
    section: &AdmissionSection,
    tier_weight: f64,
    burst: f64,
    debt: f64,
) -> f64 {
    let l_star = 0.0; // SLO 缺口维度无数据源（范围外），该项恒 1
    tier_weight
        / (1.0 + section.alpha_slo * l_star)
        / (1.0 + section.alpha_burst * burst.max(0.0))
        / (1.0 + section.alpha_debt * debt.max(0.0))
}

/// 请求的档位判定 — 请求 key（明文）经热配置 `[auth]` 段的
/// `keys` 子表查档（`ApiAuthState::tier_for_key`，与认证同一摘要比较）；
/// 无 key / `[auth]` 缺失或关闭 → 缺省档 elastic。
pub fn tier_for_request(config: &ConfigManager, headers: &axum::http::HeaderMap) -> AuthTier {
    let Some(key) = extract_api_key(headers) else {
        return AuthTier::Elastic;
    };
    match config.read().auth.as_ref() {
        Some(section) if section.enabled => ApiAuthState::from_section(section).tier_for_key(&key),
        _ => AuthTier::Elastic,
    }
}

/// per-key EWMA 状态（定点 AtomicU64）
#[derive(Default)]
struct KeyStats {
    /// 到达速率 EWMA（req/s，定点）
    burst: AtomicU64,
    /// 被 shed 频度 EWMA（定点）
    debt: AtomicU64,
    /// 最近到达毫秒时间戳（0 = 无锚，首样本不更新速率）
    last_seen_ms: AtomicU64,
}

/// 准入状态（挂载于中间件 State，全局共享 Arc）：per-tier 在途计数 +
/// per-key burst/debt EWMA。
pub struct AdmissionState {
    config: Arc<ConfigManager>,
    vendor_manager: Arc<VendorManager>,
    /// per-tier 在途计数（下标 = [`tier_index`]；Arc 共享给 guard 的
    /// Drop 释放路径，避免 AdmissionState 自引用）
    tier_in_flight: Arc<[AtomicU64; TIER_COUNT]>,
    /// per-key EWMA 表（键 = key 摘要；条目有硬上限，超限共享溢出桶）
    key_stats: RwLock<HashMap<String, Arc<KeyStats>>>,
}

/// 准入放行票据 — 持有期间该档在途计数 +1，Drop（含 panic 展开）−1。
/// 作用域包住 handler 至响应返回；流式 body 传输期不计（粗分类定位）。
pub struct AdmissionGuard {
    counters: Arc<[AtomicU64; TIER_COUNT]>,
    index: usize,
}

impl std::fmt::Debug for AdmissionGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionGuard")
            .field("tier_index", &self.index)
            .finish()
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        self.counters[self.index].fetch_sub(1, Ordering::Relaxed);
    }
}

impl AdmissionState {
    pub fn new(config: Arc<ConfigManager>, vendor_manager: Arc<VendorManager>) -> Self {
        Self {
            config,
            vendor_manager,
            tier_in_flight: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
            key_stats: RwLock::new(HashMap::new()),
        }
    }

    /// 准入判定 — 返回 Ok(guard)（放行，调用方须持至响应返回）或 Err(retry_after_secs)
    /// （拥塞且优先级低于在途最小档 → 429）。`now_ms` 注入供测试驱动 EWMA。
    /// `[admission].enabled` 热读：false（默认）直接放行（guard 仍计入在途——纯内存观测）。
    pub fn admit(
        &self,
        tier: AuthTier,
        key_hash: &str,
        now_ms: u64,
    ) -> Result<AdmissionGuard, u64> {
        let section = self.config.read().admission.clone();
        let stats = self.stats_for(key_hash);
        self.record_arrival(&stats, now_ms);
        if !section.enabled {
            return Ok(self.guard(tier));
        }
        let (burst, debt) = self.key_ewma(key_hash);
        let priority = admission_priority(&section, tier_weight(tier), burst, debt);
        // 拥塞信号：网关侧并发占用率（容量 0 = 无实例，按无拥塞处理）
        let (in_flight, capacity) = self.vendor_manager.fleet_concurrency();
        let congested =
            capacity > 0 && in_flight as f64 / capacity as f64 >= section.congestion_threshold;
        let admitted = if !congested {
            true
        } else {
            match self.min_in_flight_weight() {
                // 无在途而信号拥塞（流式近似误差）→ 保守放行：没有可比较的在途档位
                None => true,
                Some(min_weight) => priority >= min_weight,
            }
        };
        if admitted {
            self.record_debt(&stats, false);
            Ok(self.guard(tier))
        } else {
            self.record_debt(&stats, true);
            Err(RETRY_AFTER_SECS)
        }
    }

    /// 放行票据构造（计数 +1）
    fn guard(&self, tier: AuthTier) -> AdmissionGuard {
        let index = tier_index(tier);
        self.tier_in_flight[index].fetch_add(1, Ordering::Relaxed);
        AdmissionGuard {
            counters: self.tier_in_flight.clone(),
            index,
        }
    }

    /// 在途最小档权重（无在途返回 None）
    fn min_in_flight_weight(&self) -> Option<f64> {
        let mut min: Option<f64> = None;
        for tier in [
            AuthTier::Dedicated,
            AuthTier::Guaranteed,
            AuthTier::Elastic,
            AuthTier::Spot,
        ] {
            if self.tier_in_flight[tier_index(tier)].load(Ordering::Relaxed) > 0 {
                let w = tier_weight(tier);
                min = Some(min.map_or(w, |m: f64| m.min(w)));
            }
        }
        min
    }

    /// per-key 状态取用（惰性创建 + 容量守卫：超限退化共享单桶）。
    fn stats_for(&self, key_hash: &str) -> Arc<KeyStats> {
        if let Some(s) = self
            .key_stats
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key_hash)
        {
            return s.clone();
        }
        let mut guard = self
            .key_stats
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(s) = guard.get(key_hash) {
            return s.clone();
        }
        let insert_key = if guard.len() >= MAX_KEY_STATS {
            OVERFLOW_KEY.to_string()
        } else {
            key_hash.to_string()
        };
        if let Some(s) = guard.get(&insert_key) {
            return s.clone();
        }
        let s = Arc::new(KeyStats::default());
        guard.insert(insert_key, s.clone());
        s
    }

    /// 到达样本 — burst EWMA += α(rate − burst)，rate = 1000/Δms req/s
    ///（瞬时到达速率）；首样本只记时间锚（无间隔可测）。
    fn record_arrival(&self, stats: &KeyStats, now_ms: u64) {
        let last = stats.last_seen_ms.swap(now_ms, Ordering::Relaxed);
        if last == 0 || now_ms <= last {
            return;
        }
        let dt_ms = now_ms.saturating_sub(last).max(1);
        let rate = 1000.0 / dt_ms as f64;
        let cur = stats.burst.load(Ordering::Relaxed) as f64 / FIXED_POINT;
        let next = (cur + BURST_EWMA_ALPHA * (rate - cur)).max(0.0);
        stats
            .burst
            .store((next * FIXED_POINT) as u64, Ordering::Relaxed);
    }

    /// shed 样本 — debt EWMA += α(sample − debt)，被拒 1 / 放行 0。
    fn record_debt(&self, stats: &KeyStats, shed: bool) {
        let cur = stats.debt.load(Ordering::Relaxed) as f64 / FIXED_POINT;
        let sample = if shed { 1.0 } else { 0.0 };
        let next = (cur + DEBT_EWMA_ALPHA * (sample - cur)).max(0.0);
        stats
            .debt
            .store((next * FIXED_POINT) as u64, Ordering::Relaxed);
    }

    /// per-key EWMA 快照（测试与 /admin 观测入口；未跟踪 key → (0.0, 0.0)）
    pub fn key_ewma(&self, key_hash: &str) -> (f64, f64) {
        let guard = self
            .key_stats
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.get(key_hash).or_else(|| guard.get(OVERFLOW_KEY)) {
            Some(s) => (
                s.burst.load(Ordering::Relaxed) as f64 / FIXED_POINT,
                s.debt.load(Ordering::Relaxed) as f64 / FIXED_POINT,
            ),
            None => (0.0, 0.0),
        }
    }

    /// 档位在途计数（测试与观测入口）
    pub fn tier_in_flight(&self, tier: AuthTier) -> u64 {
        self.tier_in_flight[tier_index(tier)].load(Ordering::Relaxed)
    }

    /// 热配置句柄 — 中间件取请求档位用（tier_for_request 读
    /// `[auth]` 段）；`[admission]`.enabled 的热读判定在 admit 内部完成。
    pub fn config_handle(&self) -> Arc<ConfigManager> {
        self.config.clone()
    }
}
