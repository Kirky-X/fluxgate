// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 精确响应缓存 — 存储与并发去重均落地 oxcache（缓存库已就位必须用它，
//! 绝不手写 HashMap 缓存 / 在途表）。
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
//! - single-flight（并发去重）：由 oxcache `Cache::get_or_with_ttl` 承载
//!   （64 路分片注册表、watch 唤醒、`AsyncSfGuard` panic/早退安全），
//!   入口 [`ResponseCache::get_or_response`]。与退役手写版的已知行为
//!   差异及缓解（design.md 钉住、测试承载）：
//!   - 差异 A（等待模型）：follower 在 watch 上无超时等待（手写版 30s
//!     超时窃取领导权自行回源）→ 闭包内 `timeout(DISPATCH_TIMEOUT)` 钳制
//!     leader 派发：超时转 Err 放行 followers，等价恢复「30s 自愈」；
//!     4096 在途表硬界随注册表删除（分片条目完成即移除，生命周期 ≤ 单
//!     请求派发时长）。
//!   - 差异 B（失败传播）：leader 失败 → followers 得
//!     `OxCacheError::L1Error`（leader-not-cached 标记）不回源 → 调用侧
//!     （`get_or_response`）自愈回源一次，恢复手写版窃取语义，错误不透传
//!     客户端；leader 自身派发错误经 err_cell 原样传播。
//! - 语义缓存不在本条范围（依赖运行期日志数据，二期顺延）。

use crate::config::{CacheSection, ConfigManager};
use crate::error::{GateError, NexusFlowError};
use crate::utils::metrics::GlobalMetrics;
use axum::http::HeaderMap;
use oxcache::backend::CacheBackend;
use oxcache::OxCacheError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// get_or 闭包内派发的超时钳制（设计差异 A 缓解）：oxcache follower 在
/// watch 上无超时等待，释放仅取决于 leader future 的完成/panic/早退——
/// 闭包内以此时长上限包裹派发：leader 超时转 Err → `AsyncSfGuard` 放行
/// followers，等价恢复手写版「30s 自愈回源」，leader 永久挂起面亦被
/// 钳制（覆盖上游 P99 时延）。
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// leader 闭包内派发失败的库错误携带标记（原始 `NexusFlowError` 经
/// err_cell 原样取回，见 [`ResponseCache::get_or_response`]）。
const LEADER_DISPATCH_FAILED: &str = "nexusflow:leader-dispatch-failed";

/// leader 闭包内派发超时的库错误携带标记（映射 `GateError::Timeout`）。
const LEADER_DISPATCH_TIMEOUT: &str = "nexusflow:leader-dispatch-timeout";

/// 响应缓存键前缀 — 与亲和表的 `affinity:` 命名空间隔离
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

/// 调用方 API key 的 SHA256 摘要（缓存键租户隔离维度）——实现已下沉
/// [`crate::utils::tenant::api_key_digest`]（认证/亲和/配额同源单实现）。
pub use crate::utils::tenant::api_key_digest as cache_api_key_hash;

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
    /// 存储本体 — oxcache `Cache<String, Value>`（JSON 序列化面，`Arc`
    /// 包裹使结构体保持 `Clone`——库侧 Cache 未实现 Clone）：
    /// `get_or` single-flight 注册表（64 路分片）、watch 唤醒、
    /// AsyncSfGuard panic/早退安全均由库承载。库侧 `ttl_jitter_factor`
    /// 构建期显式置 0——TTL 抖动是消费侧策略（`[cache].ttl_jitter`
    /// 热读，见 [`Self::apply_ttl_jitter`]），置 0 防「配置抖动 + 库
    /// 默认 0.1」双重抖动。
    cache: Arc<oxcache::Cache<String, Value>>,
    config: Arc<ConfigManager>,
    /// 全局指标 — TTL 负载联动的 L_p（vendor_ewma 的
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

/// 回写键的最终路由维度（评审修复 major#2：resilience 换路成功后以
/// **最终成功服务者**的 vendor/model 组装键——初始候选键回写会把
/// vendor-b 的响应写进 vendor-a 的键，跨 vendor 污染）。由消费侧
/// （handler）对其路由决策实现——response_cache 不反向依赖 router
/// 类型（mod 接口隔离）。
pub trait FinalRoute {
    fn vendor_id(&self) -> &str;
    fn model_id(&self) -> &str;
}

/// [`ResponseCache::get_or_response`] 的闭包流结果——替代原
/// `cache_probe` → `cache_hit_response` → `persist_cache_miss` 三段。
#[derive(Debug)]
pub struct CacheFlow<R> {
    /// 响应体（leader 计算值 / follower 复用值 / 未参与时的直派发值）
    pub value: Value,
    /// 最终成功路由：`Some` = 本次闭包派发产出（leader / 自愈回源 /
    /// 未参与缓存的直派发）；`None` = 缓存复用（快速路径命中 / follower
    /// 复用）——调用方以其**初始**路由决策收尾（与手写版
    /// `cache_hit_response` 同口径）。
    pub final_route: Option<R>,
    /// 是否由本次请求计算（X-Cache 语义）：`true` = leader 或自愈回源
    /// → MISS；`false` = 缓存复用 → HIT。
    pub computed: bool,
    /// 是否走了缓存面：`false` = 未接线 / [cache] 关闭 / 不可缓存 /
    /// X-No-Cache / 空 canonical——响应不带 X-Cache 头。
    pub participated: bool,
}

impl ResponseCache {
    pub fn new(
        backend: Arc<dyn CacheBackend + Send + Sync>,
        config: Arc<ConfigManager>,
        metrics: Arc<GlobalMetrics>,
    ) -> Self {
        // 单后端 + 无 service_name：builder 唯一拒绝路径（多后端/空
        // service）不成立，expect 为构建期不可能失败而非吞错。
        // 用 builder 而非 `Cache::with_dependencies`：后者固定库侧
        // ttl_jitter_factor=0.1，会把默认关闭的 `[cache].ttl_jitter`
        // 悄悄打开（store 行为不再逐位不变）；builder 显式置 0 后
        // 抖动策略单一来源留在消费侧（apply_ttl_jitter）。
        let cache = Arc::new(
            oxcache::Cache::<String, Value>::builder()
                .backend_arc(backend)
                .ttl_jitter(0.0)
                .build_sync()
                .expect("single-backend CacheBuilder build cannot fail"),
        );
        Self {
            cache,
            config,
            metrics,
        }
    }

    /// [cache].enabled 热读
    pub fn enabled(&self) -> bool {
        self.config.read().cache.enabled
    }

    /// 后端可用性探针（G7 就绪检查用）：`health_check` 透传，错误转字符串。
    pub async fn probe_backend(&self) -> Result<(), String> {
        self.cache.health_check().await.map_err(|e| e.to_string())
    }

    /// TTL 三段式 —
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

    /// 查询缓存（低层直读原语）。返回 [`CacheOutcome`]：Disabled（不参与）/
    /// Hit（缓存值，含负缓存命中的 `Hit(Null)`）/ Miss（携带键的租户与
    /// body 维度，回写由 [`Self::store`] 以最终 vendor/model 组装完整键）。
    ///
    /// 生产路径的并发去重（single-flight）由 [`Self::get_or_response`] 经
    /// oxcache `get_or_with_ttl` 承载——本原语只读不写、不去重。
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
        // 直读经 `Cache::get`（库序列化面解出 Value）：Err（损坏值）按
        // Miss 重源——下次成功写覆盖，与手写 decode 期语义一致。
        match self.cache.get(&key).await {
            Ok(Some(value)) => CacheOutcome::Hit(value),
            Ok(None) | Err(_) => CacheOutcome::Miss(parts),
        }
    }

    /// 对称 TTL 抖动（与 oxcache `apply_jitter` 同口径）：
    /// `ttl×(1+factor×u)`，u∈[-1,1]；factor≤0 或 u 非有限时原样返回。
    /// 纯函数（随机源由调用方注入）， floor 1ms。
    /// 分叉说明：oxcache 的实现为 `pub(super)`，
    /// 且哨兵常量所在 `core` 模块未公开，无法引用——此处复刻口径，
    /// 语义与库一致（对称界、1ms 下限、关闭即原样）。
    fn apply_ttl_jitter(ttl: Duration, factor: f64, uniform_minus1_1: f64) -> Duration {
        if factor.is_nan() || factor <= 0.0 || !uniform_minus1_1.is_finite() {
            return ttl;
        }
        let jittered = ttl.as_millis() as f64 * (1.0 + factor * uniform_minus1_1.clamp(-1.0, 1.0));
        Duration::from_millis(jittered.max(1.0) as u64)
    }

    /// 成功响应回写缓存——以**最终成功服务**的 vendor_id/model_id 组装键
    ///（resilience 换路成功后即 fallback 的最终候选，保证键维度与响应体
    /// 一致；TTL 由 [cache].ttl_secs 热读 + 类别乘数 + 负载联动（
    /// vendor 维度 L_p/Q）；存取失败静默降级——缓存故障不得影响请求成功率）。
    ///
    /// 写入 TTL 叠加 `ttl_jitter` 对称抖动（同批键过期不齐峰，关闭时
    /// 精确）；Null 响应写 JSON null 值（`null_cache_ttl_secs` 短窗口，
    /// 窗口内 check 直接 `Hit(Null)` 不再穿透）。
    pub async fn store(
        &self,
        parts: &CacheKeyParts,
        vendor_id: &str,
        model_id: &str,
        response: &Value,
    ) {
        let key = response_cache_key(&parts.api_key_hash, vendor_id, model_id, &parts.canonical);
        // Null 响应 = JSON null 值（负缓存：窗口内 check 得 Hit(Null)
        // 不再穿透），短窗口 TTL；非 Null 三段式 TTL + 配置抖动
        // （库侧因子已置 0，见 new()）。
        let ttl = if response.is_null() {
            Duration::from_secs(self.config.read().cache.null_cache_ttl_secs.max(1))
        } else {
            let factor = self.config.read().cache.ttl_jitter;
            let uniform = rand::random::<f64>() * 2.0 - 1.0;
            Self::apply_ttl_jitter(self.ttl(vendor_id, model_id), factor, uniform)
        };
        // 写路径经 `Cache::set_with_ttl`（库序列化 + 写路径指标/审计）
        let _ = self.cache.set_with_ttl(&key, response, Some(ttl)).await;
    }

    /// get_or 闭包流 — single-flight 由 oxcache `Cache::get_or_with_ttl`
    /// 承载（64 路分片注册表 + watch 唤醒 + `AsyncSfGuard` panic/早退
    /// 安全），替代手写在途表：同 key 并发只放行第一个（leader）执行
    /// `dispatch` 闭包，其余订阅其完成信号后复用缓存值。
    ///
    /// 已知行为差异与缓解（design.md 钉住）：
    /// - 差异 A（等待模型）：follower 在 watch 上无超时等待、无 4096
    ///   在途表硬界——闭包内 `timeout(DISPATCH_TIMEOUT)` 钳制派发
    ///   恢复「30s 自愈」：leader 超时转 Err 放行 followers；分片表
    ///   条目完成即移除，无上界（生命周期 ≤ 单请求派发时长）。
    /// - 差异 B（失败传播）：leader 失败 → followers 重读未命中得
    ///   `L1Error(leader-not-cached)`——调用侧（本方法）自愈回源一次
    ///   （恢复手写版窃取语义，不透传客户端）；leader 自身派发错误经
    ///   err_cell 原样映射 `NexusFlowError` 传播。
    ///
    /// 换路场景（major#2）：闭包成功后以**最终**路由维度写完整键，
    /// `get_or` 返回后删除初始候选键的库侧写入（毫秒级残余窗口内晚到
    /// follower 重读落空，走自愈回源——见 design Consequences）。
    pub async fn get_or_response<R, F, Fut>(
        &self,
        headers: &HeaderMap,
        vendor_id: &str,
        model_id: &str,
        body: &Value,
        dispatch: F,
    ) -> Result<CacheFlow<R>, NexusFlowError>
    where
        R: FinalRoute,
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<(Value, R), NexusFlowError>>,
    {
        // 门控与手写 check 同口径：未参与 = 闭包直派发（不经库、无超时）
        let participate =
            self.enabled() && !skip_cache_requested(headers) && is_cacheable_request(body);
        let canonical = if participate {
            canonical_body(body)
        } else {
            String::new()
        };
        if !participate || canonical.is_empty() {
            let (value, route) = dispatch().await?;
            return Ok(CacheFlow {
                value,
                final_route: Some(route),
                computed: false,
                participated: false,
            });
        }

        let api_key_hash = cache_api_key_hash(headers);
        let initial_key = response_cache_key(&api_key_hash, vendor_id, model_id, &canonical);
        let ttl = self.ttl(vendor_id, model_id);
        // computed：闭包被执行 = 本次计算（leader / 自愈回源）→ X-Cache: MISS
        let computed = Arc::new(AtomicBool::new(false));
        let final_key_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let route_cell: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
        let err_cell: Arc<Mutex<Option<NexusFlowError>>> = Arc::new(Mutex::new(None));

        // 闭包经共享引用伴生（&F 为 Copy）：自愈回源路径还要再调一次
        let dispatch_ref = &dispatch;
        let leader_fut = {
            let computed = Arc::clone(&computed);
            let final_key_cell = Arc::clone(&final_key_cell);
            let route_cell = Arc::clone(&route_cell);
            let err_cell = Arc::clone(&err_cell);
            let api_key_hash = api_key_hash.clone();
            let canonical = canonical.clone();
            async move {
                computed.store(true, Ordering::SeqCst);
                match tokio::time::timeout(DISPATCH_TIMEOUT, dispatch_ref()).await {
                    Ok(Ok((value, route))) => {
                        let final_key = self
                            .write_final_key(&api_key_hash, &canonical, &route, &value)
                            .await;
                        *final_key_cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(final_key);
                        *route_cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(route);
                        Ok(value)
                    }
                    Ok(Err(e)) => {
                        // 派发错误经 cell 伴生传递：原始 NexusFlowError 由
                        // 外层原样取回（不丢错误类型），库错误仅作占位标记
                        *err_cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                        Err(OxCacheError::Operation(LEADER_DISPATCH_FAILED.to_string()))
                    }
                    Err(_elapsed) => {
                        Err(OxCacheError::Operation(LEADER_DISPATCH_TIMEOUT.to_string()))
                    }
                }
            }
        };

        // 完整初始候选键 + TTL 去重；闭包内换路后以最终键回写（库侧对
        // 返回值的初始键写入由返回后的 delete 清理——见 match 臂）
        let outcome = self
            .cache
            .get_or_with_ttl(&initial_key, Some(ttl), || leader_fut)
            .await;

        match outcome {
            Ok(value) => {
                let final_key = final_key_cell
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(final_key) = final_key {
                    if final_key != initial_key {
                        let _ = self.cache.delete(&initial_key).await;
                    }
                }
                Ok(CacheFlow {
                    value,
                    final_route: route_cell.lock().unwrap_or_else(|e| e.into_inner()).take(),
                    computed: computed.load(Ordering::SeqCst),
                    participated: true,
                })
            }
            Err(e) => {
                // leader 自身错误：原始 NexusFlowError 原样传播
                if let Some(original) = err_cell.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    return Err(original);
                }
                // leader 超时：504 语义（派发被 DISPATCH_TIMEOUT 钳制）
                if Self::is_dispatch_timeout(&e) {
                    return Err(NexusFlowError::Gate(GateError::Timeout {
                        vendor: vendor_id.to_string(),
                    }));
                }
                // 差异 B（leader-not-cached 标记）· 其余库侧错误（如损坏
                // 值反序列化失败）：自愈回源一次——缓存故障不得影响请求
                // 成功率（模块不变量），错误不透传客户端
                if Self::is_leader_not_cached(&e) {
                    tracing::debug!(
                        vendor = %vendor_id,
                        "response cache: leader-not-cached, follower self-heals from upstream"
                    );
                } else {
                    tracing::debug!(
                        vendor = %vendor_id,
                        error = %e,
                        "response cache: library fault, dispatching directly"
                    );
                }
                self.redispatch_and_write(dispatch_ref, &api_key_hash, &canonical, &initial_key)
                    .await
            }
        }
    }

    /// 自愈回源（差异 B / 库侧故障降级）：直接派发一次，成功后以最终
    /// 路由维度写完整键并清理初始键——恢复手写版「窃取回源」语义。
    async fn redispatch_and_write<R, F, Fut>(
        &self,
        dispatch: &F,
        api_key_hash: &str,
        canonical: &str,
        initial_key: &str,
    ) -> Result<CacheFlow<R>, NexusFlowError>
    where
        R: FinalRoute,
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<(Value, R), NexusFlowError>>,
    {
        let (value, route) = dispatch().await?;
        let final_key = self
            .write_final_key(api_key_hash, canonical, &route, &value)
            .await;
        if final_key != initial_key {
            let stale_key: String = initial_key.to_owned();
            let _ = self.cache.delete(&stale_key).await;
        }
        Ok(CacheFlow {
            value,
            final_route: Some(route),
            computed: true,
            participated: true,
        })
    }

    /// 以最终路由维度写完整键（major#2）并返回该键。TTL 与 [`Self::store`]
    /// 同口径（Null 短窗口；非 Null 三段式 + 配置抖动）；存取失败静默
    /// 降级——缓存故障不得影响请求成功率。
    async fn write_final_key<R: FinalRoute>(
        &self,
        api_key_hash: &str,
        canonical: &str,
        route: &R,
        value: &Value,
    ) -> String {
        let final_key =
            response_cache_key(api_key_hash, route.vendor_id(), route.model_id(), canonical);
        let ttl = if value.is_null() {
            Duration::from_secs(self.config.read().cache.null_cache_ttl_secs.max(1))
        } else {
            let factor = self.config.read().cache.ttl_jitter;
            let uniform = rand::random::<f64>() * 2.0 - 1.0;
            Self::apply_ttl_jitter(
                self.ttl(route.vendor_id(), route.model_id()),
                factor,
                uniform,
            )
        };
        let _ = self.cache.set_with_ttl(&final_key, value, Some(ttl)).await;
        final_key
    }

    /// 库侧 follower 标记（get_or leader 未回写）：`OxCacheError` 公开
    /// 变体 `L1Error` + 消息常量 `MSG_DETAIL_GET_OR_LEADER_NOT_CACHED`
    /// 的当前 locale 渲染。以库公开面断言——库升级改文案/常量时由
    /// `leader_not_cached_marker_matches_library_constant` 测试暴露漂移。
    fn is_leader_not_cached(err: &OxCacheError) -> bool {
        matches!(
            err,
            OxCacheError::L1Error(msg)
                if msg == &oxcache::i18n::messages::t(
                    oxcache::i18n::messages::MSG_DETAIL_GET_OR_LEADER_NOT_CACHED,
                    &[]
                )
        )
    }

    /// 闭包内派发超时的库错误携带标记（`LEADER_DISPATCH_TIMEOUT`）。
    fn is_dispatch_timeout(err: &OxCacheError) -> bool {
        matches!(
            err,
            OxCacheError::Operation(msg) if msg == LEADER_DISPATCH_TIMEOUT
        )
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
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
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
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
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
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
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
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
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
    //! TTL 负载联动 + 类别差异化（ttl() 三段式）。

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
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache,
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
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

#[cfg(test)]
mod single_flight_and_sentinel_tests {
    //! 并发防击穿（single-flight）+ TTL 抖动 + 空哨兵负缓存。

    use super::*;
    use crate::config::Config;

    fn make_cache(cache: CacheSection) -> ResponseCache {
        let config = Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache,
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
        };
        ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            Arc::new(crate::config::ConfigManager::new(config)),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        )
    }

    fn enabled_cache() -> ResponseCache {
        make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        })
    }

    fn test_body() -> Value {
        serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]})
    }

    /// 抖动纯函数：关闭时精确；开启时对称界内；非法输入原样。
    #[test]
    fn ttl_jitter_shapes_and_bounds() {
        let base = Duration::from_secs(300);
        assert_eq!(ResponseCache::apply_ttl_jitter(base, 0.0, 1.0), base);
        assert_eq!(ResponseCache::apply_ttl_jitter(base, -0.5, 1.0), base);
        assert_eq!(ResponseCache::apply_ttl_jitter(base, f64::NAN, 1.0), base);
        assert_eq!(ResponseCache::apply_ttl_jitter(base, 0.1, f64::NAN), base);
        assert_eq!(
            ResponseCache::apply_ttl_jitter(base, 0.1, 1.0),
            Duration::from_millis(330_000)
        );
        assert_eq!(
            ResponseCache::apply_ttl_jitter(base, 0.1, -1.0),
            Duration::from_millis(270_000)
        );
        assert_eq!(ResponseCache::apply_ttl_jitter(base, 0.1, 0.0), base);
        // floor 1ms
        assert_eq!(
            ResponseCache::apply_ttl_jitter(Duration::ZERO, 0.5, -1.0),
            Duration::from_millis(1)
        );
    }

    /// 空响应负缓存：窗口内 check 直接 Hit(Null) 不再穿透；后端存的是
    /// JSON null 值（经 `Cache::set_with_ttl` 序列化面落库）。
    /// （N=8 并发同 key 的 single-flight 语义已随去重层迁移至 oxcache
    /// `get_or_with_ttl`，见 `get_or_flow_tests` 的八并发测试。）
    #[tokio::test]
    async fn null_response_is_negative_cached() {
        let cache = enabled_cache();
        let body = test_body();
        let headers = HeaderMap::new();

        let CacheOutcome::Miss(parts) = cache.check(&headers, "v", "m", &body).await else {
            panic!("首次检查必须 Miss");
        };
        cache.store(&parts, "v", "m", &Value::Null).await;

        let outcome = cache.check(&headers, "v", "m", &body).await;
        assert!(
            matches!(outcome, CacheOutcome::Hit(Value::Null)),
            "哨兵窗口内必须 Hit(Null) 不再穿透，got {outcome:?}"
        );

        let key = response_cache_key(&parts.api_key_hash, "v", "m", &parts.canonical);
        // 负缓存载体 = JSON null 值（经 `Cache::set_with_ttl` 序列化面
        // 落库，`Cache::get` 解出即 Null）——旧手写哨兵字节随存储面
        // 切库下线，行为（窗口内 Hit(Null)）不变。
        let stored = cache
            .cache
            .get(&key)
            .await
            .expect("backend 读")
            .expect("负缓存必须落值");
        assert_eq!(stored, Value::Null);
    }

    /// 哨兵窗口过期后允许再次穿透（负缓存是短窗口，不是永久屏蔽）。
    #[tokio::test]
    async fn null_sentinel_expires_after_window() {
        let cache = make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            null_cache_ttl_secs: 1,
            ..Default::default()
        });
        let body = test_body();
        let headers = HeaderMap::new();

        let CacheOutcome::Miss(parts) = cache.check(&headers, "v", "m", &body).await else {
            panic!("首次检查必须 Miss");
        };
        cache.store(&parts, "v", "m", &Value::Null).await;
        assert!(
            matches!(
                cache.check(&headers, "v", "m", &body).await,
                CacheOutcome::Hit(Value::Null)
            ),
            "窗口内 Hit(Null)"
        );

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            matches!(
                cache.check(&headers, "v", "m", &body).await,
                CacheOutcome::Miss(_)
            ),
            "窗口过期后允许再次穿透"
        );
    }
}

#[cfg(test)]
mod get_or_flow_tests {
    //! get_or 闭包流（single-flight 由 oxcache `get_or_with_ttl` 承载）：
    //! 并发同键上游仅 1 次、leader 失败 follower 自愈回源、leader 超时
    //! followers 放行、库标记匹配。T002 行为钉住。

    use super::*;
    use crate::config::Config;
    use std::sync::atomic::AtomicUsize;

    fn make_cache(cache: CacheSection) -> ResponseCache {
        let config = Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache,
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
        };
        ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            Arc::new(crate::config::ConfigManager::new(config)),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        )
    }

    fn enabled_cache() -> ResponseCache {
        make_cache(CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        })
    }

    fn test_body() -> Value {
        serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]})
    }

    /// 测试用最终路由维度（消费侧实现 [`FinalRoute`]）
    #[derive(Debug, Clone)]
    struct TestRoute {
        vendor: String,
        model: String,
    }

    impl FinalRoute for TestRoute {
        fn vendor_id(&self) -> &str {
            &self.vendor
        }
        fn model_id(&self) -> &str {
            &self.model
        }
    }

    fn route(vendor: &str, model: &str) -> TestRoute {
        TestRoute {
            vendor: vendor.to_string(),
            model: model.to_string(),
        }
    }

    /// 差异 B/库标记：leader-not-cached 断言以库公开错误变体 + 消息常量
    /// 为准——库升级改文案/常量时本测试暴露漂移。
    #[test]
    fn leader_not_cached_marker_matches_library_constant() {
        let marked = OxCacheError::L1Error(oxcache::i18n::messages::t(
            oxcache::i18n::messages::MSG_DETAIL_GET_OR_LEADER_NOT_CACHED,
            &[],
        ));
        assert!(
            ResponseCache::is_leader_not_cached(&marked),
            "库标记必须命中（变体 L1Error + 消息常量渲染）"
        );
        // 异变体 / 异消息不得误判
        assert!(!ResponseCache::is_leader_not_cached(
            &OxCacheError::NotFound("k".into())
        ));
        assert!(!ResponseCache::is_leader_not_cached(
            &OxCacheError::L1Error("unrelated".into())
        ));
    }

    /// N=8 并发同 key：仅 leader 闭包执行（上游 1 次），followers 复用
    /// 回写值；computed 标志 leader=true（MISS）、follower=false（HIT）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn get_or_eight_concurrent_same_key_upstream_runs_once() {
        let cache = enabled_cache();
        let body = test_body();
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        // watch 同步（保有版本号，无 Notify 丢唤醒）：leader 进闭包 / 放行
        let (entered_tx, entered_rx) = tokio::sync::watch::channel(());
        let (release_tx, release_rx) = tokio::sync::watch::channel(());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let body = body.clone();
            let upstream_calls = Arc::clone(&upstream_calls);
            let entered_tx = entered_tx.clone();
            let release_rx = release_rx.clone();
            handles.push(tokio::spawn(async move {
                let dispatch = move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    let entered_tx = entered_tx.clone();
                    let mut release_rx = release_rx.clone();
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        let _ = entered_tx.send(());
                        // leader 在闭包内挂起，保证 followers 全部订阅后再放行
                        let _ = release_rx.changed().await;
                        Ok((serde_json::json!({"id": "r1"}), route("v-g1", "m")))
                    }
                };
                cache
                    .get_or_response(&HeaderMap::new(), "v-g1", "m", &body, dispatch)
                    .await
            }));
        }

        // leader 已进闭包；再等 200ms 保证 followers 全部 park 到注册表
        let mut entered_rx = entered_rx;
        let _ = tokio::time::timeout(Duration::from_secs(5), entered_rx.changed()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = release_tx.send(());

        let mut leader_count = 0;
        let mut follower_count = 0;
        for h in handles {
            let flow = tokio::time::timeout(Duration::from_secs(10), h)
                .await
                .expect("join 超时")
                .expect("任务 panic")
                .expect("get_or 不得 Err");
            assert_eq!(flow.value, serde_json::json!({"id": "r1"}));
            assert!(flow.participated, "8 并发同 key 均参与缓存");
            if flow.computed {
                leader_count += 1;
                assert!(flow.final_route.is_some(), "leader 携带最终路由");
            } else {
                follower_count += 1;
                assert!(
                    flow.final_route.is_none(),
                    "follower 复用不携带路由（调用方用初始决策收尾）"
                );
            }
        }
        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            1,
            "8 并发同 key 上游只调用 1 次"
        );
        assert_eq!(leader_count, 1, "恰一个 leader（X-Cache: MISS）");
        assert_eq!(follower_count, 7, "7 个 follower 复用（X-Cache: HIT）");
    }

    /// 差异 B 缓解：leader 派发失败（上游错误不写缓存）→ followers 得
    /// L1Error leader-not-cached 标记，自愈回源一次成功（恢复手写版
    /// 窃取语义，错误不透传客户端）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn get_or_leader_failure_follower_self_heals_from_upstream() {
        let cache = enabled_cache();
        let body = test_body();
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        // watch 同步（保有版本号，无 Notify 丢唤醒）
        let (entered_tx, entered_rx) = tokio::sync::watch::channel(());
        let (release_tx, release_rx) = tokio::sync::watch::channel(());

        // leader：闭包返回 Err（上游失败）
        let leader_cache = cache.clone();
        let leader_body = body.clone();
        let leader: tokio::task::JoinHandle<Result<CacheFlow<TestRoute>, NexusFlowError>> = {
            let upstream_calls = Arc::clone(&upstream_calls);
            let entered_tx = entered_tx.clone();
            let release_rx = release_rx.clone();
            tokio::spawn(async move {
                let dispatch = move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    let entered_tx = entered_tx.clone();
                    let mut release_rx = release_rx.clone();
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        let _ = entered_tx.send(());
                        let _ = release_rx.changed().await;
                        Err(NexusFlowError::Gate(GateError::VendorUnavailable {
                            vendor: "v".into(),
                        }))
                    }
                };
                leader_cache
                    .get_or_response(&HeaderMap::new(), "v-g2", "m", &leader_body, dispatch)
                    .await
            })
        };

        // 等 leader 进闭包后再起 follower——注册表条目已存在，必为 follower
        let mut entered_rx = entered_rx;
        let _ = tokio::time::timeout(Duration::from_secs(5), entered_rx.changed()).await;
        let follower_cache = cache.clone();
        let follower = {
            let upstream_calls = Arc::clone(&upstream_calls);
            tokio::spawn(async move {
                let dispatch = move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        Ok((serde_json::json!({"id": "healed"}), route("v-g2", "m")))
                    }
                };
                follower_cache
                    .get_or_response(&HeaderMap::new(), "v-g2", "m", &body, dispatch)
                    .await
            })
        };

        let _ = release_tx.send(());

        let leader_result = tokio::time::timeout(Duration::from_secs(10), leader)
            .await
            .expect("leader join 超时")
            .expect("leader 任务 panic");
        assert!(
            matches!(
                leader_result,
                Err(NexusFlowError::Gate(GateError::VendorUnavailable { .. }))
            ),
            "leader 自身派发错误原样传播，got {leader_result:?}"
        );

        let follower_flow = tokio::time::timeout(Duration::from_secs(10), follower)
            .await
            .expect("follower join 超时")
            .expect("follower 任务 panic")
            .expect("follower 自愈回源必须成功（不永久挂起、不透传库错误）");
        assert_eq!(follower_flow.value, serde_json::json!({"id": "healed"}));
        assert!(
            follower_flow.computed,
            "自愈回源 = 本次计算（X-Cache: MISS，同手写版窃取者语义）"
        );
        assert!(follower_flow.participated);
        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            2,
            "leader 失败 1 次 + follower 自愈 1 次"
        );
    }

    /// 差异 A 缓解：闭包内 timeout 钳制派发——leader 超时（sleep 31s >
    /// DISPATCH_TIMEOUT 30s，paused clock 虚拟推进）→ AsyncSfGuard 放行
    /// followers；leader 得 504 超时错误，follower 自愈回源成功。
    #[tokio::test(start_paused = true)]
    async fn get_or_leader_timeout_releases_followers_without_hang() {
        let cache = enabled_cache();
        let body = test_body();
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        // watch 同步（有版本号、无定时器依赖，paused clock 下不受
        // auto-advance 抢跑影响）
        let (entered_tx, entered_rx) = tokio::sync::watch::channel(());

        let leader_cache = cache.clone();
        let leader_body = body.clone();
        let leader: tokio::task::JoinHandle<Result<CacheFlow<TestRoute>, NexusFlowError>> = {
            let upstream_calls = Arc::clone(&upstream_calls);
            let entered_tx = entered_tx.clone();
            tokio::spawn(async move {
                let dispatch = move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    let entered_tx = entered_tx.clone();
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        let _ = entered_tx.send(());
                        // 超过 DISPATCH_TIMEOUT(30s)：paused clock 下虚拟推进即触发
                        tokio::time::sleep(Duration::from_secs(31)).await;
                        Ok((serde_json::json!({"id": "late"}), route("v-g3", "m")))
                    }
                };
                leader_cache
                    .get_or_response(&HeaderMap::new(), "v-g3", "m", &leader_body, dispatch)
                    .await
            })
        };

        // 等 leader 进闭包后再起 follower——注册表条目已存在，必为 follower
        let mut entered_rx = entered_rx;
        let _ = entered_rx.changed().await;
        let follower_cache = cache.clone();
        let follower = {
            let upstream_calls = Arc::clone(&upstream_calls);
            tokio::spawn(async move {
                let dispatch = move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        Ok((serde_json::json!({"id": "healed"}), route("v-g3", "m")))
                    }
                };
                follower_cache
                    .get_or_response(&HeaderMap::new(), "v-g3", "m", &body, dispatch)
                    .await
            })
        };

        let leader_result = tokio::time::timeout(Duration::from_secs(120), leader)
            .await
            .expect("leader join 超时")
            .expect("leader 任务 panic");
        assert!(
            matches!(
                leader_result,
                Err(NexusFlowError::Gate(GateError::Timeout { .. }))
            ),
            "leader 派发超时 → 504 语义错误，got {leader_result:?}"
        );

        let follower_flow = tokio::time::timeout(Duration::from_secs(120), follower)
            .await
            .expect("follower join 超时")
            .expect("follower 任务 panic")
            .expect("follower 被放行后自愈回源必须成功");
        assert_eq!(follower_flow.value, serde_json::json!({"id": "healed"}));
        assert!(follower_flow.computed);
        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            2,
            "leader 超时 1 次 + follower 自愈 1 次"
        );
    }
}
