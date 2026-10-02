// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! recId=12：精确响应缓存 — 存储落地 oxcache（缓存库已就位必须用它，
//! 绝不手写 HashMap 缓存）。
//!
//! - 键 `resp:{api_key_sha256}:{vendor_id}:{model_id}:{sha256(canonical_body)}`：
//!   vendor+model+完整 canonical body 入键防错命中；调用方 API key 的
//!   SHA256 摘要入键做租户隔离（LaCache 键碰撞投毒面——明文 key 不入键
//!   不入日志）；
//! - canonical_body：`serde_json::to_value` 后**递归按 key 排序**再序列化，
//!   防客户端键序差异错命中（不依赖 serde_json Map 的内部排序实现）；
//! - 门控：仅 `stream==false && tools 为空 && temperature==0 或缺省` 的
//!   确定性请求查/写；`X-No-Cache: 1` 请求头强制回源（Cloudflare 语义，
//!   兼容 `X-Skip-Cache` 别名）；
//! - enabled/TTL 热读 `[cache]` 段（默认关闭，TTL 默认 300s）；
//! - 语义缓存不在本条范围（依赖运行期日志数据，二期顺延）。

use crate::config::{CacheSection, ConfigManager};
use crate::utils::metrics::GlobalMetrics;
use axum::http::HeaderMap;
use oxcache::backend::CacheBackend;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

/// 响应缓存键前缀 — 与亲和表（recId=11）的 `affinity:` 命名空间隔离
pub const RESPONSE_KEY_PREFIX: &str = "resp:";

/// 递归 canonical 化 JSON：对象键按字典序排序后序列化（数组保序——
/// 语义上有序，messages/choices 的顺序参与哈希是正确的）。
pub fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(map.len());
            for key in keys {
                out.insert(key.clone(), canonicalize(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// canonical body 字符串（键序无关的确定性序列化）
pub fn canonical_body(body: &Value) -> String {
    serde_json::to_string(&canonicalize(body)).unwrap_or_default()
}

/// SHA256 hex 摘要
fn sha256_hex(input: &[u8]) -> String {
    hex::encode(Sha256::digest(input))
}

/// 组装缓存键：`resp:{api_key_sha256}:{vendor_id}:{model_id}:{body_hash}`
pub fn response_cache_key(
    api_key_hash: &str,
    vendor_id: &str,
    model_id: &str,
    canonical: &str,
) -> String {
    format!(
        "{RESPONSE_KEY_PREFIX}{}:{}:{}:{}",
        api_key_hash,
        vendor_id,
        model_id,
        sha256_hex(canonical.as_bytes())
    )
}

/// 调用方 API key 的 SHA256 摘要（缓存键租户隔离维度）。
/// 从 Authorization: Bearer / x-api-key 提取（与 auth 中间件同源）；
/// 认证关闭/无 key 的请求统一归入 `anonymous` 租户。
pub fn cache_api_key_hash(headers: &HeaderMap) -> String {
    match crate::middleware::auth::extract_api_key(headers) {
        Some(key) => sha256_hex(key.as_bytes()),
        None => sha256_hex(b"anonymous"),
    }
}

/// 确定性请求判定（缓存门控）：仅
/// `stream==false && tools 为空 && temperature==0 或缺省` 的请求查/写。
/// 有工具调用或带采样语义的请求永不缓存。
pub fn is_cacheable_request(body: &Value) -> bool {
    let streaming = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if streaming {
        return false;
    }
    // tools 缺失或空数组 = 可缓存；存在非空 tools = 不可缓存
    let has_tools = body
        .get("tools")
        .and_then(|v| v.as_array())
        .is_some_and(|tools| !tools.is_empty());
    if has_tools {
        return false;
    }
    // temperature 缺省或 ==0 = 确定性；非数值视为不可缓存（保守）
    match body.get("temperature") {
        None => true,
        Some(v) => v.as_f64() == Some(0.0),
    }
}

/// `X-No-Cache: 1` 跳过缓存（Cloudflare 语义；兼容 `X-Skip-Cache` 别名）。
/// 跳过 = 不读不写。
pub fn skip_cache_requested(headers: &HeaderMap) -> bool {
    ["x-no-cache", "x-skip-cache"].iter().any(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "1")
    })
}

/// 非流式响应的结果缓存。None 接线 = 关闭（与 ApiState 的
/// logger/pre_aggregator 降级语义一致）；接线后 enabled/TTL 每次操作
/// 从热配置现读。
#[derive(Clone)]
pub struct ResponseCache {
    backend: Arc<dyn CacheBackend + Send + Sync>,
    config: Arc<ConfigManager>,
    /// recId=12：全局指标 — TTL 负载联动的 L_p（vendor_ewma 的
    /// ewma_e2e_ms）与 Q（traffic_shares 份额）来源；比 VendorManager
    /// 更轻的依赖面。
    metrics: Arc<GlobalMetrics>,
}

/// 缓存键中与 vendor/model 无关的维度（评审修复 major#2：Miss 不再携带
/// 成品键——resilience 换路成功后 decision.vendor/model 会变成最终成功
/// 者，store 时以**最终** vendor/model + 此处快照的租户/body 维度组装
/// 键，杜绝"vendor-b 的响应写进 vendor-a 的键"的跨 vendor 污染）。
#[derive(Debug, Clone)]
pub struct CacheKeyParts {
    /// 调用方 API key 的 SHA256 摘要（租户隔离维度）
    pub api_key_hash: String,
    /// canonical body 字符串（键序无关的确定性序列化）
    pub canonical: String,
}

/// 单次缓存查询结果
#[derive(Debug)]
pub enum CacheOutcome {
    /// 缓存未接线 / [cache] 关闭 / 请求不可缓存 / X-No-Cache —— 不参与
    Disabled,
    /// 命中：直接以缓存值响应
    Hit(Value),
    /// 未命中：携带键的租户与 body 维度，成功响应后由 [`ResponseCache::store`]
    /// 以最终 vendor/model 组装完整键回写
    Miss(CacheKeyParts),
}

impl CacheOutcome {
    /// 本次请求是否参与了缓存（决定响应是否带 X-Cache 头——Disabled
    /// 不带，Hit/Miss 分别带 HIT/MISS）
    pub fn participated(&self) -> bool {
        matches!(self, CacheOutcome::Hit(_) | CacheOutcome::Miss(_))
    }
}

impl ResponseCache {
    pub fn new(
        backend: Arc<dyn CacheBackend + Send + Sync>,
        config: Arc<ConfigManager>,
        metrics: Arc<GlobalMetrics>,
    ) -> Self {
        Self {
            backend,
            config,
            metrics,
        }
    }

    /// [cache].enabled 热读
    pub fn enabled(&self) -> bool {
        self.config.read().cache.enabled
    }

    /// recId=12：TTL 三段式 —
    /// 1. `base = ttl_secs × 类别乘数`（[[cache.categories]] 按 model_id
    ///    前缀匹配，(endpoint, model) 划分的零开销近似；无命中 1.0）；
    /// 2. 负载联动（`ttl_load_linked` 默认 false = 纯静态、与现状逐位
    ///    一致）：`λ = min(1, (L_p/L_target)·w_L + (Q/Q_target)·w_Q)`；
    /// 3. `实际 = base × (1 + λ·(β_max−1))`。
    ///
    /// **口径降级声明**：论文 L_p 是延迟分位（P95），本实现以
    /// ewma_e2e_ms 均值替代（不引入分位估计器）——高估方向比低估安全
    /// （陈旧容忍度随负载放大符合论文意图）。冷 vendor（无观测）λ=0
    /// → TTL=base，不惩罚冷启动。
    fn ttl(&self, vendor_id: &str, model_id: &str) -> Duration {
        let cache = self.config.read().cache.clone();
        let multiplier = Self::category_multiplier(&cache, model_id);
        let base = (cache.ttl_secs.max(1) as f64) * multiplier;
        let secs = if cache.ttl_load_linked {
            let lambda = self.load_lambda(vendor_id, &cache);
            let beta = if cache.beta_max.is_finite() {
                cache.beta_max.max(1.0)
            } else {
                1.0
            };
            (base * (1.0 + lambda * (beta - 1.0))).max(1.0)
        } else {
            base
        };
        Duration::from_secs(secs as u64)
    }

    /// 类别乘数 — model_id 前缀匹配首条命中（表序优先）；无命中 1.0；
    /// 乘数负值/非有限按 1.0（防御：不产生零/负 TTL）。
    fn category_multiplier(cache: &CacheSection, model_id: &str) -> f64 {
        cache
            .categories
            .iter()
            .find(|c| model_id.starts_with(&c.model_id_prefix))
            .map(|c| c.multiplier)
            .filter(|m| m.is_finite() && *m > 0.0)
            .unwrap_or(1.0)
    }

    /// 负载联动 λ ∈ [0,1] — L_p（EWMA e2e 均值，口径降级见 ttl 注释）
    /// 与 Q（traffic_shares 份额）的加权归一；目标值 ≤0/非有限时该项
    /// 不参与（避免除零）。
    fn load_lambda(&self, vendor_id: &str, cache: &CacheSection) -> f64 {
        let l_p = self
            .metrics
            .vendor_ewma(vendor_id)
            .map(|(ms, _)| ms)
            .unwrap_or(0.0);
        let q = self
            .metrics
            .traffic_shares()
            .iter()
            .find(|(v, _)| v == vendor_id)
            .map(|(_, share)| *share)
            .unwrap_or(0.0);
        let l_term = if cache.l_target.is_finite() && cache.l_target > 0.0 {
            (l_p / cache.l_target) * cache.w_l
        } else {
            0.0
        };
        let q_term = if cache.q_target.is_finite() && cache.q_target > 0.0 {
            (q / cache.q_target) * cache.w_q
        } else {
            0.0
        };
        (l_term + q_term).clamp(0.0, 1.0)
    }

    /// 查询缓存。返回 [`CacheOutcome`]：Disabled（不参与）/ Hit（缓存值）/
    /// Miss（键的租户与 body 维度快照——store 时再拼最终 vendor/model）。
    ///
    /// 查询键以调用方传入的 vendor_id/model_id 组装（初始路由候选）；
    /// resilience 换路成功后回写键由 [`Self::store`] 以最终候选重建
    ///（评审修复 major#2）。
    pub async fn check(
        &self,
        headers: &HeaderMap,
        vendor_id: &str,
        model_id: &str,
        body: &Value,
    ) -> CacheOutcome {
        if !self.enabled() || skip_cache_requested(headers) || !is_cacheable_request(body) {
            return CacheOutcome::Disabled;
        }
        let canonical = canonical_body(body);
        if canonical.is_empty() {
            return CacheOutcome::Disabled;
        }
        let parts = CacheKeyParts {
            api_key_hash: cache_api_key_hash(headers),
            canonical,
        };
        let key = response_cache_key(&parts.api_key_hash, vendor_id, model_id, &parts.canonical);
        match self.backend.get(&key).await {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(cached) => CacheOutcome::Hit(cached),
                // 缓存值损坏视为 miss（下次成功后覆盖）
                Err(_) => CacheOutcome::Miss(parts),
            },
            _ => CacheOutcome::Miss(parts),
        }
    }

    /// 成功响应回写缓存——以**最终成功服务**的 vendor_id/model_id 组装键
    ///（resilience 换路成功后即 fallback 的最终候选，保证键维度与响应体
    /// 一致；TTL 由 [cache].ttl_secs 热读 + 类别乘数 + 负载联动（recId=12，
    /// vendor 维度 L_p/Q）；存取失败静默降级——缓存故障不得影响请求成功率）。
    pub async fn store(
        &self,
        parts: &CacheKeyParts,
        vendor_id: &str,
        model_id: &str,
        response: &Value,
    ) {
        let key = response_cache_key(&parts.api_key_hash, vendor_id, model_id, &parts.canonical);
        if let Ok(bytes) = serde_json::to_vec(response) {
            let ttl = self.ttl(vendor_id, model_id);
            let _ = self
                .backend
                .set(Arc::from(key), Arc::new(bytes), Some(ttl))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_is_key_order_insensitive() {
        let a = serde_json::json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 8});
        let b = serde_json::json!({"max_tokens": 8, "messages": [{"role": "user", "content": "hi"}], "model": "gpt-4"});
        assert_eq!(
            canonical_body(&a),
            canonical_body(&b),
            "键序不同必须同 canonical"
        );

        let nested_a = serde_json::json!({"outer": {"x": 1, "y": [1, {"b": 2, "a": 1}]}});
        let nested_b = serde_json::json!({"outer": {"y": [1, {"a": 1, "b": 2}], "x": 1}});
        assert_eq!(
            canonical_body(&nested_a),
            canonical_body(&nested_b),
            "嵌套对象键序也须归一"
        );

        // 数组保序：顺序不同 = 不同 canonical（messages 语义有序）
        let arr_a = serde_json::json!({"messages": [{"content": "a"}, {"content": "b"}]});
        let arr_b = serde_json::json!({"messages": [{"content": "b"}, {"content": "a"}]});
        assert_ne!(canonical_body(&arr_a), canonical_body(&arr_b));
    }

    #[test]
    fn cache_key_binds_tenant_vendor_model_and_body() {
        let canonical = canonical_body(&serde_json::json!({"model": "gpt-4"}));
        let k1 = response_cache_key("keyhash1", "vendor-a", "m1", &canonical);
        let k2 = response_cache_key("keyhash2", "vendor-a", "m1", &canonical);
        let k3 = response_cache_key("keyhash1", "vendor-b", "m1", &canonical);
        let k4 = response_cache_key("keyhash1", "vendor-a", "m2", &canonical);
        let k5 = response_cache_key("keyhash1", "vendor-a", "m1", &canonical);
        assert_ne!(k1, k2, "不同 API key 必须异键（租户隔离）");
        assert_ne!(k1, k3, "不同 vendor 必须异键");
        assert_ne!(k1, k4, "不同 model 必须异键");
        assert_eq!(k1, k5, "相同维度必须同键");
        assert!(k1.starts_with(RESPONSE_KEY_PREFIX));
        // 明文 key 不入键（键中只出现哈希）
        assert!(!k1.contains("secret-plaintext"));
    }

    #[test]
    fn cacheable_gate() {
        let ok = serde_json::json!({"model": "m", "messages": [], "temperature": 0});
        assert!(is_cacheable_request(&ok));
        // temperature 缺省 = 可缓存
        let no_temp = serde_json::json!({"model": "m", "messages": []});
        assert!(is_cacheable_request(&no_temp));

        // 采样语义不可缓存
        let sampled = serde_json::json!({"model": "m", "messages": [], "temperature": 0.7});
        assert!(!is_cacheable_request(&sampled));
        // 带工具不可缓存
        let with_tools =
            serde_json::json!({"model": "m", "messages": [], "tools": [{"type": "function"}]});
        assert!(!is_cacheable_request(&with_tools));
        let empty_tools = serde_json::json!({"model": "m", "messages": [], "tools": []});
        assert!(is_cacheable_request(&empty_tools), "空 tools 视为无工具");
        // 流式不可缓存
        let streaming = serde_json::json!({"model": "m", "messages": [], "stream": true});
        assert!(!is_cacheable_request(&streaming));
        // temperature 非数值保守视为不可缓存
        let weird = serde_json::json!({"model": "m", "messages": [], "temperature": "auto"});
        assert!(!is_cacheable_request(&weird));
    }

    #[tokio::test]
    async fn cache_roundtrip_via_oxcache_memory_backend() {
        let config = crate::config::ConfigManager::new(crate::config::Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: crate::config::CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            admission: Default::default(),
            resilience: Default::default(),
        });
        let cache = ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            Arc::new(config),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        );
        assert!(cache.enabled());

        let headers = HeaderMap::new();
        let body =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let outcome = cache.check(&headers, "v1", "m1", &body).await;
        let CacheOutcome::Miss(parts) = outcome else {
            panic!("首次查询必须 Miss");
        };
        let response = serde_json::json!({"id": "resp-1", "choices": []});
        cache.store(&parts, "v1", "m1", &response).await;

        let outcome = cache.check(&headers, "v1", "m1", &body).await;
        let CacheOutcome::Hit(cached) = outcome else {
            panic!("回写后必须 Hit");
        };
        assert_eq!(cached, response);

        // X-No-Cache: 1 → Disabled（不读不写）
        let mut no_cache_headers = HeaderMap::new();
        no_cache_headers.insert("x-no-cache", "1".parse().unwrap());
        let outcome = cache.check(&no_cache_headers, "v1", "m1", &body).await;
        assert!(
            matches!(outcome, CacheOutcome::Disabled),
            "X-No-Cache 必须跳过"
        );

        // X-Skip-Cache 别名同样生效
        let mut alias_headers = HeaderMap::new();
        alias_headers.insert("x-skip-cache", "1".parse().unwrap());
        let outcome = cache.check(&alias_headers, "v1", "m1", &body).await;
        assert!(matches!(outcome, CacheOutcome::Disabled));
    }

    /// 评审回归（major#2）：resilience 换路成功后响应必须写进**最终**
    /// vendor/model 的键——初始候选的键不得命中该响应（跨 vendor 污染）。
    #[tokio::test]
    async fn fallback_vendor_response_lands_under_final_vendor_key() {
        let config = crate::config::ConfigManager::new(crate::config::Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: crate::config::CacheSection {
                enabled: true,
                ttl_secs: 300,
                ..Default::default()
            },
            admission: Default::default(),
            resilience: Default::default(),
        });
        let cache = ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            Arc::new(config),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        );

        let headers = HeaderMap::new();
        let body =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        // check 以初始候选 vendor-a/model-x 查询 → Miss
        let CacheOutcome::Miss(parts) = cache.check(&headers, "vendor-a", "model-x", &body).await
        else {
            panic!("首次查询必须 Miss");
        };
        // 换路成功：以最终成功者 vendor-b/model-y 回写（API 层传最终 decision）
        let response = serde_json::json!({"id": "resp-b", "model": "model-y"});
        cache.store(&parts, "vendor-b", "model-y", &response).await;

        // 初始候选键（vendor-a/model-x）不得命中 vendor-b 的响应
        let outcome = cache.check(&headers, "vendor-a", "model-x", &body).await;
        assert!(
            matches!(outcome, CacheOutcome::Miss(_)),
            "初始候选键不得命中换路 vendor 的响应（跨 vendor 污染）"
        );
        // 最终候选键命中
        let outcome = cache.check(&headers, "vendor-b", "model-y", &body).await;
        let CacheOutcome::Hit(cached) = outcome else {
            panic!("最终候选键必须命中");
        };
        assert_eq!(cached, response);
    }
}

#[cfg(test)]
mod ttl_load_linking_tests {
    //! recId=12：TTL 负载联动 + 类别差异化（ttl() 三段式）。

    use super::*;
    use crate::config::Config;
    use crate::utils::metrics::GlobalMetrics;
    use std::time::Instant;

    fn make_config(cache: CacheSection) -> Config {
        Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache,
            admission: Default::default(),
            resilience: Default::default(),
        }
    }

    fn make_cache(cache: CacheSection) -> (ResponseCache, Arc<GlobalMetrics>) {
        let metrics = Arc::new(GlobalMetrics::new());
        let cm = Arc::new(crate::config::ConfigManager::new(make_config(cache)));
        let rc = ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            cm,
            metrics.clone(),
        );
        (rc, metrics)
    }

    /// 开关等价断言：ttl_load_linked=false（默认）+ 类别表空 → ttl 与
    /// 配置值逐位相等（现状语义）。
    #[test]
    fn ttl_load_linked_off_equals_config_value() {
        let (cache, _m) = make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        });
        assert_eq!(cache.ttl("v1", "m1"), Duration::from_secs(300));
    }

    /// 类别乘数：model_id 前缀命中 ×2.0；无命中 1.0；与负载联动开关
    /// 独立生效。
    #[test]
    fn ttl_category_multiplier_hit_and_default() {
        let (cache, _m) = make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            categories: vec![crate::config::CacheCategory {
                model_id_prefix: "gpt-4".into(),
                multiplier: 2.0,
            }],
            ..Default::default()
        });
        assert_eq!(cache.ttl("v1", "gpt-4-turbo"), Duration::from_secs(600));
        assert_eq!(cache.ttl("v1", "claude-3"), Duration::from_secs(300));
    }

    /// λ 钳位：高负载（L_p=5000ms ≫ L_target=1000、w_L=1）→ λ=1 →
    /// TTL = base×β_max 封顶。
    #[tokio::test]
    async fn ttl_load_linked_caps_lambda_at_one() {
        let (cache, metrics) = make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            ttl_load_linked: true,
            w_l: 1.0,
            l_target: 1000.0,
            beta_max: 1.5,
            ..Default::default()
        });
        // 灌 e2e 观测：ewma_latency 5000ms（首样本直接种子）
        metrics.record_request("v1", true, Duration::from_millis(1));
        metrics.record_e2e("v1", Duration::from_millis(5000), Instant::now());

        assert_eq!(
            cache.ttl("v1", "m1"),
            Duration::from_secs(450),
            "λ=1 → TTL = 300×β_max(1.5)"
        );
    }

    /// 冷 vendor（无观测数据）→ λ=0 → TTL=base，不惩罚冷启动。
    #[tokio::test]
    async fn ttl_cold_vendor_keeps_base_ttl() {
        let (cache, _m) = make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            ttl_load_linked: true,
            w_l: 1.0,
            l_target: 1000.0,
            beta_max: 1.5,
            ..Default::default()
        });
        assert_eq!(cache.ttl("cold-vendor", "m1"), Duration::from_secs(300));
    }
}
