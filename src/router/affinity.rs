// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! recId=11：会话/前缀亲和表 — oxcache 承载存储。
//!
//! 语义（bifrost 式"上次成功服务的 vendor"）：
//! - 存储走 oxcache `CacheBackend`（`CacheWriter::set` 带 TTL、
//!   `CacheReader::get` 读取、`CacheWriter::expire` 续期），不手写 map；
//! - 键 `affinity:{session_key}` 与响应缓存的 `resp:` 命名空间隔离；
//!   recId=10(2)：`affinity_tenant_scoped` 开启时带租户维度
//!   `affinity:{api_key_hash}:{session_key}`（租户劫持修复）；
//! - 值为 vendor_id 的 UTF-8 字节；
//! - TTL 活跃续期：lookup 命中即 `expire` 重置 TTL —— 活跃会话粘住，
//!   闲置会话到点自动过期；
//! - 只记"上次成功服务的 vendor"：路由层不写绑定，执行成功后由 API 层
//!   回写（见 `crate::api` 的 bind/clear helper）；
//! - 亲和是 boost 不是硬粘：命中 vendor 不在合格集合内时清除旧绑定，
//!   绝不强选（负载均衡与故障迁移语义优先）。
//!
//! 存取出错一律静默降级为无亲和（缓存故障不得影响路由可用性）。

use oxcache::backend::CacheBackend;
use std::sync::Arc;
use std::time::Duration;

/// 亲和键前缀 — 与响应缓存（recId=12）的 `resp:` 命名空间隔离
pub const AFFINITY_KEY_PREFIX: &str = "affinity:";

/// 默认亲和 TTL 1h — 每次命中（活跃请求）续期，闲置会话 1h 后过期
pub const DEFAULT_AFFINITY_TTL: Duration = Duration::from_secs(3600);

/// 会话 → 上次成功服务的 vendor 的亲和表
#[derive(Clone)]
pub struct AffinityTable {
    backend: Arc<dyn CacheBackend + Send + Sync>,
    ttl: Duration,
}

impl AffinityTable {
    /// 以给定 oxcache 后端与 TTL 构造
    pub fn new(backend: Arc<dyn CacheBackend + Send + Sync>, ttl: Duration) -> Self {
        Self { backend, ttl }
    }

    /// 以 oxcache 默认内存后端（moka）构造 — 生产/测试的便捷入口
    pub fn with_memory_backend(ttl: Duration) -> Self {
        Self::new(Arc::new(oxcache::backend::default_memory_backend()), ttl)
    }

    /// recId=10(2)：亲和键构造 — `affinity:{session_key}`（legacy，默认）；
    /// `affinity_tenant_scoped` 开启时调用方传 `Some(api_key_hash)` →
    /// `affinity:{api_key_hash}:{session_key}`（租户隔离：两租户同
    /// session id 不得互相劫持）。None/空租户一律 legacy 键。
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
        let key = Self::key(tenant, session_key);
        let value = self.backend.get(&key).await.ok()??;
        // 活跃续期：命中即重置 TTL；键恰在此刻被逐出时 expire 返回 false，
        // 无碍本次命中（下次请求自然 miss 重新绑定）
        let _ = self.backend.expire(&key, self.ttl).await;
        String::from_utf8(value).ok()
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

    /// recId=10(3)：首绑定收敛（get+set 模拟 SetNX）— 已绑定时不覆盖并
    /// 返回 false，未绑定写入返回 true。供 API 层派发成功后的首绑定；
    /// 并行首请求下竞态窗口仍可能双写（后写胜出），报告已声明可接受。
    /// 命中已有绑定时照常续期 TTL（与 lookup 的活跃续期同拍）。
    pub async fn bind_if_absent(
        &self,
        tenant: Option<&str>,
        session_key: &str,
        vendor_id: &str,
    ) -> bool {
        let key = Self::key(tenant, session_key);
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

    /// 解除会话绑定 — 失败迁移耗尽（最终失败）或绑定 vendor 已不在
    /// 合格集合时调用，避免旧键指向失效 vendor
    pub async fn unbind(&self, tenant: Option<&str>, session_key: &str) {
        let _ = self.backend.delete(&Self::key(tenant, session_key)).await;
    }
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

    /// TTL 活跃续期：50ms TTL 内每次 lookup 都重置过期点——连续 lookup
    /// 跨越原 TTL 后值仍存活（活跃会话粘住）。
    #[tokio::test]
    async fn lookup_renews_ttl_for_active_sessions() {
        let table = AffinityTable::with_memory_backend(Duration::from_millis(80));
        table.bind(None, "hot", "vendor-a").await;
        // 3 × 50ms = 150ms > 原 TTL 80ms；若无续期，第三次 lookup 前
        // 该键已过期
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
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

    // ============ recId=10(2) tests：键租户隔离（Red 先行） ============

    /// 两租户同 session id 不得互相劫持：tenant-A 绑定后，同 session key
    /// 的 tenant-B lookup 必须 None（现状无租户维度会命中 vendor-a——
    /// 本测试在实现前为 Red）。租户键与 legacy 键空间互不可见。
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

    // ============ recId=10(3) tests：bind_if_absent 首绑定收敛 ============

    /// 未绑定写入返回 true；已绑定不覆盖返回 false（get+set 模拟 SetNX，
    /// 并行首请求竞态窗口可接受——报告已声明）。
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
}
