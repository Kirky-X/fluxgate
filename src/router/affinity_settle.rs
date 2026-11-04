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
    AffinityDecision, AffinityOutcome, AffinityStickTable, AffinityTable, CacheUsage,
    DEFAULT_AFFINITY_TTL, EVICT_SCAN_INTERVAL, MAX_TRACKED_STICKS,
};
use crate::config::{
    loader::default_vendor_cache_ttl_secs, AffinityMode, AffinitySection, ConfigManager,
    VendorConfig,
};
use crate::storage::{AffinityStick, AffinityStickSink};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    decide_for_mode(mode, section, vendor, usage)
}

/// [`decide`] 的模式已解析形态：热路径（settle）每次响应已解析过一次 mode，
/// 不在此重复 trim/比较；与 [`decide`] 同源（同一实现），口径不会漂移。
fn decide_for_mode(
    mode: AffinityMode,
    section: &AffinitySection,
    vendor: Option<&VendorConfig>,
    usage: CacheUsage,
) -> AffinityDecision {
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

/// 缓存经济学亲和写入器（api 层 usage 结算点持有；Clone 廉价——内部全是
/// Arc / 原子量，克隆共享同一份粘性与采样状态）。
#[derive(Clone)]
pub struct AffinityWriter {
    config: Arc<ConfigManager>,
    table: Arc<AffinityTable>,
    sticks: Arc<AffinityStickTable>,
    /// 粘性落盘通道（auto/turn 投递）；None = 未接线，只内存粘不落盘
    sink: Option<Arc<dyn AffinityStickSink>>,
    metrics: Arc<crate::utils::metrics::GlobalMetrics>,
    /// 已采样进 metrics 的通道丢弃累计（差值对账——丢弃只发生在有流量的
    /// 时候，settle 采样点足够稠密；两次采样间的丢弃在下一次 settle 补记）
    sampled_sink_drops: Arc<AtomicU64>,
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
            sampled_sink_drops: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 响应 usage 结算：读配置算决策 → 落亲和表 →（auto/turn）记账 + 投递落盘。
    ///
    /// 亲和表未接线或无会话键时返回 `inert` 决策（零副作用，不算命中）。
    /// 亲和是优化非承诺：落盘投递失败不影响已生效的内存粘性。
    ///
    /// 写入语义按模式分流：
    /// - `legacy`：与派发成功点的 `crate::api::bind_session_affinity` 同语义
    ///   （租户隔离开启时首绑定收敛 `bind_if_absent`，否则无条件 `bind`），
    ///   不绕过并发首绑定的收敛结果；
    /// - `auto`/`turn`：`bind_for` 无条件覆盖（每次结算是一份新的粘持判定）。
    pub async fn settle(
        &self,
        tenant: Option<&str>,
        session_key: Option<&str>,
        vendor: &VendorConfig,
        usage: CacheUsage,
    ) -> AffinityDecision {
        // 借用配置快照（Arc<Config> 不深拷贝）：section 只在本次 settle 内
        // 使用，克隆整段会每请求多一次 String 分配（热路径）
        let config = self.config.read();
        let section = &config.routing.affinity;
        // 租户隔离开关决定 legacy 的写入语义（首绑定收敛仅在该形态成立）
        let tenant_scoped = config.routing.affinity_tenant_scoped;
        let mode = AffinityMode::from_section(section);
        let Some(session) = session_key else {
            return AffinityDecision::inert(mode);
        };
        self.sync_sink_drops();
        let decision = decide_for_mode(mode, section, Some(vendor), usage);
        if !decision.outcome.stuck() {
            // 该写未写（auto 下 worth 低于门限）= 会话回到路由池的漂移
            if decision.outcome == AffinityOutcome::BelowThreshold {
                self.metrics.record_affinity_drift();
            }
            return decision;
        }
        self.metrics.record_affinity_stick();
        if decision.outcome == AffinityOutcome::LegacyStuck {
            // legacy 现行为：与 `bind_session_affinity` 同源，避免第二条
            // 无条件 set 推翻并发首绑定的收敛结果
            if tenant_scoped {
                self.table.bind_if_absent(tenant, session, &vendor.id).await;
            } else {
                self.table.bind(tenant, session, &vendor.id).await;
            }
            return decision;
        }
        // TTL 下限 1s：0 会让记录写完即过期、等于永不粘（配置侧已拒绝 0，
        // 此处兜住手构配置与单位换算的边界）
        let ttl = Duration::from_secs(decision.ttl_secs.max(1));
        self.table.bind_for(tenant, session, &vendor.id, ttl).await;
        // 落盘只对 auto/turn 记账（legacy 维持现状：不落盘，避免改既有语义）
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
        decision
    }

    /// 粘性记账表句柄（启动恢复与落盘读取的注入点）
    pub fn stick_table(&self) -> &Arc<AffinityStickTable> {
        &self.sticks
    }

    /// 通道满丢弃的实时累计（未接线 = 0）。/admin/metrics 的精确读数出口
    ///（metrics 快照里的 `affinity_persist_drop_total` 是 settle 采样近似）。
    pub fn dropped_sticks(&self) -> u64 {
        self.sink.as_ref().map(|s| s.dropped_sticks()).unwrap_or(0)
    }

    /// 落库失败的批次数（未接线 / 实现不暴露时为 0）。与通道丢弃分列——
    /// 一个是被背压丢掉，一个是 UPSERT 失败整批丢弃。
    pub fn flush_failures(&self) -> u64 {
        self.sink.as_ref().map(|s| s.flush_failures()).unwrap_or(0)
    }

    /// 会话最后一次粘性记账的缓存价值（`worth_tokens`）；无记账（legacy 模式
    /// / 尚未 settle）返回 None——解释端点的 `worth_tokens` 来源。
    ///
    /// 只在记账行的 vendor 与当前绑定一致时可用（调用方传入绑定 vendor）：
    /// 记账表按（租户, 会话）唯一，绑定可能在两次记账之间换过 vendor。
    /// 记账行的 `expires_at` 是 settle 当时的快照，不随读路径变化——解释端的
    /// TTL 一律取亲和表侧的真实剩余（[`AffinityTable::remaining_ttl`]）。
    pub fn last_stick_for(
        &self,
        tenant: Option<&str>,
        session_key: &str,
        bound_vendor: &str,
    ) -> Option<u64> {
        let key = format!("{}\n{}", tenant.unwrap_or_default(), session_key);
        self.sticks
            .sticks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .filter(|(_, stick)| stick.vendor_id == bound_vendor)
            .map(|(_, stick)| stick.worth_tokens.max(0) as u64)
    }

    /// 把通道丢弃计数差值采样进 metrics（settle 打点；`record` 路径本身
    /// 无 metrics 句柄，这里按单调计数对账）。
    ///
    /// 并发 settle 共享本句柄：`fetch_max` 是 CAS 推进——并发者只有把
    /// `sampled` 推到更高值的那一个拿到旧值记账，其余拿到相同旧值与新值
    /// （差 0），不会重复记账。
    fn sync_sink_drops(&self) {
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let total = sink.dropped_sticks();
        let previous = self.sampled_sink_drops.fetch_max(total, Ordering::Relaxed);
        if total > previous {
            self.metrics.record_affinity_persist_drop(total - previous);
        }
    }
}

#[cfg(test)]
mod tests;
