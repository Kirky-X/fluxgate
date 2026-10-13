// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 定档结果缓存 — oxcache 承载存储（亲和表同款后端）。键不含配置代际：
//! 热更阈值/权重后旧配置算出的档位最长残留一个 TTL（默认 300s）自然过期，
//! 立即生效诉求用 cache_ttl_secs=0 关缓存兜底。键 =
//! `difficulty:{特征 hash}`（有界特征的结构化哈希），值 = 档位决策 JSON。
//! 同特征请求（模板化/重试流量）复用定档，免重复推理。ttl=0 完全旁路；
//! 存取出错静默降级为未命中（F10：缓存自身语义，不影响正确性）。

use super::features::RuleFeatures;
use crate::config::Tier;
use oxcache::backend::CacheBackend;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// 缓存键前缀 — 与 `affinity:` / `resp:` 命名空间隔离
pub const DECISION_KEY_PREFIX: &str = "difficulty:";

/// 缓存的定档结果。path 不缓存（命中恒报 CacheHit，原始路径只进观测）；
/// r_rule 随缓存保存，explain 在命中路径也能给出规则分
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedDecision {
    pub tier: Tier,
    pub score: f64,
    pub r_rule: f64,
}

#[derive(Clone)]
pub struct DecisionCache {
    backend: Arc<dyn CacheBackend + Send + Sync>,
    ttl: Duration,
}

impl DecisionCache {
    pub fn new(backend: Arc<dyn CacheBackend + Send + Sync>, ttl_secs: u64) -> Self {
        Self {
            backend,
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// 以 oxcache 默认内存后端（moka）构造 — 生产/测试的便捷入口
    pub fn with_memory_backend(ttl_secs: u64) -> Self {
        Self::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            ttl_secs,
        )
    }

    /// 特征 → 缓存键：有界文本与结构维全量参与（std 默认哈希，进程内稳定
    /// 即可——缓存不跨进程）；`difficulty:` 前缀与其它命名空间隔离
    pub fn key(features: &RuleFeatures) -> Arc<str> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        features.num_messages.hash(&mut hasher);
        features.body_chars.hash(&mut hasher);
        features.has_tools.hash(&mut hasher);
        features.stream.hash(&mut hasher);
        features.hard_hits.hash(&mut hasher);
        features.easy_hits.hash(&mut hasher);
        features.question_marks.hash(&mut hasher);
        features.math_symbols.hash(&mut hasher);
        features.code_fence.hash(&mut hasher);
        features.text.hash(&mut hasher);
        Arc::from(format!("{DECISION_KEY_PREFIX}{:016x}", hasher.finish()).as_str())
    }

    /// ttl=0 → 不缓存（恒 None）；反序列化失败按未命中（脏值自然过期）
    pub async fn get(&self, key: &str) -> Option<CachedDecision> {
        if self.ttl.is_zero() {
            return None;
        }
        let bytes = self.backend.get(&Arc::from(key)).await.ok()??;
        serde_json::from_slice(&bytes).ok()
    }

    /// ttl=0 → 不写。写失败静默（缓存不影响正确性）
    pub async fn put(&self, key: &str, decision: &CachedDecision) {
        if self.ttl.is_zero() {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(decision) else {
            return;
        };
        let _ = self
            .backend
            .set(Arc::from(key), Arc::new(bytes), Some(self.ttl))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(text: &str) -> RuleFeatures {
        RuleFeatures {
            text: text.to_string(),
            ..RuleFeatures::neutral()
        }
    }

    #[tokio::test]
    async fn put_then_get_roundtrip() {
        let cache = DecisionCache::with_memory_backend(300);
        let key = DecisionCache::key(&features("hard question"));
        let decision = CachedDecision {
            tier: Tier::High,
            score: 0.8,
            r_rule: 0.2,
        };
        cache.put(&key, &decision).await;
        assert_eq!(cache.get(&key).await, Some(decision));
        let other = DecisionCache::key(&features("different"));
        assert_eq!(cache.get(&other).await, None);
    }

    #[tokio::test]
    async fn zero_ttl_bypasses() {
        let cache = DecisionCache::with_memory_backend(0);
        let key = DecisionCache::key(&features("anything"));
        cache
            .put(
                &key,
                &CachedDecision {
                    tier: Tier::Low,
                    score: 0.1,
                    r_rule: 0.1,
                },
            )
            .await;
        assert_eq!(cache.get(&key).await, None, "ttl=0 恒未命中");
    }

    #[tokio::test]
    async fn expired_entry_misses() {
        let cache = DecisionCache::with_memory_backend(1);
        let key = DecisionCache::key(&features("expiring"));
        cache
            .put(
                &key,
                &CachedDecision {
                    tier: Tier::Mid,
                    score: 0.5,
                    r_rule: 0.5,
                },
            )
            .await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(cache.get(&key).await, None, "1s TTL 后过期");
    }

    #[test]
    fn key_is_stable_and_text_sensitive() {
        assert_eq!(
            DecisionCache::key(&features("same")),
            DecisionCache::key(&features("same"))
        );
        assert_ne!(
            DecisionCache::key(&features("a")),
            DecisionCache::key(&features("b"))
        );
        // 结构维参与：文本同、结构不同 → 键不同
        let mut f = features("same");
        f.has_tools = true;
        assert_ne!(
            DecisionCache::key(&features("same")),
            DecisionCache::key(&f)
        );
    }
}
