// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! fail-closed 掩码门：`[masking]` enabled（热读）+ feature `masking`
//! 双门控。检测/掩码/还原任一步 Err 或 panic → [`MaskError::Unavailable`]
//!（调用方映射 503 `masking-unavailable` 拒发），绝不原文出站；Block 动作
//! → [`MaskError::Blocked`]（400 `masking-blocked`）。
//!
//! 策略面热语义：检测器 + 动作映射（actions/kinds/allow）按整段配置指纹
//! 惰性重建（配置段相等性比较，快路径零克隆）；测试可 `with_detector`
//! 注入固定检测器（只替换检测器，动作面仍按配置缓存）。占位符注册表
//! 进程级单实例；回程还原只认本请求 [`MaskTicket`] 登记过的占位符
//!（TTL 24h 覆盖单请求生命周期）。
//!
//! 统计窗：最近 24h 命中元数据（种类 + 值长度）滚动保留，供
//! `GET /admin/masking/stats` 输出（不含值本身）。

use super::masker::{mask_json, ActionMap, AllowList, KindSet};
use super::restorer::restore_json;
use super::rule_detector::RuleDetector;
use super::stream_restore::StreamRestorer;
use super::{
    MaskAction, MaskDetector, MaskError, MaskKind, MaskTicket, MaskedHitInfo, PlaceholderRegistry,
};
use crate::config::{ConfigManager, MaskingSection};
use crate::error::GateError;
use crate::utils::metrics::GlobalMetrics;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// 统计窗保留时长（admin stats 滚动窗口）。
pub const STATS_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// 窗口硬上界（防极小窗口内爆量条目；超出淘汰最旧并计数）。
const WINDOW_HARD_CAP: usize = 100_000;

/// 缓存的策略面：整段配置作指纹（`MaskingSection: Eq`），重建时一次克隆。
struct PolicySurface {
    key: MaskingSection,
    detector: Arc<dyn MaskDetector>,
    actions: ActionMap,
    kinds: KindSet,
    allow: Arc<AllowList>,
}

/// 统计窗条目：命中时刻 + 种类 + 原值字节长度（不含值）。
#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub at: Instant,
    pub kind: MaskKind,
    pub value_len: usize,
}

/// fail-closed 掩码门。
pub struct MaskGate {
    config: Arc<ConfigManager>,
    metrics: Arc<GlobalMetrics>,
    registry: Arc<PlaceholderRegistry>,
    /// 惰性构建/指纹变更重建的策略面（检测器 + 动作映射）。
    policy: RwLock<Option<Arc<PolicySurface>>>,
    /// 测试注入的固定检测器（Some 时只替换检测器，动作面仍按配置缓存）。
    injected: Option<Arc<dyn MaskDetector>>,
    /// 24h 滚动统计窗（条目按 `at` 非降序追加；读取/写入时剪过期前缀）。
    window: Mutex<Vec<WindowEntry>>,
    /// 窗内淘汰累计（观测面）。
    window_evicted: AtomicU64,
}

impl std::fmt::Debug for MaskGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskGate")
            .field("enabled", &self.enabled())
            .field("injected", &self.injected.is_some())
            .finish_non_exhaustive()
    }
}

impl MaskGate {
    /// 由配置管理器构造门（策略面按 `[masking]` 惰性构建）。
    pub fn new(config: Arc<ConfigManager>, metrics: Arc<GlobalMetrics>) -> Self {
        Self {
            config,
            metrics,
            registry: Arc::new(PlaceholderRegistry::new()),
            policy: RwLock::new(None),
            injected: None,
            window: Mutex::new(Vec::new()),
            window_evicted: AtomicU64::new(0),
        }
    }

    /// 注入固定检测器（测试/嵌入方面）：只替换检测器，动作面仍按配置缓存。
    pub fn with_detector(mut self, detector: Arc<dyn MaskDetector>) -> Self {
        self.injected = Some(detector);
        self
    }

    /// 占位符注册表（流式/整量还原共用同一映射面）。
    pub fn registry(&self) -> Arc<PlaceholderRegistry> {
        Arc::clone(&self.registry)
    }

    /// `[masking]`.enabled 热读。
    pub fn enabled(&self) -> bool {
        self.config.read().masking.enabled
    }

    /// 出站 body 掩码：enabled=false 原样透传（零行为）并返回空凭据；
    /// 检测 panic 折叠 `Unavailable`；Block 命中透出 `Blocked`。命中入指标
    /// 与统计窗。返回的 [`MaskTicket`] 是回程还原的放行凭据（同请求穿参）。
    pub fn mask_body(&self, body: Value) -> Result<(Value, Arc<MaskTicket>), MaskError> {
        if !self.enabled() {
            return Ok((body, Arc::new(MaskTicket::new())));
        }
        let (detector, surface) = self.active_policy();
        let registry = Arc::clone(&self.registry);
        let ticket = Arc::new(MaskTicket::new());
        let closure_surface = Arc::clone(&surface);
        let closure_ticket = Arc::clone(&ticket);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut masked = body;
            let infos = mask_json(
                &mut masked,
                detector.as_ref(),
                &closure_surface.actions,
                &closure_surface.kinds,
                &closure_surface.allow,
                &registry,
                &closure_ticket,
            )?;
            Ok((masked, infos))
        }));
        match outcome {
            Ok(Ok((masked, infos))) => {
                self.record_hits(&infos, &surface.actions);
                Ok((masked, ticket))
            }
            Ok(Err(err)) => {
                if let MaskError::Blocked { kind } = &err {
                    self.metrics.record_masking_hit(kind.as_str(), "block");
                    self.metrics.record_masking_blocked();
                } else {
                    self.metrics.record_masking_fail();
                }
                Err(err)
            }
            Err(_panic) => {
                self.metrics.record_masking_fail();
                Err(MaskError::Unavailable)
            }
        }
    }

    /// 非流式整量还原（响应回程）：enabled=false 直通；`ticket` 为空
    ///（本请求零登记）整体跳过遍历；panic 折叠 `Unavailable`（fail-closed：
    /// 还原不可用宁 503 不吐占位符）。回填数入 `masking_restore_total`。
    pub fn restore_body(&self, body: &mut Value, ticket: &MaskTicket) -> Result<(), MaskError> {
        if !self.enabled() || ticket.is_empty() {
            return Ok(());
        }
        let registry = Arc::clone(&self.registry);
        // panic 时 body 可能已部分还原——fail-closed 折叠 Unavailable，
        // 调用方拒发（占位符映射可信度已不可证）
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            restore_json(body, &registry, Some(ticket))
        }));
        match outcome {
            Ok(count) => {
                self.metrics.record_masking_restore(count as u64);
                Ok(())
            }
            Err(_panic) => {
                self.metrics.record_masking_fail();
                Err(MaskError::Unavailable)
            }
        }
    }

    /// 流式还原器工厂（不限域；生产路径走 [`Self::stream_restorer_scoped`]）。
    pub fn stream_restorer(&self) -> StreamRestorer {
        StreamRestorer::new(self.registry())
    }

    /// 流式还原器工厂（请求域绑定：只还原本请求登记过的占位符）。
    pub fn stream_restorer_scoped(&self, ticket: Arc<MaskTicket>) -> StreamRestorer {
        StreamRestorer::scoped(self.registry(), ticket)
    }

    /// 流式增量还原 + 打点：`feed` 经 catch_unwind 折叠 `Unavailable`；
    /// 回填数按还原器计数差值精确累加。
    pub fn restore_delta(
        &self,
        restorer: &mut StreamRestorer,
        delta: &str,
    ) -> Result<String, MaskError> {
        let before = restorer.restored_total();
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| restorer.feed(delta)));
        match outcome {
            Ok(out) => {
                let after = restorer.restored_total();
                self.metrics
                    .record_masking_restore(after.saturating_sub(before));
                Ok(out)
            }
            Err(_panic) => {
                self.metrics.record_masking_fail();
                Err(MaskError::Unavailable)
            }
        }
    }

    /// 流收流（flush 缓冲剩余）；打点同 [`Self::restore_delta`]。
    pub fn flush_stream(&self, restorer: &mut StreamRestorer) -> Result<String, MaskError> {
        let before = restorer.restored_total();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| restorer.flush()));
        match outcome {
            Ok(out) => {
                let after = restorer.restored_total();
                self.metrics
                    .record_masking_restore(after.saturating_sub(before));
                Ok(out)
            }
            Err(_panic) => {
                self.metrics.record_masking_fail();
                Err(MaskError::Unavailable)
            }
        }
    }

    /// 24h 统计窗快照：逐 kind 命中次数与值长度直方图（桶边界
    /// 8/16/32/64/128/256/1KiB/∞）。过期条目惰性清理。不含任何值本身。
    pub fn stats_window(&self) -> Vec<(MaskKind, u64, [u64; 8])> {
        let mut window = self.lock_window();
        let now = Instant::now();
        prune_expired(&mut window, now);
        let mut per_kind: Vec<(MaskKind, u64, [u64; 8])> = MaskKind::all()
            .into_iter()
            .map(|k| (k, 0u64, [0u64; 8]))
            .collect();
        for entry in window.iter() {
            if let Some(slot) = per_kind.iter_mut().find(|(k, _, _)| *k == entry.kind) {
                slot.1 += 1;
                slot.2[len_bucket(entry.value_len)] += 1;
            }
        }
        per_kind.retain(|(_, count, _)| *count > 0);
        per_kind
    }

    /// 窗内淘汰累计（观测面）。
    pub fn window_evicted(&self) -> u64 {
        self.window_evicted.load(Ordering::Relaxed)
    }

    /// 当前生效策略面：检测器注入优先；配置段相等比较命中缓存（快路径
    /// 零克隆），段变更惰性重建（慢路径单次克隆整段并重建动作面）。
    fn active_policy(&self) -> (Arc<dyn MaskDetector>, Arc<PolicySurface>) {
        let surface = self.policy_surface();
        let detector = match &self.injected {
            Some(injected) => Arc::clone(injected),
            None => Arc::clone(&surface.detector),
        };
        (detector, surface)
    }

    /// 策略面缓存读取/重建（检测器 + 动作映射同指纹缓存——每请求一次
    /// 段相等比较与缓存读，零克隆零重建）。
    fn policy_surface(&self) -> Arc<PolicySurface> {
        {
            let config = self.config.read();
            let cached = self.policy.read().unwrap_or_else(|p| p.into_inner());
            if let Some(surface) = cached.as_ref() {
                if surface.key == config.masking {
                    return Arc::clone(surface);
                }
            }
        }
        let section = self.config.read().masking.clone();
        let surface = Arc::new(Self::build_surface(section));
        *self.policy.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&surface));
        surface
    }

    /// 由配置段构建策略面（重建路径：整段已克隆）。
    fn build_surface(section: MaskingSection) -> PolicySurface {
        let detector: Arc<dyn MaskDetector> = Arc::new(
            RuleDetector::build(&section.words, section.words_any_case, &section.rules)
                .unwrap_or_else(|_| {
                    // 构造失败只可能来自自定义正则（feature 关闭时
                    // validate_masking 跳过编译校验的兜底面）：内置规则与
                    // 词表不受影响——空 rules 重建保持检测能力并告警
                    tracing::warn!("{}", crate::i18n::t("masking-rule-build-failed"));
                    RuleDetector::build(&section.words, section.words_any_case, &[])
                        .expect("fallback detector without custom rules cannot fail")
                }),
        );
        PolicySurface {
            key: section.clone(),
            detector,
            actions: Self::action_map(&section),
            kinds: Self::kind_set(&section),
            allow: Arc::new(AllowList::from_values(section.allow.iter().cloned())),
        }
    }

    /// `actions` 配置 → 动作映射（键经闭集校验后必可解析；未知键容错跳过）。
    fn action_map(section: &MaskingSection) -> ActionMap {
        let mut actions = ActionMap::default();
        for (key, value) in &section.actions {
            if let Some(kind) = MaskKind::parse(key) {
                let action = match value.as_str() {
                    "warn" => MaskAction::Warn,
                    "block" => MaskAction::Block,
                    _ => MaskAction::Mask,
                };
                actions.set(kind, action);
            }
        }
        actions
    }

    /// `kinds` 配置 → 种类集（空 = 全启用）。
    fn kind_set(section: &MaskingSection) -> KindSet {
        if section.kinds.is_empty() {
            KindSet::all()
        } else {
            KindSet::from_kinds(section.kinds.iter().filter_map(|k| MaskKind::parse(k)))
        }
    }

    /// 命中入指标（Mask/Warn 各记一行）与 24h 统计窗。
    fn record_hits(&self, infos: &[MaskedHitInfo], actions: &ActionMap) {
        let mut window = self.lock_window();
        let now = Instant::now();
        for info in infos {
            let action_str = match actions.resolve(info.kind) {
                MaskAction::Mask => "mask",
                MaskAction::Warn => "warn",
                MaskAction::Block => "block",
            };
            self.metrics
                .record_masking_hit(info.kind.as_str(), action_str);
            window.push(WindowEntry {
                at: now,
                kind: info.kind,
                value_len: info.len,
            });
        }
        prune_expired(&mut window, now);
        if window.len() > WINDOW_HARD_CAP {
            let drop = window.len() - WINDOW_HARD_CAP;
            window.drain(0..drop);
            self.window_evicted
                .fetch_add(drop as u64, Ordering::Relaxed);
        }
    }

    fn lock_window(&self) -> std::sync::MutexGuard<'_, Vec<WindowEntry>> {
        self.window.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// 统计窗过期前缀剪枝：条目按 `at` 非降序追加（每批一个采集时刻），
/// `partition_point` 定位过期前缀 O(log n) + `drain`——锁内工作量与窗龄
/// 挂钩而非窗长（稳态满窗 10 万条时避免逐条 retain 的 O(n) 串行点）。
fn prune_expired(window: &mut Vec<WindowEntry>, now: Instant) {
    let expired = window.partition_point(|e| now.duration_since(e.at) >= STATS_WINDOW);
    if expired > 0 {
        window.drain(0..expired);
    }
}

/// 值长度直方图桶（边界 8/16/32/64/128/256/1KiB，其余封顶桶）。
fn len_bucket(len: usize) -> usize {
    match len {
        0..=8 => 0,
        9..=16 => 1,
        17..=32 => 2,
        33..=64 => 3,
        65..=128 => 4,
        129..=256 => 5,
        257..=1024 => 6,
        _ => 7,
    }
}

/// [`MaskError`] → 应用面错误映射（vendor 出站路径消费）：Block → 400
/// `masking-blocked`，其余 → 503 `masking-unavailable`。
impl From<MaskError> for GateError {
    fn from(err: MaskError) -> Self {
        match err {
            MaskError::Blocked { kind } => GateError::MaskingBlocked {
                kind: kind.as_str().to_string(),
            },
            MaskError::Unavailable | MaskError::InvalidRule { .. } => GateError::MaskingUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_bucket_boundaries() {
        assert_eq!(len_bucket(0), 0);
        assert_eq!(len_bucket(8), 0);
        assert_eq!(len_bucket(9), 1);
        assert_eq!(len_bucket(16), 1);
        assert_eq!(len_bucket(17), 2);
        assert_eq!(len_bucket(1024), 6);
        assert_eq!(len_bucket(1025), 7);
        assert_eq!(len_bucket(100_000), 7);
    }

    /// 统计窗剪枝：过期前缀（backdate 条目）在写入/读取时被 O(log n)
    /// 定位并剪除——窗内只留活条目（H1 修复钉：不再全量 retain）。
    #[test]
    fn stats_window_prunes_expired_prefix() {
        let toml_v = "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n[database]\npath = \":memory:\"\n";
        let config: crate::config::Config = crate::config::parse_config_content(
            std::path::Path::new("masking-window.toml"),
            toml_v,
        )
        .expect("样例配置解析");
        let config = Arc::new(crate::config::ConfigManager::new(config));
        let gate = MaskGate::new(config, Arc::new(GlobalMetrics::new()));
        let now = Instant::now();
        {
            let mut window = gate.lock_window();
            // 旧条目（全超 24h）与新条目——按 at 非降序追加
            for hours_ago in [30, 26, 25] {
                window.push(WindowEntry {
                    at: now - Duration::from_secs(hours_ago * 3600),
                    kind: MaskKind::ApiKey,
                    value_len: 16,
                });
            }
            window.push(WindowEntry {
                at: now,
                kind: MaskKind::Token,
                value_len: 32,
            });
        }
        let live = gate.stats_window();
        assert_eq!(live.len(), 1, "过期前缀必须剪除: {live:?}");
        assert_eq!(live[0].0, MaskKind::Token);
        assert_eq!(live[0].1, 1);
        let mut window = gate.lock_window();
        prune_expired(&mut window, Instant::now());
        assert_eq!(window.len(), 1, "剪除后窗内只留活条目");
    }

    #[test]
    fn mask_error_maps_to_gate_error() {
        let blocked: GateError = MaskError::Blocked {
            kind: MaskKind::Custom,
        }
        .into();
        assert_eq!(blocked.status_code(), 400);
        assert_eq!(blocked.error_code(), "masking_blocked");
        let unavailable: GateError = MaskError::Unavailable.into();
        assert_eq!(unavailable.status_code(), 503);
        assert_eq!(unavailable.error_code(), "masking_unavailable");
    }
}
