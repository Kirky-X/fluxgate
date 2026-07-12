// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Oxcache 集成实现 — 委托给 oxcache::Cache<String, String>
//!
//! 使用 oxcache 原生 async API 实现 NexusFlow 的 async Cache trait。
//! 所有方法都是 `async fn`，符合 NexusFlow 的 tokio 异步架构，
//! 避免了 sync API 内部 `block_on` 在 tokio runtime 中 panic 的问题。

use std::sync::Arc;
use std::time::Duration;

use super::Cache;

/// 基于 oxcache::Cache 的缓存实现。
///
/// 内部持有 `oxcache::Cache<String, String>`，通过 oxcache 原生 async API
/// （`get/set/delete/exists/clear`）实现 NexusFlow 的 async Cache trait。
#[derive(Clone)]
pub struct Oxcache {
    inner: Arc<oxcache::Cache<String, String>>,
}

impl Oxcache {
    /// 创建默认缓存（5 分钟 TTL，10000 条目）。
    ///
    /// # Examples
    ///
    /// ```ignore
    /// # #[tokio::main] async fn main() {
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    /// let cache = Oxcache::new().await;
    /// cache.set("key", "value".to_string()).await;
    /// assert_eq!(cache.get("key").await, Some("value".to_string()));
    /// # }
    /// ```
    pub async fn new() -> Self {
        Self::builder().build().await
    }

    /// 创建自定义配置的 builder。
    ///
    /// # Examples
    ///
    /// ```ignore
    /// # #[tokio::main] async fn main() {
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    /// let cache = Oxcache::builder()
    ///     .ttl(Duration::from_secs(600))
    ///     .capacity(5000)
    ///     .build()
    ///     .await;
    /// cache.set("key", "value".to_string()).await;
    /// # }
    /// ```
    pub fn builder() -> OxcacheBuilder {
        OxcacheBuilder::new()
    }
}

#[async_trait::async_trait]
impl Cache for Oxcache {
    async fn get(&self, key: &str) -> Option<String> {
        self.inner.get(&key.to_string()).await.ok().flatten()
    }

    async fn set(&self, key: &str, value: String) {
        let _ = self.inner.set(&key.to_string(), &value).await;
    }

    async fn delete(&self, key: &str) -> bool {
        let existed = self
            .inner
            .exists(&key.to_string())
            .await
            .unwrap_or(false);
        if existed {
            let _ = self.inner.delete(&key.to_string()).await;
        }
        existed
    }

    async fn exists(&self, key: &str) -> bool {
        self.inner
            .exists(&key.to_string())
            .await
            .unwrap_or(false)
    }

    async fn clear(&self) {
        let _ = self.inner.clear().await;
    }
}

/// Builder for [`Oxcache`] with customizable options.
#[derive(Default)]
pub struct OxcacheBuilder {
    ttl: Option<Duration>,
    capacity: Option<usize>,
}

impl OxcacheBuilder {
    /// 创建新的 builder。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 TTL（time-to-live）。
    ///
    /// 超过此时间的条目将自动过期。`None` 表示不过期。
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// 设置最大容量。
    ///
    /// 达到此容量时将应用 LRU 淘汰策略。
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// 构建 [`Oxcache`] 实例。
    ///
    /// # Examples
    ///
    /// ```ignore
    /// # #[tokio::main] async fn main() {
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    /// let cache = Oxcache::builder()
    ///     .ttl(Duration::from_secs(300))
    ///     .capacity(10000)
    ///     .build()
    ///     .await;
    /// cache.set("key", "value".to_string()).await;
    /// # }
    /// ```
    pub async fn build(self) -> Oxcache {
        let mut builder = oxcache::Cache::builder();
        if let Some(ttl) = self.ttl {
            builder = builder.ttl(ttl);
        }
        if let Some(capacity) = self.capacity {
            builder = builder.capacity(capacity as u64);
        }
        let cache = builder.build().await.expect("oxcache build failed");
        Oxcache {
            inner: Arc::new(cache),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_cache() -> Oxcache {
        Oxcache::builder()
            .ttl(Duration::from_secs(300))
            .capacity(1000)
            .build()
            .await
    }

    #[tokio::test]
    async fn test_cache_get_set() {
        let cache = test_cache().await;
        cache.set("key1", "value1".to_string()).await;
        assert_eq!(cache.get("key1").await, Some("value1".to_string()));
    }

    #[tokio::test]
    async fn test_cache_get_missing() {
        let cache = test_cache().await;
        assert_eq!(cache.get("missing_key").await, None);
    }

    #[tokio::test]
    async fn test_cache_delete() {
        let cache = test_cache().await;
        cache.set("key1", "value1".to_string()).await;
        assert!(cache.delete("key1").await);
        assert_eq!(cache.get("key1").await, None);
    }

    #[tokio::test]
    async fn test_cache_delete_missing() {
        let cache = test_cache().await;
        assert!(!cache.delete("missing_key").await);
    }

    #[tokio::test]
    async fn test_cache_exists() {
        let cache = test_cache().await;
        assert!(!cache.exists("key1").await);
        cache.set("key1", "value1".to_string()).await;
        assert!(cache.exists("key1").await);
    }

    #[tokio::test]
    async fn test_cache_clear() {
        let cache = test_cache().await;
        cache.set("key1", "value1".to_string()).await;
        cache.set("key2", "value2".to_string()).await;
        cache.clear().await;
        assert_eq!(cache.get("key1").await, None);
        assert_eq!(cache.get("key2").await, None);
    }

    #[tokio::test]
    async fn test_cache_new() {
        let cache = Oxcache::new().await;
        cache.set("key", "value".to_string()).await;
        assert_eq!(cache.get("key").await, Some("value".to_string()));
    }

    #[tokio::test]
    async fn test_cache_builder() {
        let cache = Oxcache::builder()
            .ttl(Duration::from_secs(300))
            .capacity(1000)
            .build()
            .await;

        cache.set("key", "value".to_string()).await;
        assert_eq!(cache.get("key").await, Some("value".to_string()));
    }

    #[tokio::test]
    async fn test_cache_clone() {
        let cache = test_cache().await;
        cache.set("key", "value".to_string()).await;
        let cloned = cache.clone();
        assert_eq!(cloned.get("key").await, Some("value".to_string()));
    }
}
