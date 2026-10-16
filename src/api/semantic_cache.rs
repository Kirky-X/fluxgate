// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 语义响应缓存（O4，`semantic-cache` feature 门控）：精确 miss 后的
//! 嵌入相似检索层。租户强制隔离（检索域限同 api_key_hash，无共享开关，
//! 防 GPTCache 式键碰撞投毒 arXiv:2601.23088）；线性余弦检索（容量
//! ≤max_entries，µs 级）；嵌入失败静默跳过并计数，不阻塞写路径。

#[cfg(feature = "semantic-cache")]
use crate::i18n::t_args;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 嵌入器抽象：生产实现为 vecboost BGE（spawn_blocking 包裹），
/// 测试用确定性 fake
pub trait SemanticEmbedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

#[derive(Debug, Clone)]
struct Entry {
    tenant: String,
    vector: Vec<f32>,
    response: Value,
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    store_failures: AtomicU64,
    evictions: AtomicU64,
}

pub struct SemanticCacheLayer {
    similarity: f64,
    max_entries: usize,
    entries: Mutex<Vec<Entry>>,
    embedder: Arc<dyn SemanticEmbedder>,
    counters: Counters,
}

impl SemanticCacheLayer {
    pub fn new(similarity: f64, max_entries: usize, embedder: Arc<dyn SemanticEmbedder>) -> Self {
        Self {
            similarity: similarity.clamp(0.0, 1.0),
            max_entries: max_entries.max(1),
            entries: Mutex::new(Vec::new()),
            embedder,
            counters: Counters::default(),
        }
    }

    /// 嵌入查询文本（有界：头部 1024 字符，与难度定档同口径）
    fn embed_query(&self, body: &Value) -> Result<Vec<f32>, String> {
        let text = query_text(body);
        self.embedder.embed(&text)
    }

    /// 语义检索：同租户内余弦相似度 ≥ 阈值的最近条目
    pub async fn lookup(&self, tenant: &str, body: &Value) -> Option<Value> {
        // BGE 嵌入为同步 CPU 推理：spawn_blocking 防止阻塞 tokio worker
        let embedder = self.embedder.clone();
        let embed_text = query_text(body);
        let vector = match tokio::task::spawn_blocking(move || embedder.embed(&embed_text)).await {
            Ok(Ok(v)) => v,
            _ => {
                self.counters.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let best = guard
            .iter()
            .filter(|e| e.tenant == tenant)
            .filter_map(|e| cosine(&vector, &e.vector).map(|sim| (sim, e)))
            .filter(|(sim, _)| *sim >= self.similarity as f32)
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match best {
            Some((_, e)) => {
                self.counters.hits.fetch_add(1, Ordering::Relaxed);
                Some(e.response.clone())
            }
            None => {
                self.counters.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// 入列（嵌入失败静默 + 计数）；容量满 FIFO 逐出
    pub fn store(&self, tenant: &str, body: &Value, response: &Value) {
        let vector = match self.embed_query(body) {
            Ok(v) => v,
            Err(_) => {
                self.counters.store_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let mut guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if guard.len() >= self.max_entries {
            guard.remove(0);
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }
        guard.push(Entry {
            tenant: tenant.to_string(),
            vector,
            response: response.clone(),
        });
    }

    /// 观测快照（hits/misses/evictions）
    pub fn counters(&self) -> (u64, u64, u64) {
        (
            self.counters.hits.load(Ordering::Relaxed),
            self.counters.misses.load(Ordering::Relaxed),
            self.counters.evictions.load(Ordering::Relaxed),
        )
    }
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= f32::EPSILON {
        None
    } else {
        Some(dot / denom)
    }
}

/// 查询文本：最后一条 user 消息的头部 1024 字符。
/// 不含 system/tools——它们被所有请求共享，纳入嵌入会让不同问题的
/// 相似度被共同前缀抬高，造成跨问题错误命中
fn query_text(body: &Value) -> String {
    body.as_object()
        .and_then(|o| o.get("messages"))
        .and_then(|m| m.as_array())
        .and_then(|a| {
            a.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.chars().take(1024).collect())
        .unwrap_or_default()
}

/// vecboost BGE 嵌入器（feature 门控下的生产实现）
#[cfg(feature = "semantic-cache")]
pub struct BgeEmbedder {
    engine: Arc<dyn vecboost::engine::InferenceEngine>,
}

#[cfg(feature = "semantic-cache")]
impl BgeEmbedder {
    /// 加载 BGE embedding checkpoint（阻塞，bootstrap 一次性）
    pub fn load(model_path: &str) -> Result<Self, String> {
        let config = vecboost::config::model::ModelConfig {
            name: "nexusflow-semantic-cache".to_string(),
            engine_type: vecboost::config::model::EngineType::Candle,
            model_path: model_path.into(),
            task: vecboost::config::model::ModelTask::Embedding,
            device: vecboost::config::model::DeviceType::Cpu,
            ..vecboost::config::model::ModelConfig::default()
        };
        let any = vecboost::engine::EngineFactory::create(
            vecboost::config::model::EngineType::Candle,
            &config,
        )
        .map_err(|e| {
            t_args(
                "api-semantic-cache-embedder-load-failed",
                &[("error", e.to_string())],
            )
        })?;
        Ok(Self {
            engine: Arc::new(any),
        })
    }
}

#[cfg(feature = "semantic-cache")]
impl SemanticEmbedder for BgeEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.engine
            .embed(text)
            .map_err(|e| format!("embed failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeEmbedder;
    impl SemanticEmbedder for FakeEmbedder {
        // 词面重叠向量：含 "refund" → (1,0)，否则 (0,1)——确定性可断言
        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            if text.contains("refund") {
                Ok(vec![1.0, 0.0])
            } else {
                Ok(vec![0.0, 1.0])
            }
        }
    }

    fn layer() -> SemanticCacheLayer {
        SemanticCacheLayer::new(0.92, 2, Arc::new(FakeEmbedder))
    }

    #[tokio::test]
    async fn same_topic_hit_same_tenant_only() {
        let layer = layer();
        let q = json!({"messages": [{"role": "user", "content": "I want a refund"}]});
        let resp = json!({"answer": "refund policy"});
        layer.store("tenant-a", &q, &resp);
        let hit = layer.lookup("tenant-a", &q).await.expect("同租户命中");
        assert_eq!(hit["answer"], "refund policy");
        // 跨租户：强制隔离，同文本也不命中
        assert!(layer.lookup("tenant-b", &q).await.is_none());
    }

    #[tokio::test]
    async fn below_threshold_misses() {
        let layer = layer();
        let q = json!({"messages": [{"role": "user", "content": "I want a refund"}]});
        let other = json!({"messages": [{"role": "user", "content": " totally different topic"}]});
        layer.store("t", &q, &json!({"a": 1}));
        assert!(layer.lookup("t", &other).await.is_none(), "正交向量不命中");
    }

    #[tokio::test]
    async fn fifo_eviction_at_capacity() {
        fn msg_body(text: &str) -> serde_json::Value {
            json!({"messages": [{"role": "user", "content": text}]})
        }
        let layer = SemanticCacheLayer::new(0.5, 1, Arc::new(FakeEmbedder));
        layer.store("t", &msg_body("refund a"), &json!({"r": 1}));
        layer.store("t", &msg_body(" totally other"), &json!({"r": 2}));
        // 第一条被逐出
        assert!(layer.lookup("t", &msg_body("refund a")).await.is_none());
        assert!(layer
            .lookup("t", &msg_body(" totally other"))
            .await
            .is_some());
        let (hits, misses, evictions) = layer.counters();
        assert_eq!(evictions, 1);
        assert_eq!(hits, 1);
        assert!(misses >= 1);
    }
}
