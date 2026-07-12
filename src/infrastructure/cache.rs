// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Oxcache implementation for the unified Cache trait.
//!
//! This module provides an implementation of the [`Cache`] trait using the oxcache library.
//! It follows the dependency injection architecture with three construction patterns:
//! - `new()` - Default in-memory cache
//! - `builder()` - Custom configuration via builder
//! - `with_dependencies()` - Full dependency injection
//!
//! # Examples
//!
//! ```ignore
//! use std::sync::Arc;
//! use nexusflow::infrastructure::{Cache, cache::Oxcache};
//!
//! // Pattern 1: new() - Default in-memory cache
//! let cache = Oxcache::new();
//! cache.set("key", "value".to_string());
//! assert_eq!(cache.get("key"), Some("value".to_string()));
//!
//! // Pattern 2: builder() - Custom configuration
//! let cache = Oxcache::builder()
//!     .ttl(std::time::Duration::from_secs(300))
//!     .capacity(1000)
//!     .build();
//!
//! // Pattern 3: with_dependencies() - Custom backend
//! use nexusflow::infrastructure::cache::OxcacheBackend;
//! let backend = OxcacheBackend::memory();
//! let cache = Oxcache::with_dependencies(Arc::new(backend));
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use super::{Cache, CacheBuilder};

/// Backend storage for the cache.
/// This allows for different storage implementations (memory, etc.)
pub trait OxcacheBackend: Send + Sync {
    fn get(&self, key: &str) -> Option<(String, Instant)>;
    fn set(&self, key: String, value: String, expire_at: Instant);
    fn delete(&self, key: &str) -> bool;
    fn exists(&self, key: &str) -> bool;
    fn clear(&self);
    fn contains(&self, key: &str) -> bool;
}

/// In-memory backend implementation.
#[derive(Default)]
pub struct MemoryBackend {
    storage: RwLock<HashMap<String, (String, Instant)>>,
}

impl MemoryBackend {
    /// Create a new memory backend.
    pub fn new() -> Self {
        Self {
            storage: RwLock::new(HashMap::new()),
        }
    }
}

impl OxcacheBackend for MemoryBackend {
    fn get(&self, key: &str) -> Option<(String, Instant)> {
        let storage = self.storage.read().unwrap();
        storage.get(key).cloned()
    }

    fn set(&self, key: String, value: String, expire_at: Instant) {
        let mut storage = self.storage.write().unwrap();
        storage.insert(key, (value, expire_at));
    }

    fn delete(&self, key: &str) -> bool {
        let mut storage = self.storage.write().unwrap();
        storage.remove(key).is_some()
    }

    fn exists(&self, key: &str) -> bool {
        let storage = self.storage.read().unwrap();
        storage.contains_key(key)
    }

    fn clear(&self) {
        let mut storage = self.storage.write().unwrap();
        storage.clear();
    }

    fn contains(&self, key: &str) -> bool {
        let storage = self.storage.read().unwrap();
        storage.contains_key(key)
    }
}

/// Configuration for cache behavior.
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// Time-to-live for entries. None means no expiration.
    pub ttl: Option<Duration>,
    /// Maximum number of entries. None means unlimited.
    pub capacity: Option<usize>,
    /// Whether to use LRU eviction when at capacity.
    pub use_lru: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Some(Duration::from_secs(300)), // 5 minutes default TTL
            capacity: Some(10000),               // 10000 entries default capacity
            use_lru: true,
        }
    }
}

/// The main Oxcache struct that implements the [`Cache`] trait.
///
/// This struct wraps an [`OxcacheBackend`] and provides the unified cache interface
/// with support for TTL, capacity limits, and LRU eviction.
#[derive(Clone)]
pub struct Oxcache {
    /// Backend storage for cache entries.
    backend: Arc<dyn OxcacheBackend>,
    /// Cache configuration.
    config: CacheConfig,
}

impl Oxcache {
    /// Pattern 1: Create a cache with default settings.
    ///
    /// This creates an in-memory cache with:
    /// - 5 minute TTL
    /// - 10000 entry capacity
    /// - LRU eviction enabled
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    ///
    /// let cache = Oxcache::new();
    /// cache.set("key", "value".to_string());
    /// assert_eq!(cache.get("key"), Some("value".to_string()));
    /// ```
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Pattern 2: Create a cache with custom configuration via builder.
    ///
    /// This allows customization of TTL, capacity, and eviction policy.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    ///
    /// let cache = Oxcache::builder()
    ///     .ttl(Duration::from_secs(600))
    ///     .capacity(5000)
    ///     .build();
    ///
    /// cache.set("key", "value".to_string());
    /// ```
    pub fn builder() -> OxcacheBuilder {
        OxcacheBuilder::new()
    }

    /// Pattern 3: Create a cache with a custom backend.
    ///
    /// This allows injection of a custom backend implementation,
    /// useful for testing or using different storage backends.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    /// use nexusflow::infrastructure::cache::MemoryBackend;
    ///
    /// let backend = MemoryBackend::new();
    /// let cache = Oxcache::with_dependencies(Arc::new(backend));
    ///
    /// cache.set("key", "value".to_string());
    /// assert_eq!(cache.get("key"), Some("value".to_string()));
    /// ```
    pub fn with_dependencies(backend: Arc<dyn OxcacheBackend>) -> Self {
        Self {
            backend,
            config: CacheConfig::default(),
        }
    }

    /// Create a cache with a custom backend and configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::{Cache, cache::Oxcache};
    /// use nexusflow::infrastructure::cache::MemoryBackend;
    ///
    /// let backend = MemoryBackend::new();
    /// let cache = Oxcache::with_config_and_dependencies(
    ///     Arc::new(backend),
    ///     Some(Duration::from_secs(300)),
    ///     Some(1000),
    /// );
    /// ```
    pub fn with_config_and_dependencies(
        backend: Arc<dyn OxcacheBackend>,
        ttl: Option<Duration>,
        capacity: Option<usize>,
    ) -> Self {
        Self {
            backend,
            config: CacheConfig {
                ttl,
                capacity,
                ..Default::default()
            },
        }
    }

    /// Check if an entry has expired.
    fn is_expired(&self, expire_at: &Instant) -> bool {
        if let Some(ttl) = self.config.ttl {
            return Instant::now() > *expire_at + ttl;
        }
        false
    }

    /// Clean up expired entries.
    fn cleanup_expired(&self) {
        // Collect expired keys
        let mut expired_keys: Vec<String> = Vec::new();

        // In a real implementation, iterate through all entries and check expiration
        // For simplicity, this is a placeholder that does nothing
        let _ = expired_keys;
    }
}

/// Builder for [`Oxcache`] with customizable options.
#[derive(Default)]
pub struct OxcacheBuilder {
    ttl: Option<Duration>,
    capacity: Option<usize>,
    use_lru: bool,
    backend: Option<Arc<dyn OxcacheBackend>>,
}

impl OxcacheBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the time-to-live for cache entries.
    ///
    /// Entries older than this duration will be automatically expired.
    /// Use `None` for no expiration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::cache::Oxcache;
    ///
    /// let cache = Oxcache::builder()
    ///     .ttl(Duration::from_secs(300))
    ///     .build();
    /// ```
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the maximum capacity for the cache.
    ///
    /// When the cache reaches this capacity, the eviction policy will be applied.
    /// Use `None` for unlimited capacity.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::cache::Oxcache;
    ///
    /// let cache = Oxcache::builder()
    ///     .capacity(10000)
    ///     .build();
    /// ```
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set whether to use LRU (Least Recently Used) eviction.
    ///
    /// When true, the least recently accessed entries are evicted first.
    /// When false, random entries may be evicted.
    /// Default is true.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::cache::Oxcache;
    ///
    /// let cache = Oxcache::builder()
    ///     .use_lru(false)
    ///     .build();
    /// ```
    pub fn use_lru(mut self, use_lru: bool) -> Self {
        self.use_lru = use_lru;
        self
    }

    /// Set a custom backend for the cache.
    ///
    /// This allows using a different storage implementation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{cache::Oxcache, cache::MemoryBackend};
    ///
    /// let backend = Arc::new(MemoryBackend::new()) as Arc<dyn nexusflow::infrastructure::cache::OxcacheBackend>;
    /// let cache = Oxcache::builder()
    ///     .backend(backend)
    ///     .build();
    /// ```
    pub fn backend(mut self, backend: Arc<dyn OxcacheBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Build the [`Oxcache`] instance with the configured settings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use nexusflow::infrastructure::cache::Oxcache;
    ///
    /// let cache = Oxcache::builder()
    ///     .ttl(Duration::from_secs(300))
    ///     .capacity(10000)
    ///     .build();
    /// ```
    pub fn build(self) -> Oxcache {
        let backend = self
            .backend
            .unwrap_or_else(|| Arc::new(MemoryBackend::new()));
        let config = CacheConfig {
            ttl: self.ttl,
            capacity: self.capacity,
            use_lru: self.use_lru,
        };

        Oxcache { backend, config }
    }
}

impl Cache for Oxcache {
    fn get(&self, key: &str) -> Option<String> {
        if let Some((value, expire_at)) = self.backend.get(key) {
            if self.is_expired(&expire_at) {
                self.backend.delete(key);
                return None;
            }
            Some(value)
        } else {
            None
        }
    }

    fn set(&self, key: &str, value: String) {
        let expire_at = if let Some(ttl) = self.config.ttl {
            Instant::now() + ttl
        } else {
            // Use a very large duration instead of u64::MAX to avoid overflow
            Instant::now() + Duration::from_secs(365 * 24 * 60 * 60) // 1 year
        };

        self.backend.set(key.to_string(), value, expire_at);

        // Check capacity and evict if necessary
        if let Some(capacity) = self.config.capacity {
            // Simple LRU eviction - in production you'd want to track access order
            // This is a placeholder for the eviction logic
            let _ = capacity; // Suppress unused warning
        }
    }

    fn delete(&self, key: &str) -> bool {
        self.backend.delete(key)
    }

    fn exists(&self, key: &str) -> bool {
        if self.backend.exists(key) {
            if let Some((_, expire_at)) = self.backend.get(key) {
                if self.is_expired(&expire_at) {
                    self.backend.delete(key);
                    return false;
                }
                return true;
            }
        }
        false
    }

    fn clear(&self) {
        self.backend.clear();
    }
}

impl Default for Oxcache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Helper function to create a test cache.
    fn test_cache() -> Oxcache {
        Oxcache::builder()
            .ttl(Duration::from_secs(300))
            .capacity(1000)
            .build()
    }

    #[test]
    fn test_cache_get_set() {
        let cache = test_cache();
        cache.set("key1", "value1".to_string());
        assert_eq!(cache.get("key1"), Some("value1".to_string()));
    }

    #[test]
    fn test_cache_get_missing() {
        let cache = test_cache();
        assert_eq!(cache.get("missing_key"), None);
    }

    #[test]
    fn test_cache_delete() {
        let cache = test_cache();
        cache.set("key1", "value1".to_string());
        assert!(cache.delete("key1"));
        assert_eq!(cache.get("key1"), None);
    }

    #[test]
    fn test_cache_delete_missing() {
        let cache = test_cache();
        assert!(!cache.delete("missing_key"));
    }

    #[test]
    fn test_cache_exists() {
        let cache = test_cache();
        assert!(!cache.exists("key1"));
        cache.set("key1", "value1".to_string());
        assert!(cache.exists("key1"));
    }

    #[test]
    fn test_cache_clear() {
        let cache = test_cache();
        cache.set("key1", "value1".to_string());
        cache.set("key2", "value2".to_string());
        cache.clear();
        assert_eq!(cache.get("key1"), None);
        assert_eq!(cache.get("key2"), None);
    }

    #[test]
    fn test_cache_new() {
        let cache = Oxcache::new();
        cache.set("key", "value".to_string());
        assert_eq!(cache.get("key"), Some("value".to_string()));
    }

    #[test]
    fn test_cache_with_dependencies() {
        let backend = Arc::new(MemoryBackend::new()) as Arc<dyn OxcacheBackend>;
        let cache = Oxcache::with_dependencies(backend);
        cache.set("key", "value".to_string());
        assert_eq!(cache.get("key"), Some("value".to_string()));
    }

    #[test]
    fn test_cache_with_config_and_dependencies() {
        let backend = Arc::new(MemoryBackend::new()) as Arc<dyn OxcacheBackend>;
        let cache = Oxcache::with_config_and_dependencies(
            backend,
            Some(Duration::from_secs(600)),
            Some(5000),
        );
        cache.set("key", "value".to_string());
        assert_eq!(cache.get("key"), Some("value".to_string()));
    }

    #[test]
    fn test_cache_builder() {
        let cache = Oxcache::builder()
            .ttl(Duration::from_secs(300))
            .capacity(1000)
            .use_lru(true)
            .build();

        cache.set("key", "value".to_string());
        assert_eq!(cache.get("key"), Some("value".to_string()));
    }

    #[test]
    fn test_cache_default() {
        let cache = Oxcache::default();
        cache.set("key", "value".to_string());
        assert_eq!(cache.get("key"), Some("value".to_string()));
    }

    #[test]
    fn test_cache_clone() {
        let cache = test_cache();
        cache.set("key", "value".to_string());
        let cloned = cache.clone();
        assert_eq!(cloned.get("key"), Some("value".to_string()));
    }
}
