// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 缓存经济学亲和的 settle 决策与粘性记账。
//!
//! 「值不值得粘」的判定（R-affinity-002）与落盘契约（R-affinity-003）同居：
//! `AffinityWriter::settle` 是 api 层响应 usage 结算点的唯一入口，四模式
//! 语义与粘性记账都在本文件，`affinity.rs` 只留表本身（规则14）。
//!
//! 决策是纯确定性逻辑（模式分派 + 阈值比较），按规则7 显式写成代码，
//! 不交给模型。

use super::affinity::{
    AffinityDecision, AffinityLookup, AffinityOutcome, AffinityStickTable, AffinityTable,
    CacheUsage, DEFAULT_AFFINITY_TTL, EVICT_SCAN_INTERVAL, MAX_TRACKED_STICKS,
};
use crate::config::{
    loader::default_vendor_cache_ttl_secs, AffinityMode, AffinitySection, ConfigManager,
    VendorConfig,
};
use crate::storage::{AffinityStick, AffinityStickSink};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// 亲和查询态三态常量与机器码（`affinity.rs` 的结构体在此取得行为）
impl AffinityLookup {
    /// 未命中（无绑定，或绑定值不可解析）—— 与「未接线」区分开
    pub const MISS: AffinityLookup = AffinityLookup {
        vendor_id: None,
        disabled: false,
    };
    /// 亲和未接线（`off` 模式或亲和表未装配）
    pub const DISABLED: AffinityLookup = AffinityLookup {
        vendor_id: None,
        disabled: true,
    };

    /// 命中构造
    pub fn hit(vendor_id: String) -> Self {
        Self {
            vendor_id: Some(vendor_id),
            disabled: false,
        }
    }

    /// 机器码（解释端点输出口径；合法值集单源于本文件）
    pub fn reason(&self) -> &'static str {
        if self.disabled {
            "disabled"
        } else if self.vendor_id.is_some() {
            "hit"
        } else {
            "miss"
        }
    }
}

/// 亲和判定结果的机器码与谓词
impl AffinityOutcome {
    /// 机器码（解释端点/日志字段口径；合法值集单源于本文件）
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stuck => "stuck",
            Self::BelowThreshold => "threshold-skip",
            Self::TurnStuck => "turn-stuck",
            Self::LegacyStuck => "legacy-stuck",
            Self::Skipped => "skipped",
        }
    }

    /// 本次结算是否真的写了亲和键
    pub fn stuck(self) -> bool {
        matches!(self, Self::Stuck | Self::TurnStuck | Self::LegacyStuck)
    }
}

/// 决策快照的构造
impl AffinityDecision {
    /// 亲和未接线或无会话键时的空决策（不动任何状态，也不算命中）
    pub fn inert(mode: AffinityMode) -> Self {
        Self {
            mode,
            outcome: AffinityOutcome::Skipped,
            worth_tokens: 0,
            ttl_secs: 0,
        }
    }
}

/// 缓存价值计算
impl CacheUsage {
    /// 缓存价值 = cache_read + cache_creation +（沉默缓存 vendor 的
    /// prompt_tokens 兜底）。饱和加法：三档各自已是上游原值，求和溢出按
    /// u64 上限封顶（不影响门限判定方向）
    pub fn worth(self, silent_cache: bool) -> u64 {
        let base = self
            .cache_read_tokens
            .saturating_add(self.cache_creation_tokens);
        if silent_cache {
            base.saturating_add(self.prompt_tokens)
        } else {
            base
        }
    }
}

/// 粘性记账表行为
impl AffinityStickTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记账（覆盖同键旧值）。返回实际保留的条数。
    pub fn record(&self, stick: AffinityStick) -> usize {
        let key = stick_key(&stick.tenant, &stick.session_key);
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self.sticks.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(key, (seq, stick));
        if seq.is_multiple_of(EVICT_SCAN_INTERVAL) {
            evict_oldest(&mut guard, MAX_TRACKED_STICKS);
        }
        guard.len()
    }

    /// 快照（落盘/测试读取；按写入序升序，恢复侧取最后写入的同键行）
    pub fn snapshot(&self) -> Vec<AffinityStick> {
        let guard = self.sticks.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(u64, AffinityStick)> = guard.values().cloned().collect();
        rows.sort_by_key(|(seq, _)| *seq);
        rows.into_iter().map(|(_, stick)| stick).collect()
    }

    pub fn len(&self) -> usize {
        self.sticks.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 灌入启动恢复的行（覆盖同键；返回灌入后条数）
    pub fn restore(&self, sticks: Vec<AffinityStick>) -> usize {
        let mut guard = self.sticks.lock().unwrap_or_else(|e| e.into_inner());
        for stick in sticks {
            let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            guard.insert(stick_key(&stick.tenant, &stick.session_key), (seq, stick));
        }
        evict_oldest(&mut guard, MAX_TRACKED_STICKS);
        guard.len()
    }
}

/// 粘性键（租户维度与 legacy 互不碰撞——空租户显式占位）
fn stick_key(tenant: &str, session_key: &str) -> String {
    format!("{tenant}\n{session_key}")
}

/// 淘汰最旧的记录到上限以内（摊还清理；只在记账路径按步长触发）
fn evict_oldest(guard: &mut HashMap<String, (u64, AffinityStick)>, limit: usize) {
    if guard.len() <= limit {
        return;
    }
    let mut seqs: Vec<u64> = guard.values().map(|(seq, _)| *seq).collect();
    seqs.sort_unstable();
    let drop_until = seqs[guard.len() - limit];
    guard.retain(|_, (seq, _)| *seq >= drop_until);
}

/// 四模式 settle 决策（纯函数，零 IO）：只算「该不该粘、粘多久」，写入动作
/// 由 [`AffinityWriter::settle`] 落到亲和表。
///
/// - `legacy`：粘 1h（现行为冻结，TTL = [`DEFAULT_AFFINITY_TTL`]）
/// - `auto`：`worth ≥ cache_worth_tokens` → 粘 `vendor.cache_ttl_secs`；
///   否则 `BelowThreshold`（**不写也不删**——既有记录按自身 TTL 自然过期，
///   中途删记录会立刻换 vendor，而 vendor 侧缓存可能仍有值）
/// - `turn`：粘 `turn_gap_secs`（不看 worth）
/// - `off`：不写
pub fn decide(
    section: &AffinitySection,
    vendor: Option<&VendorConfig>,
    usage: CacheUsage,
) -> AffinityDecision {
    let mode = AffinityMode::from_section(section);
    match mode {
        AffinityMode::Legacy => AffinityDecision {
            mode,
            outcome: AffinityOutcome::LegacyStuck,
            worth_tokens: 0,
            ttl_secs: DEFAULT_AFFINITY_TTL.as_secs(),
        },
        AffinityMode::Auto => {
            let silent = vendor.is_some_and(|v| v.silent_cache);
            let worth = usage.worth(silent);
            if worth >= section.cache_worth_tokens {
                let ttl_secs = vendor
                    .map(|v| v.cache_ttl_secs)
                    .unwrap_or_else(default_vendor_cache_ttl_secs);
                AffinityDecision {
                    mode,
                    outcome: AffinityOutcome::Stuck,
                    worth_tokens: worth,
                    ttl_secs,
                }
            } else {
                AffinityDecision {
                    mode,
                    outcome: AffinityOutcome::BelowThreshold,
                    worth_tokens: worth,
                    ttl_secs: 0,
                }
            }
        }
        AffinityMode::Turn => AffinityDecision {
            mode,
            outcome: AffinityOutcome::TurnStuck,
            worth_tokens: 0,
            ttl_secs: section.turn_gap_secs,
        },
        AffinityMode::Off => AffinityDecision {
            mode,
            outcome: AffinityOutcome::Skipped,
            worth_tokens: 0,
            ttl_secs: 0,
        },
    }
}

/// 缓存经济学亲和写入器（api 层 usage 结算点持有；Clone 廉价）。
#[derive(Clone)]
pub struct AffinityWriter {
    config: Arc<ConfigManager>,
    table: Arc<AffinityTable>,
    sticks: Arc<AffinityStickTable>,
    /// 粘性落盘通道（auto/turn 投递）；None = 未接线，只内存粘不落盘
    sink: Option<Arc<dyn AffinityStickSink>>,
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
}

impl AffinityWriter {
    pub fn new(
        config: Arc<ConfigManager>,
        table: Arc<AffinityTable>,
        sticks: Arc<AffinityStickTable>,
        sink: Option<Arc<dyn AffinityStickSink>>,
        metrics: Arc<crate::utils::metrics::GlobalMetrics>,
    ) -> Self {
        Self {
            config,
            table,
            sticks,
            sink,
            metrics,
        }
    }

    /// 响应 usage 结算：读配置算决策 → 落亲和表 →（auto/turn）记账 + 投递落盘。
    ///
    /// 亲和表未接线或无会话键时返回 `inert` 决策（零副作用，不算命中）。
    /// 亲和是优化非承诺：落盘投递失败不影响已生效的内存粘性。
    pub async fn settle(
        &self,
        tenant: Option<&str>,
        session_key: Option<&str>,
        vendor: &VendorConfig,
        usage: CacheUsage,
    ) -> AffinityDecision {
        let section = self.config.read().routing.affinity.clone();
        let mode = AffinityMode::from_section(&section);
        let Some(session) = session_key else {
            return AffinityDecision::inert(mode);
        };
        let decision = decide(&section, Some(vendor), usage);
        if !decision.outcome.stuck() {
            // 该写未写（auto 下 worth 低于门限）= 会话回到路由池的漂移
            if decision.outcome == AffinityOutcome::BelowThreshold {
                self.metrics.record_affinity_drift();
            }
            return decision;
        }
        self.metrics.record_affinity_stick();
        // TTL 下限 1s：0 会让记录写完即过期、等于永不粘（配置侧已拒绝 0，
        // 此处兜住手构配置与单位换算的边界）
        let ttl = Duration::from_secs(decision.ttl_secs.max(1));
        self.table.bind_for(tenant, session, &vendor.id, ttl).await;
        // 落盘只对 auto/turn 记账（legacy 维持现状：不落盘，避免改既有语义）
        if matches!(
            decision.outcome,
            AffinityOutcome::Stuck | AffinityOutcome::TurnStuck
        ) {
            let stick = AffinityStick {
                tenant: tenant.unwrap_or_default().to_string(),
                session_key: session.to_string(),
                vendor_id: vendor.id.clone(),
                worth_tokens: decision.worth_tokens.min(i64::MAX as u64) as i64,
                expires_at: chrono::Utc::now().timestamp() + decision.ttl_secs as i64,
            };
            self.sticks.record(stick.clone());
            if let Some(sink) = self.sink.as_ref() {
                sink.record(stick);
            }
        }
        decision
    }

    /// 只算不写：无会话键（或需在写入前预读决策）时按当前配置求值。
    /// 与 [`Self::settle`] 同源（同一 `decide`），口径不会漂移。
    pub fn decide_for(&self, vendor: &VendorConfig, usage: CacheUsage) -> AffinityDecision {
        decide(&self.config.read().routing.affinity, Some(vendor), usage)
    }

    /// 粘性记账表句柄（启动恢复与落盘读取的注入点）
    pub fn stick_table(&self) -> &Arc<AffinityStickTable> {
        &self.sticks
    }
}

#[cfg(test)]
mod tests;
