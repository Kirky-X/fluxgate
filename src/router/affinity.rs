// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 会话/前缀亲和表 — oxcache 承载存储。
//!
//! 键 `affinity:{session_key}`；`affinity_tenant_scoped` 开启时带租户维度
//! `affinity:{api_key_hash}:{session_key}`。值 = vendor_id，命中即续期 TTL
//! （活跃会话粘住、闲置到点过期）；存取出错静默降级为无亲和。

use crate::config::AffinityMode;
// 粘性行契约单源在 `storage`（落盘与恢复共用）；此处 re-export 供 router 半边复用
pub use crate::storage::AffinityStick;
use oxcache::backend::CacheBackend;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 亲和键前缀 — 与响应缓存的 `resp:` 命名空间隔离
pub const AFFINITY_KEY_PREFIX: &str = "affinity:";

/// 默认亲和 TTL 1h — 每次命中（活跃请求）续期，闲置会话 1h 后过期
pub const DEFAULT_AFFINITY_TTL: Duration = Duration::from_secs(3600);

/// 粘性记账表的软上限（超出按 seq 淘汰最旧的）：粘性只服务 auto/turn 落盘
/// 与可观测，是优化非承诺，必须有界——活跃会话数超过此值时最旧的记录被
/// 丢弃（不影响亲和表本体，亲和仍由 oxcache 承载）。
///
/// 淘汰是摊还的（每 [`EVICT_SCAN_INTERVAL`] 次记账扫一次），故实际条目数
/// 上界 = 本值 + `EVICT_SCAN_INTERVAL`（两次扫描之间可短暂越界，不超界）。
pub(crate) const MAX_TRACKED_STICKS: usize = 4096;
/// 淘汰扫描步长（累计这么多次记账做一次最旧项清理，摊还 O(1)）
pub(crate) const EVICT_SCAN_INTERVAL: u64 = 256;

/// 亲和查询态（三态：`Hit { vendor_id }` / `MISS` / `DISABLED`）。
/// 常量与 `hit()` / `reason()` 落在 `affinity_settle.rs`（规则14）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffinityLookup {
    /// 命中时的绑定 vendor；未命中/未接线恒 None
    pub vendor_id: Option<String>,
    /// 亲和未接线（`off` 模式或表未装配）
    pub disabled: bool,
}

/// 会话 → 上次成功服务的 vendor 的亲和表
#[derive(Clone)]
pub struct AffinityTable {
    backend: Arc<dyn CacheBackend + Send + Sync>,
    ttl: Duration,
}

impl AffinityTable {
    pub fn new(backend: Arc<dyn CacheBackend + Send + Sync>, ttl: Duration) -> Self {
        Self { backend, ttl }
    }

    /// 以 oxcache 默认内存后端（moka）构造 — 生产/测试的便捷入口
    pub fn with_memory_backend(ttl: Duration) -> Self {
        Self::new(Arc::new(oxcache::backend::default_memory_backend()), ttl)
    }

    /// 亲和键：legacy `affinity:{session_key}`；租户隔离开启时
    /// `affinity:{api_key_hash}:{session_key}`。None/空租户一律 legacy 键
    fn key(tenant: Option<&str>, session_key: &str) -> Arc<str> {
        match tenant {
            Some(t) if !t.is_empty() => {
                Arc::from(format!("{AFFINITY_KEY_PREFIX}{t}:{session_key}").as_str())
            }
            _ => Arc::from(format!("{AFFINITY_KEY_PREFIX}{session_key}").as_str()),
        }
    }

    /// 查询会话绑定的 vendor_id；命中即续期 TTL（活跃续期语义）。
    /// 未绑定或存取出错返回 None。
    pub async fn lookup(&self, tenant: Option<&str>, session_key: &str) -> Option<String> {
        self.lookup_with_reason(tenant, session_key).await.vendor_id
    }

    /// 查询 + 命中原因（路由解释端点消费）。命中仍按现行为续期 TTL；
    /// 亲和表未接线 / 无会话键时返回 `Disabled`（与 `None` 区分开——
    /// 「没接线」和「没绑定」是两回事，解释面必须能分辨）。
    pub async fn lookup_with_reason(
        &self,
        tenant: Option<&str>,
        session_key: &str,
    ) -> AffinityLookup {
        let key = Self::key(tenant, session_key);
        match self.backend.get(&key).await {
            Ok(Some(value)) => {
                // 活跃续期：命中即重置 TTL；键恰在此刻被逐出时 expire 返回
                // false，无碍本次命中（下次请求自然 miss 重新绑定）
                let _ = self.backend.expire(&key, self.ttl).await;
                match String::from_utf8(value) {
                    Ok(vendor_id) => AffinityLookup::hit(vendor_id),
                    // 值非 UTF-8（非本进程写入或存储损坏）：按未绑定处理
                    Err(_) => AffinityLookup::MISS,
                }
            }
            // 存储出错同 lookup 的静默无亲和降级：报 Miss（不谎报命中）
            Ok(None) | Err(_) => AffinityLookup::MISS,
        }
    }

    /// 绑定会话 → vendor（执行成功后由 API 层回写；重复绑定覆盖旧值 —
    /// 失败迁移重绑定语义走本方法）
    pub async fn bind(&self, tenant: Option<&str>, session_key: &str, vendor_id: &str) {
        let _ = self
            .backend
            .set(
                Self::key(tenant, session_key),
                Arc::new(vendor_id.as_bytes().to_vec()),
                Some(self.ttl),
            )
            .await;
    }

    /// 首绑定收敛（真 SETNX）：已绑定不覆盖返回 false，未绑定写入返回 true。
    ///
    /// 后端提供原子面（`as_atomic_writer` 返回 `Some`，moka 恒有）时走库侧
    /// 原子 `set_if_absent` — 同一会话的并发首请求只有一个写入者胜出，不会各自
    /// 绑到不同 vendor；无原子面的后端退回 get+set 兜底（并存竞态窗口，后写胜出）。
    pub async fn bind_if_absent(
        &self,
        tenant: Option<&str>,
        session_key: &str,
        vendor_id: &str,
    ) -> bool {
        let key = Self::key(tenant, session_key);
        if let Some(atomic) = self.backend.as_atomic_writer() {
            return match atomic
                .set_if_absent(&key, vendor_id.as_bytes().to_vec(), Some(self.ttl))
                .await
            {
                Ok(true) => true,
                // 已绑定：不覆盖；与其他 path 同口径按命中续期刷新 TTL
                Ok(false) => {
                    let _ = self.backend.expire(&key, self.ttl).await;
                    false
                }
                // 存储出错：视为未写入（同 lookup 的静默无亲和降级，不谎报胜出）
                Err(_) => false,
            };
        }
        if self.backend.get(&key).await.ok().flatten().is_some() {
            let _ = self.backend.expire(&key, self.ttl).await;
            return false;
        }
        let _ = self
            .backend
            .set(key, Arc::new(vendor_id.as_bytes().to_vec()), Some(self.ttl))
            .await;
        true
    }

    /// 解除会话绑定 — 失败迁移耗尽或绑定 vendor 已不在合格集合时调用，
    /// 避免旧键指向失效 vendor
    pub async fn unbind(&self, tenant: Option<&str>, session_key: &str) {
        let _ = self.backend.delete(&Self::key(tenant, session_key)).await;
    }

    /// 只读窥视（不续期）：路由解释端点专用——`lookup` 的命中续期是副作用，
    /// 解释面必须纯只读（不改亲和状态）。未绑定 / 存取出错返回 None。
    pub async fn peek(&self, tenant: Option<&str>, session_key: &str) -> Option<String> {
        let value = self
            .backend
            .get(&Self::key(tenant, session_key))
            .await
            .ok()??;
        String::from_utf8(value).ok()
    }

    /// 键的剩余存活时长（秒级精度不足用 Duration）；未绑定 / 后端不支持
    /// TTL 查询返回 None。解释端点与 settle TTL 断言消费。
    pub async fn remaining_ttl(&self, tenant: Option<&str>, session_key: &str) -> Option<Duration> {
        let key = Self::key(tenant, session_key);
        self.backend.ttl(&key).await.ok()?
    }

    /// 带 TTL 的绑定（缓存经济学模式专用）：TTL 随上游缓存存活时长
    /// （`cache_ttl_secs`）或会话连续窗口（`turn_gap_secs`）变化，不能用
    /// 表级固定 TTL 的 [`Self::bind`]。语义按现行为：无条件 set（重复绑定
    /// 覆盖——与失败迁移重绑定 `bind` 同款，首绑定收敛只对 legacy 生效）。
    pub async fn bind_for(
        &self,
        tenant: Option<&str>,
        session_key: &str,
        vendor_id: &str,
        ttl: Duration,
    ) {
        let _ = self
            .backend
            .set(
                Self::key(tenant, session_key),
                Arc::new(vendor_id.as_bytes().to_vec()),
                Some(ttl),
            )
            .await;
    }
}

/// 亲和判定结果（`settle` 的机器码输出；stable，不做 i18n）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityOutcome {
    /// 已写入亲和，TTL = vendor `cache_ttl_secs`（auto 且 worth ≥ 门限）
    Stuck,
    /// 未写入：`auto` 下 worth < `cache_worth_tokens`（threshold-skip）。
    /// 既不粘也不删既有记录——既有记录按其 TTL 自然过期
    BelowThreshold,
    /// 已写入亲和，TTL = `turn_gap_secs`（turn 模式，不看 worth）
    TurnStuck,
    /// 已写入亲和，TTL = 1h（legacy 现行为冻结）
    LegacyStuck,
    /// 未写入：`off` 模式
    Skipped,
}

/// 一次 `settle` 的完整决策快照（解释端点与测试的可观测面）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityDecision {
    /// 生效模式（未知配置值按 `Legacy`；配置期已拒绝非法值）
    pub mode: AffinityMode,
    pub outcome: AffinityOutcome,
    /// 本次判定的缓存价值（auto 语义：cache_read + cache_creation +
    /// silent_cache 时的 prompt_tokens）；其余模式为 0
    pub worth_tokens: u64,
    /// 写入的 TTL（秒）；未写入（`BelowThreshold` / `Skipped`）为 0
    pub ttl_secs: u64,
}

/// 响应 usage 的缓存价值输入（settle 的纯数据入口；由 api 层从协议统一的
/// usage 形态构造，避免 router 半边依赖 serde_json）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheUsage {
    pub prompt_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// 粘性记账表：记录 auto/turn 模式写了哪些亲和（`tenant + session` 唯一），
/// 供落盘与启动恢复消费。只服务落盘与可观测，亲和判定本身由
/// [`AffinityTable`] 承载，本表缺失不阻塞路由。
#[derive(Default)]
pub struct AffinityStickTable {
    /// key = `tenant + '\n' + session_key`；value = 粘性行 + 写入序号
    pub(crate) sticks: Mutex<HashMap<String, (u64, AffinityStick)>>,
    pub(crate) seq: AtomicU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_lookup_unbind_roundtrip() {
        let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
        assert_eq!(table.lookup(None, "s1").await, None, "未绑定返回 None");

        table.bind(None, "s1", "vendor-a").await;
        assert_eq!(table.lookup(None, "s1").await.as_deref(), Some("vendor-a"));

        table.bind(None, "s1", "vendor-b").await;
        assert_eq!(
            table.lookup(None, "s1").await.as_deref(),
            Some("vendor-b"),
            "重复绑定覆盖"
        );

        table.unbind(None, "s1").await;
        assert_eq!(table.lookup(None, "s1").await, None, "unbind 后 miss");
    }

    /// TTL 过期：短 TTL（50ms）绑定后 sleep 60ms → lookup None（闲置过期）。
    #[tokio::test]
    async fn short_ttl_expires_idle_session() {
        let table = AffinityTable::with_memory_backend(Duration::from_millis(50));
        table.bind(None, "drift", "vendor-a").await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            table.lookup(None, "drift").await,
            None,
            "闲置超过 TTL 应过期"
        );
    }

    /// TTL 活跃续期：每次 lookup 都重置过期点——连续 lookup 跨越原 TTL
    /// 后值仍存活（活跃会话粘住）。参数已放大防调度抖动误过期。
    #[tokio::test]
    async fn lookup_renews_ttl_for_active_sessions() {
        let table = AffinityTable::with_memory_backend(Duration::from_millis(500));
        table.bind(None, "hot", "vendor-a").await;
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(
                table.lookup(None, "hot").await.as_deref(),
                Some("vendor-a"),
                "活跃会话每次命中续期 TTL，不得过期"
            );
        }
    }

    /// 键前缀隔离：亲和键带 `affinity:` 前缀，与裸键空间不冲突
    #[tokio::test]
    async fn keys_are_namespaced() {
        assert!(AFFINITY_KEY_PREFIX.starts_with("affinity:"));
        let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
        table.bind(None, "x", "vendor-a").await;
        // 底层键为 affinity:x —— 直接以裸键 "x" 查 oxcache 库不得命中亲和值
        let raw = table.backend.get("x").await.unwrap();
        assert_eq!(raw, None, "亲和键必须带 affinity: 前缀");
    }

    // tests：键租户隔离

    /// 两租户同 session id 不得互相劫持；租户键与 legacy 键空间互不可见
    #[tokio::test]
    async fn affinity_key_scoped_by_tenant() {
        let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
        table
            .bind(Some("tenant-a-hash"), "sess-1", "vendor-a")
            .await;

        // 同 session key 的 tenant-B：不得命中 A 的绑定（劫持修复本体）
        assert_eq!(
            table.lookup(Some("tenant-b-hash"), "sess-1").await,
            None,
            "tenant-B 不得命中 tenant-A 的绑定"
        );
        // A 自己正常命中
        assert_eq!(
            table
                .lookup(Some("tenant-a-hash"), "sess-1")
                .await
                .as_deref(),
            Some("vendor-a")
        );
        // legacy 键空间（tenant=None）与租户键空间互不可见
        assert_eq!(table.lookup(None, "sess-1").await, None);

        // unbind 只清除本租户的键
        table.unbind(Some("tenant-a-hash"), "sess-1").await;
        assert_eq!(table.lookup(Some("tenant-a-hash"), "sess-1").await, None);
    }

    // tests：bind_if_absent 首绑定收敛

    /// 未绑定写入返回 true；已绑定不覆盖返回 false（后端有原子面走 SETNX）。
    #[tokio::test]
    async fn bind_if_absent_converges_first_binding() {
        let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
        assert!(
            table.bind_if_absent(None, "s", "vendor-a").await,
            "未绑定应写入"
        );
        assert_eq!(table.lookup(None, "s").await.as_deref(), Some("vendor-a"));
        assert!(
            !table.bind_if_absent(None, "s", "vendor-b").await,
            "已绑定不得覆盖"
        );
        assert_eq!(
            table.lookup(None, "s").await.as_deref(),
            Some("vendor-a"),
            "首绑定收敛：后到请求不得改写"
        );
        // 租户维度同样收敛
        assert!(table.bind_if_absent(Some("t1"), "s", "vendor-a").await);
        assert!(!table.bind_if_absent(Some("t1"), "s", "vendor-b").await);
    }

    /// 并发首绑定：同一会话多请求同时首绑定 → 恰好一个写入者胜出，
    /// 且胜出者的 vendor 就是最终绑定值（原子面 SETNX 语义，无后写覆盖）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bind_if_absent_first_writer_wins_under_concurrency() {
        let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
        let mut handles = Vec::new();
        for i in 0..16 {
            let table = table.clone();
            handles.push(tokio::spawn(async move {
                let vendor = format!("vendor-{i}");
                let won = table.bind_if_absent(None, "race", &vendor).await;
                (won, vendor)
            }));
        }
        let mut winners = Vec::new();
        for handle in handles {
            let (won, vendor) = handle.await.expect("并发任务不得 panic");
            if won {
                winners.push(vendor);
            }
        }
        assert_eq!(winners.len(), 1, "并发首绑定只允许一个写入者胜出");
        assert_eq!(
            table.lookup(None, "race").await.as_deref(),
            Some(winners[0].as_str()),
            "最终绑定值必须是胜出者写入的值"
        );
    }

    /// 绑定过期后重新绑定：过期条目对本方法等同未绑定，不得因残留条目
    /// 而永久拒绝重新绑定（老会话重开仍能收敛到一次首绑定）。
    #[tokio::test]
    async fn bind_if_absent_rebinds_after_ttl_expiry() {
        let table = AffinityTable::with_memory_backend(Duration::from_millis(50));
        assert!(table.bind_if_absent(None, "exp", "vendor-a").await);
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(table.lookup(None, "exp").await, None, "TTL 到点已失效");
        assert!(
            table.bind_if_absent(None, "exp", "vendor-b").await,
            "过期后应可重新绑定"
        );
        assert_eq!(table.lookup(None, "exp").await.as_deref(), Some("vendor-b"));
    }

    /// 已绑定分支按命中续期：不覆盖绑定 vendor，但把 TTL 刷回一个完整周期
    ///（活跃会话粘住语义不因切到库侧原子面而丢失）。
    #[tokio::test]
    async fn bind_if_absent_renews_ttl_when_already_bound() {
        let table = AffinityTable::with_memory_backend(Duration::from_millis(600));
        assert!(table.bind_if_absent(None, "renew", "vendor-a").await);

        tokio::time::sleep(Duration::from_millis(400)).await;
        let remaining = table
            .backend
            .ttl(&AffinityTable::key(None, "renew"))
            .await
            .unwrap()
            .expect("绑定应仍存活");
        assert!(
            remaining < Duration::from_millis(400),
            "已走过 400ms，剩余 TTL 应不足 400ms（实测 {remaining:?}）"
        );

        assert!(!table.bind_if_absent(None, "renew", "vendor-b").await);
        let renewed = table
            .backend
            .ttl(&AffinityTable::key(None, "renew"))
            .await
            .unwrap()
            .expect("续期后绑定应仍存活");
        assert!(
            renewed > Duration::from_millis(500),
            "已绑定分支应把 TTL 续回约一个完整周期（实测 {renewed:?}）"
        );
    }

    /// 无原子面的后端（dashmap 后端 `as_atomic_writer` 为 None）走 get+set
    /// 兜底，语义与原子面一致：首绑写、后绑不覆盖。
    #[tokio::test]
    async fn bind_if_absent_falls_back_without_atomic_surface() {
        let backend: Arc<dyn CacheBackend + Send + Sync> =
            Arc::new(oxcache::backend::dashmap_memory());
        assert!(
            backend.as_atomic_writer().is_none(),
            "本用例前提：该后端无原子面，必须走兜底 path"
        );
        let table = AffinityTable::new(backend, DEFAULT_AFFINITY_TTL);

        assert!(table.bind_if_absent(None, "s", "vendor-a").await);
        assert!(!table.bind_if_absent(None, "s", "vendor-b").await);
        assert_eq!(
            table.lookup(None, "s").await.as_deref(),
            Some("vendor-a"),
            "兜底 path 同样首绑定收敛"
        );
    }
}
