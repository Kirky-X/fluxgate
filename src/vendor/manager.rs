// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{
    AdvancedRateLimiter, ConcurrencyLimiter, ConcurrencyPermit, PendingRequest, TokenBucket,
    VendorQueue, VendorState, VendorStatus,
};
use crate::config::{ConfigManager, RetrySection, VendorConfig};
use crate::error::GateError;
use crate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
use crate::protocol::{StreamChunk, UnifiedRequest, UnifiedResponse};
use crate::utils::metrics::GlobalMetrics;
use crate::utils::retry::{retry_delay_for, spread_delay, ErrorCategory, RetryDelayPolicy};
#[cfg(feature = "cost-tracking")]
use crate::utils::CostTracker;
use crate::vendor::key_pool::{KeyPool, KeyPoolInfo, PoolSnapshot, RequestCredential};
#[cfg(feature = "resilience")]
use crate::vendor::resilience::error_map::{from_circuit_call, GateErrorClassifier};
use bytes::Bytes;
#[cfg(feature = "resilience")]
use limiteron::circuit::{CircuitBreaker, CircuitBreakerConfig};
#[cfg(feature = "resilience")]
use limiteron::error::CircuitState;
use limiteron::retry::{RetryDecision, RetryPolicy};
use reqwest;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub max_tokens: Option<u64>,
    pub used_tokens: u64,
    pub window_end: Option<Instant>,
}

impl WindowInfo {
    /// 剩余时间 < 5 分钟。
    pub fn is_ending_soon(&self) -> bool {
        if let Some(end) = self.window_end {
            let remaining = end.duration_since(Instant::now());
            remaining.as_secs() < 300
        } else {
            false
        }
    }

    pub fn has_budget(&self) -> bool {
        if let Some(max) = self.max_tokens {
            self.used_tokens < max
        } else {
            true
        }
    }

    /// 预算百分比（0.0 - 1.0）。
    pub fn budget_percentage(&self) -> f64 {
        if let Some(max) = self.max_tokens {
            if max == 0 {
                return 0.0;
            }
            (1.0 - (self.used_tokens as f64 / max as f64)).max(0.0)
        } else {
            1.0
        }
    }
}

/// token 加权票据策略 — VendorConfig.concurrency 的 token_ticket_* 三元（构造期定死，restart-required）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TicketPolicy {
    pub min: u64,
    pub max: u64,
    pub default: u64,
}

/// token 加权票据成本估算（Unified 形态）：cost = clamp(messages 序列化字节/4 +
/// max_tokens, min, max)；字节/4 对中文/多模态偏差大（约 1.5-3×），max_tokens 缺省用
/// policy.default 兜底。
pub fn estimate_cost_unified(req: &UnifiedRequest, policy: &TicketPolicy) -> usize {
    let bytes = serde_json::to_string(&req.messages)
        .map(|s| s.len())
        .unwrap_or(0);
    let max_tokens = req.max_tokens.map(|v| v as u64).unwrap_or(policy.default);
    ((bytes / 4) as u64 + max_tokens).clamp(policy.min, policy.max) as usize
}

/// token 加权票据成本估算（JSON 形态）：序列化字节/4 + max_tokens 字段（口径与兜底同上）。
pub fn estimate_cost_json(body: &Value, policy: &TicketPolicy) -> usize {
    let bytes = serde_json::to_string(body).map(|s| s.len()).unwrap_or(0);
    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(policy.default);
    ((bytes / 4) as u64 + max_tokens).clamp(policy.min, policy.max) as usize
}

struct VendorInstance {
    vendor_id: String,
    state: VendorState,
    queue: Arc<VendorQueue>,
    rate_limiter: TokenBucket,
    concurrency_limiter: ConcurrencyLimiter,
    /// token 加权票据策略；None = 未开启（单票据）。
    ticket: Option<TicketPolicy>,
    advanced_limiter: AdvancedRateLimiter,
    /// key 池；池空回退 legacy 单 key 行为。
    key_pool: KeyPool,
    /// vendor 级熔断器 — legacy 回退（池空）请求的执行熔断器；池命中改用 per-key 指纹
    /// 熔断器。错误类型即 [`GateError`]（`CircuitCallError::Inner` 无损透传）。
    #[cfg(feature = "resilience")]
    circuit_breaker: Arc<CircuitBreaker<GateError>>,
    /// per-key 指纹熔断器注册表（键三元 = (model, endpoint, key 指纹)，vendor_id 由实例
    /// 域隐含）——同 key 同 model 跨请求共享计数，不同 key / 不同 model 互相隔离；条目
    /// 上界 = 活跃指纹数 × 活跃 model 数，字节级有界。legacy 回退不经此表。
    #[cfg(feature = "resilience")]
    per_key_circuit_breakers: PerKeyCircuitBreakers,
    /// per-key 熔断器构造参数（取自 [resilience] 段快照，构造期定死，restart-required）。
    #[cfg(feature = "resilience")]
    circuit_open_timeout: Duration,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
}

/// 生产熔断器配置的单一构造点（tests 与 manager 测试同源引用）。
///
/// 慢调用阈值显式抬至 60s / rate 1.0（均为 ≥ 比较），避免 limiteron 默认 500ms/0.5
/// 让常态超 500ms 的 LLM 成功调用误触发熔断。
#[cfg(feature = "resilience")]
pub(crate) fn gate_circuit_config(
    open_timeout: std::time::Duration,
) -> CircuitBreakerConfig<GateError> {
    let mut config = CircuitBreakerConfig::with_error_classifier(Arc::new(GateErrorClassifier))
        .slow_call_duration_threshold(Duration::from_secs(60))
        .slow_call_rate_threshold(1.0);
    config.failure_threshold = 5;
    config.success_threshold = 3;
    config.timeout = open_timeout;
    config
}

/// 单次 HTTP 尝试的失败信号 — 信号在此产生，业务决策在 decide 闭包，
/// 出口映射保留各自错误语义。
#[derive(Debug)]
enum AttemptFailure {
    /// send 阶段网络错误；`transient` = 超时/连接类（RetryableShort 候选）
    Network { transient: bool },
    /// 上游非 2xx：状态码 + `Retry-After` 头 + 已成形错误（i18n 语义串）
    Http {
        status: u16,
        retry_after: Option<Duration>,
        error: GateError,
    },
    /// 终结失败（构建 / 解析 / 协议转换错误），原样透传不重试
    Fatal(GateError),
}

/// per-key 指纹熔断器表（键三元 = (model, endpoint, key 指纹)）。
#[cfg(feature = "resilience")]
type PerKeyCircuitBreakers = std::sync::Mutex<
    std::collections::HashMap<(String, String, String), Arc<CircuitBreaker<GateError>>>,
>;

impl VendorInstance {
    fn new(
        config: &VendorConfig,
        queue_size: usize,
        rps: u32,
        #[cfg(feature = "resilience")] circuit_open_timeout: std::time::Duration,
        #[cfg(feature = "cost-tracking")] cost_tracker: Arc<CostTracker>,
    ) -> Self {
        Self {
            vendor_id: config.id.clone(),
            advanced_limiter: AdvancedRateLimiter::new(&config.rate_limit),
            state: VendorState::new(),
            queue: Arc::new(VendorQueue::new(queue_size)),
            rate_limiter: TokenBucket::new(queue_size, rps),
            // 并发上限可配置（缺省沿用队列容量）；permit 持有至流结束，对流式生效
            concurrency_limiter: ConcurrencyLimiter::new(
                config
                    .concurrency
                    .max_concurrent_requests
                    .unwrap_or(queue_size),
            ),
            ticket: if config.concurrency.token_weighted {
                Some(TicketPolicy {
                    min: config.concurrency.token_ticket_min.max(1),
                    max: config.concurrency.token_ticket_max.max(1),
                    default: config.concurrency.token_ticket_default.max(1),
                })
            } else {
                None
            },
            #[cfg(feature = "resilience")]
            circuit_breaker: Arc::new(CircuitBreaker::new(
                // limiteron 三态机（half_open_max_calls=3，探针计数每次状态转换清零），
                // 无「熔断一次即永久拒绝」死锁；Open 窗口经 [resilience].circuit_open_timeout_ms 可配。
                gate_circuit_config(circuit_open_timeout),
            )),
            // key 池构造期解析一次（缺 env 逐名显性 warn）
            key_pool: KeyPool::new(config, &config.id),
            #[cfg(feature = "resilience")]
            per_key_circuit_breakers: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "resilience")]
            circuit_open_timeout,
            #[cfg(feature = "cost-tracking")]
            cost_tracker,
        }
    }

    /// 请求级凭证选择（池命中 → 当前 key + 指纹；池空 → legacy）。
    fn select_request_key(&self, config: &VendorConfig) -> RequestCredential {
        self.key_pool.select(config, &self.vendor_id)
    }

    /// 编程式内联 key 池热注入（见 [`KeyPool::set_inline`]）。
    fn set_inline_key_pool(&self, snapshot: PoolSnapshot) {
        self.key_pool.set_inline(snapshot);
    }

    /// key 错误处置三态：委托 [`KeyPool::note_request_error`]（分类门控内聚于池）。
    fn handle_key_error(
        &self,
        credential: &RequestCredential,
        status: reqwest::StatusCode,
        body: &str,
    ) {
        self.key_pool.note_request_error(
            &self.vendor_id,
            credential.fingerprint(),
            status.as_u16(),
            body,
        );
    }

    /// 管理面重置：清空 key 池黑名单（含游标）与冷却状态。
    fn reset_resilience_state(&self) {
        self.key_pool.reset_key_state();
        self.state.mark_available();
    }

    /// 探针恢复（minimal_call 成功）：解黑名单 + 清冷却 + 重置该指纹名下全部模型通道熔断器。
    async fn probe_recover_key(&self, fingerprint: &str) {
        #[cfg(feature = "resilience")]
        self.key_pool.remove_blacklisted(fingerprint);
        #[cfg(not(feature = "resilience"))]
        let _ = fingerprint;
        self.state.mark_available();
        #[cfg(feature = "resilience")]
        for cb in self.peek_circuit_breakers_for_fingerprint(Some(fingerprint)) {
            cb.reset().await;
        }
    }

    /// 本次凭证应使用的熔断器 — 池命中按 (model, endpoint, key 指纹) 取（同 key 同 model 共享计数），legacy 回退用 vendor 级。
    #[cfg(feature = "resilience")]
    fn circuit_breaker_for(
        &self,
        credential: &RequestCredential,
        model: &str,
        endpoint: &str,
    ) -> Arc<CircuitBreaker<GateError>> {
        self.circuit_breaker_for_fp(credential.fingerprint(), model, endpoint)
    }

    /// [`Self::circuit_breaker_for`] 的指纹直入版（`None` = legacy 回退）；表键含客户端
    /// 可控的 model/endpoint，无界增长即内存 DoS——达上限后新维度退化到 vendor 级熔断器。
    #[cfg(feature = "resilience")]
    fn circuit_breaker_for_fp(
        &self,
        fingerprint: Option<&str>,
        model: &str,
        endpoint: &str,
    ) -> Arc<CircuitBreaker<GateError>> {
        const MAX_PER_KEY_CIRCUIT_BREAKERS: usize = 2048;
        match fingerprint {
            Some(fp) => {
                // 锁毒化不 panic，取锁内值继续（毒化源于别处 bug，不应放大为崩溃）。
                let mut guard = self
                    .per_key_circuit_breakers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let key = (model.to_string(), endpoint.to_string(), fp.to_string());
                if let Some(cb) = guard.get(&key) {
                    return cb.clone();
                }
                if guard.len() >= MAX_PER_KEY_CIRCUIT_BREAKERS {
                    return self.circuit_breaker.clone();
                }
                let cb = Arc::new(CircuitBreaker::new(gate_circuit_config(
                    self.circuit_open_timeout,
                )));
                guard.insert(key, cb.clone());
                cb
            }
            None => self.circuit_breaker.clone(),
        }
    }

    /// 只读指定指纹的全部 model 通道熔断器（不创建）——路由观测与观测 API 的聚合输入；
    /// `None`（legacy 回退）返回 vendor 级单元素，表空返回空 Vec。
    #[cfg(feature = "resilience")]
    fn peek_circuit_breakers_for_fingerprint(
        &self,
        fingerprint: Option<&str>,
    ) -> Vec<Arc<CircuitBreaker<GateError>>> {
        match fingerprint {
            Some(fp) => {
                let guard = self
                    .per_key_circuit_breakers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard
                    .iter()
                    .filter(|((_, _, entry_fp), _)| entry_fp == fp)
                    .map(|(_, cb)| cb.clone())
                    .collect()
            }
            None => vec![self.circuit_breaker.clone()],
        }
    }

    /// admin 观测（仅 env 名 / 数量 / 下标 / 指纹，绝无 key 原文）。
    fn key_pool_info(&self) -> KeyPoolInfo {
        self.key_pool.info()
    }
}

#[derive(Clone)]
/// 健康探针目标快照：一次周期内只读采集。`keys` 为 key 明文（探针 HTTP 构造专用，
/// 绝不记日志）；`current` 为当前轮换游标所指（free 探测的出站凭证）。
pub(crate) struct ProbeTarget {
    pub vendor_id: String,
    pub config: VendorConfig,
    pub status: VendorStatus,
    pub keys: Vec<(String, String)>,
    pub blacklist: Vec<String>,
    pub current: Option<(String, String)>,
}

pub struct VendorManager {
    config: Arc<ConfigManager>,
    instances: Arc<RwLock<std::collections::HashMap<String, Arc<VendorInstance>>>>,
    /// 编程式内联 key 池 — vendor_id → 快照（见 [`PoolSnapshot`]）。实例创建时消费生效，
    /// 实例已存在时直接热注入；config 面仍只存 env 名。
    inline_key_pools: Arc<std::sync::Mutex<std::collections::HashMap<String, PoolSnapshot>>>,
    client: reqwest::Client,
    openai_converter: Arc<OpenAIConverter>,
    anthropic_converter: Arc<AnthropicConverter>,
    metrics: Arc<GlobalMetrics>,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
}

/// TCP 连接建立超时 — Client 级共享常量（对端不响应时连接阶段不无限挂起）。
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `execute_request` 外层的排队等待预算 — 覆盖「入队 → worker 取走 → 限流放行」段；
/// 执行段由 `VendorConfig.timeout_ms`（per-attempt，从 send 起算）承担，两者解耦。
const QUEUE_WAIT_BUDGET: Duration = Duration::from_secs(60);

impl VendorManager {
    pub fn new(config: Arc<ConfigManager>) -> Self {
        Self {
            config,
            instances: Arc::new(RwLock::new(std::collections::HashMap::new())),
            inline_key_pools: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            // 连接阶段 10s 超时
            client: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("reqwest Client build failed"),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            metrics: Arc::new(GlobalMetrics::new()),
            #[cfg(feature = "cost-tracking")]
            cost_tracker: Arc::new(CostTracker::new()),
        }
    }

    /// 热读厂商配置 — 执行路径统一入口；vendor 已移除时返回 Err（以 VendorUnavailable 拒绝）。
    fn fresh_config(&self, vendor_id: &str) -> Result<VendorConfig, GateError> {
        self.config
            .get_vendor(vendor_id)
            .ok_or_else(|| GateError::VendorUnavailable {
                vendor: crate::i18n::t_args(
                    "vendor-removed-from-config",
                    &[("vendor", vendor_id.to_string())],
                ),
            })
    }

    /// 全局指标访问器 — 用于 /admin/metrics 端点
    pub fn metrics(&self) -> &Arc<GlobalMetrics> {
        &self.metrics
    }

    /// 注入编程式内联 key 池（见 [`PoolSnapshot`]）：实例未创建时登记到 side-table，
    /// 建实例时消费生效；实例已存在时立即热注入（重复注入以最后一次为准）。
    /// 生命周期由调用方管理——env 变更检测/重解析对内联池不适用。
    pub fn set_inline_key_pool(&self, vendor_id: &str, snapshot: PoolSnapshot) {
        self.inline_key_pools
            .lock()
            .unwrap()
            .insert(vendor_id.to_string(), snapshot.clone());
        if let Some(instance) = self.instances.read().unwrap().get(vendor_id) {
            instance.set_inline_key_pool(snapshot);
        }
    }

    /// `[routing].ewma_tau_ms` 热同步到 metrics（每次延迟道落账前刷新）。
    fn sync_ewma_tau(&self) {
        self.metrics
            .set_ewma_tau_ms(self.config.read().routing.ewma_tau_ms);
    }

    /// 全局成本追踪器访问器（仅 `cost-tracking` feature）。
    #[cfg(feature = "cost-tracking")]
    pub fn cost_tracker(&self) -> &Arc<CostTracker> {
        &self.cost_tracker
    }

    /// 读取 vendor 的 CircuitBreaker 聚合状态（运维观测/测试断言）；未启用 resilience 或
    /// vendor 未初始化返回 `None`。全部 model 通道 Open 才报 Open，任一 HalfOpen 报 HalfOpen。
    #[cfg(feature = "resilience")]
    pub async fn get_circuit_state(&self, vendor_id: &str) -> Option<CircuitState> {
        let instance = self.instance_for(vendor_id)?;
        let fingerprint = instance.key_pool.peek_current_fingerprint();
        let breakers = instance.peek_circuit_breakers_for_fingerprint(fingerprint.as_deref());
        if breakers.is_empty() {
            return Some(instance.circuit_breaker.get_state().await);
        }
        Some(Self::aggregate_circuit_state(&breakers).await)
    }

    /// 按 key 指纹直读聚合熔断状态（per-key 隔离的观测入口）。指定指纹尚无 model
    /// 通道请求时返回 `None`（只读 peek，不创建）。
    #[cfg(feature = "resilience")]
    pub async fn get_circuit_state_for_fingerprint(
        &self,
        vendor_id: &str,
        fingerprint: &str,
    ) -> Option<CircuitState> {
        let instance = self.instance_for(vendor_id)?;
        let breakers = instance.peek_circuit_breakers_for_fingerprint(Some(fingerprint));
        if breakers.is_empty() {
            return None;
        }
        Some(Self::aggregate_circuit_state(&breakers).await)
    }

    /// 多通道聚合判定：全 Open → Open；任一 HalfOpen → HalfOpen；其余 → Closed（空集由调用方守卫）。
    #[cfg(feature = "resilience")]
    async fn aggregate_circuit_state(breakers: &[Arc<CircuitBreaker<GateError>>]) -> CircuitState {
        let mut all_open = true;
        let mut any_half_open = false;
        for cb in breakers {
            match cb.get_state().await {
                CircuitState::Open => {}
                CircuitState::HalfOpen => {
                    any_half_open = true;
                    all_open = false;
                }
                CircuitState::Closed => all_open = false,
            }
        }
        if all_open {
            CircuitState::Open
        } else if any_half_open {
            CircuitState::HalfOpen
        } else {
            CircuitState::Closed
        }
    }

    /// 实例只读句柄（观测路径共用）。
    #[cfg(feature = "resilience")]
    fn instance_for(&self, vendor_id: &str) -> Option<Arc<VendorInstance>> {
        let guard = self.instances.read().unwrap();
        guard.get(vendor_id).cloned()
    }

    /// per-model 能力查询（路由决策/响应面可取）：已登记模型返回目录真实值
    /// （context_window 非 0）；未登记回落 per-flavor 保守默认。热读 ConfigManager。
    pub fn model_capabilities(
        &self,
        vendor_type: &str,
        model_id: &str,
    ) -> crate::config::ModelCapabilities {
        crate::config::model_catalog::capabilities_for(
            &self.config.read().model_catalog,
            vendor_type,
            model_id,
        )
    }

    /// [`Self::model_capabilities`] 的 vendor_id 便捷版；未知 vendor 返回 `None`。
    pub fn model_capabilities_for_vendor(
        &self,
        vendor_id: &str,
        model_id: &str,
    ) -> Option<crate::config::ModelCapabilities> {
        let vendor_type = self.config.get_vendor(vendor_id)?.type_;
        Some(self.model_capabilities(&vendor_type, model_id))
    }

    /// admin 观测 — vendor 的 key 池快照（绝无 key 原文）；实例未初始化返回 `None`。
    pub fn key_pool_info(&self, vendor_id: &str) -> Option<KeyPoolInfo> {
        if let Some(instance) = self.instances.read().unwrap().get(vendor_id) {
            return Some(instance.key_pool_info());
        }
        // 实例未创建：走内联 side-table 观测（env 模式无预创建快照 → None）。
        self.inline_key_pools
            .lock()
            .unwrap()
            .get(vendor_id)
            .map(|snapshot| KeyPoolInfo {
                envs: Vec::new(),
                size: snapshot.len(),
                current_index: 0,
                fingerprints: snapshot
                    .keys()
                    .iter()
                    .map(|k| crate::vendor::key_pool::fingerprint_of(k))
                    .collect(),
            })
    }

    /// explain 判定面：该 vendor 的 key 池非空且全部指纹已入黑名单（全黑形态）；实例
    /// 未初始化或池空返回 false。返回黑名单指纹明细供 explain detail 呈现。
    pub fn key_pool_exhausted(&self, vendor_id: &str) -> Option<Vec<String>> {
        let guard = self.instances.read().ok()?;
        let instance = guard.get(vendor_id)?;
        if instance.key_pool.all_blacklisted() {
            Some(instance.key_pool.blacklisted_fingerprints())
        } else {
            None
        }
    }

    /// vendor 的 CircuitBreaker 句柄（未初始化时 `None`），仅供 cover_all_paths 取用；
    /// 按凭证 + model + endpoint 取，池命中用 per-key 指纹熔断器。
    #[cfg(feature = "resilience")]
    fn circuit_breaker_for(
        &self,
        vendor_id: &str,
        credential: &RequestCredential,
        model: &str,
        endpoint: &str,
    ) -> Option<Arc<CircuitBreaker<GateError>>> {
        let guard = self.instances.read().unwrap();
        guard
            .get(vendor_id)
            .map(|i| i.circuit_breaker_for(credential, model, endpoint))
    }

    pub async fn initialize(&self) -> Result<(), GateError> {
        let vendors = self.config.get_enabled_vendors();
        for vendor in &vendors {
            self.get_or_create_instance(vendor);
        }

        // 窗口翻转由 limiteron FixedWindowLimiter 惰性重置（下一次 allow/peek 时完成）。

        Ok(())
    }

    fn get_or_create_instance(&self, vendor: &VendorConfig) -> Arc<VendorInstance> {
        let mut guard = self.instances.write().unwrap();

        if let Some(instance) = guard.get(&vendor.id) {
            return instance.clone();
        }

        let queue_size = 100;
        let rps = vendor.concurrency.requests_per_second;

        let instance = Arc::new(VendorInstance::new(
            vendor,
            queue_size,
            rps,
            #[cfg(feature = "resilience")]
            std::time::Duration::from_millis(self.config.read().resilience.circuit_open_timeout_ms),
            #[cfg(feature = "cost-tracking")]
            self.cost_tracker.clone(),
        ));
        guard.insert(vendor.id.clone(), instance.clone());
        // 编程式内联 key 池：side-table 有登记则消费注入（创建期一次性生效）
        if let Some(snapshot) = self
            .inline_key_pools
            .lock()
            .unwrap()
            .get(&vendor.id)
            .cloned()
        {
            instance.set_inline_key_pool(snapshot);
        }

        let instance_clone = instance.clone();
        let client_clone = self.client.clone();
        let openai = self.openai_converter.clone();
        let anthropic = self.anthropic_converter.clone();
        let config_manager = self.config.clone();
        // metrics 传入 worker — 转换路径（经队列）在 worker 内记账
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            Self::process_vendor_queue(
                instance_clone,
                client_clone,
                openai,
                anthropic,
                config_manager,
                metrics,
            )
            .await;
        });

        instance
    }

    async fn process_vendor_queue(
        instance: Arc<VendorInstance>,
        client: reqwest::Client,
        openai: Arc<OpenAIConverter>,
        anthropic: Arc<AnthropicConverter>,
        config_manager: Arc<ConfigManager>,
        metrics: Arc<GlobalMetrics>,
    ) {
        loop {
            // 先取请求再等 RPS 令牌：避免空闲期空烧令牌与突发期最长 100ms 轮询等待。
            let pending = match instance.queue.recv().await {
                Some(pending) => pending,
                None => break,
            };

            instance.rate_limiter.acquire().await;

            // Advanced limiter 非消费 peek
            if !instance.advanced_limiter.check().await {
                // 路由状态不在此回写：Throttled 由 get_status 实时 peek 承担，回写持久状态
                // 会造成「路由排除 → 无请求 → 窗口重置后仍被排除」死锁。
                let _ = pending.response_tx.send(Err(GateError::RateLimited {
                    vendor: instance.vendor_id.clone(),
                }));
                continue;
            }

            // token_weighted 开启时按预估 token 量计费
            let permit_result = match instance.ticket {
                Some(policy) => {
                    let cost = estimate_cost_unified(&pending.request, &policy);
                    instance.concurrency_limiter.acquire_weighted(cost).await
                }
                None => instance.concurrency_limiter.acquire().await,
            };

            // 热读新鲜配置（热更新立即生效；vendor 已移除则以 VendorUnavailable 拒绝）
            let config = config_manager
                .get_vendor(&instance.vendor_id)
                .ok_or_else(|| GateError::VendorUnavailable {
                    vendor: crate::i18n::t_args(
                        "vendor-removed-from-config",
                        &[("vendor", instance.vendor_id.clone())],
                    ),
                });

            let config = match config {
                Ok(c) => c,
                Err(e) => {
                    let _ = pending.response_tx.send(Err(e));
                    continue;
                }
            };

            tokio::spawn(Self::process_dequeued(
                instance.clone(),
                client.clone(),
                openai.clone(),
                anthropic.clone(),
                config_manager.clone(),
                metrics.clone(),
                config,
                pending,
                permit_result,
            ));
        }
    }

    /// 单条出队请求的执行任务（[`Self::process_vendor_queue`] 的 spawn 体）：凭证选择 →
    /// 按 key 熔断器装配 → perform_http_request → 结局与配额记账 → 回传结果。
    #[allow(clippy::too_many_arguments)]
    async fn process_dequeued(
        instance: Arc<VendorInstance>,
        client: reqwest::Client,
        openai: Arc<OpenAIConverter>,
        anthropic: Arc<AnthropicConverter>,
        config_manager: Arc<ConfigManager>,
        metrics: Arc<GlobalMetrics>,
        config: VendorConfig,
        pending: PendingRequest,
        permit: ConcurrencyPermit,
    ) {
        let _permit = permit;

        // [retry] 段每请求热读，配置更新后下一次重试循环即用新参数
        let retry_cfg = config_manager.read().retry.clone();

        // 请求级凭证选择（池命中 → 当前 key；池空 → legacy）+ 按 (model, endpoint, 指纹)
        // 选熔断器。key 在请求启动时固定，本请求所有重试尝试复用同一 key。
        let credential = instance.select_request_key(&config);
        #[cfg(feature = "resilience")]
        let circuit_breaker = instance.circuit_breaker_for(
            &credential,
            Self::cb_model(&instance.vendor_id, &config, &pending.request.model),
            Self::cb_endpoint(&config),
        );

        // anthropic 转换出站的缓存断点策略（openai 型 None 不注入；透传路径不注入）
        let cache_policy = if config.type_ == "anthropic" {
            Some(crate::config::model_catalog::cache_policy_for(
                &config_manager.read().model_catalog,
                &pending.request.model,
            ))
        } else {
            None
        };

        // 拆出 request 局部绑定：perform 消费请求体后 pending 其余字段仍可整结构体借用
        let request = pending.request;
        let result = Self::perform_http_request(
            &client,
            &config,
            request,
            &openai,
            &anthropic,
            &retry_cfg,
            #[cfg(feature = "resilience")]
            &circuit_breaker,
            &metrics,
            &instance,
            &credential,
            cache_policy.as_ref(),
        )
        .await;

        Self::record_outcome(
            &instance,
            &metrics,
            &config_manager,
            pending.enqueued_at,
            pending.dequeued_at,
            &result,
        );

        // 成功路径：原子扣减 token 使用统计 + 累加成本。
        if let Ok(ref response) = result {
            Self::charge_success_usage(&instance, &config_manager, &config, response).await;
        }

        let _ = pending.response_tx.send(result);
    }

    /// 执行结局记账：成功清错误计数，失败累加；路由状态不在此回写——回写会让 router 的
    /// `==Available` 过滤永久排除该 vendor，而恢复通路都要求请求先到达（恶性循环）。
    ///
    /// metrics 同点记账：duration = enqueued_at 起算，落 e2e 道；排队段单列 queue 道。
    fn record_outcome(
        instance: &Arc<VendorInstance>,
        metrics: &Arc<GlobalMetrics>,
        config_manager: &Arc<ConfigManager>,
        enqueued_at: Instant,
        dequeued_at: Option<Instant>,
        result: &Result<UnifiedResponse, GateError>,
    ) {
        metrics.set_ewma_tau_ms(config_manager.read().routing.ewma_tau_ms);
        let duration = enqueued_at.elapsed();
        let lane_now = Instant::now();
        if let Some(dequeued_at) = dequeued_at {
            metrics.record_queue(
                &instance.vendor_id,
                dequeued_at.saturating_duration_since(enqueued_at),
                lane_now,
            );
        }
        metrics.record_e2e(&instance.vendor_id, duration, lane_now);
        match result {
            Ok(_) => {
                instance.state.mark_available();
                instance.state.reset_error();
                metrics.record_request(&instance.vendor_id, true, duration);
                // 失败率滑窗双记（纯观测，常开）
                metrics.record_vendor_outcome_at(&instance.vendor_id, true, Instant::now());
            }
            Err(e) => {
                instance.state.inc_error();
                metrics.record_request(&instance.vendor_id, false, duration);
                metrics.record_error(&instance.vendor_id, &e.to_string());
                // 失败率滑窗双记 — 失败（含 429）计入 failures；该计数独立于 CB 分类器。
                metrics.record_vendor_outcome_at(&instance.vendor_id, false, Instant::now());
                // 重试维度失败计数 — 仅转换路径（有重试循环）累加，透传/流式不计入。
                metrics.record_failed_request_before_retry();
            }
        }
    }

    /// 成功路径：原子扣减 token 使用统计（CAS 全有全无，剩余不足则拒绝记账不超卖）+
    /// 累加成本。仅 Token 变体记账（Interval/TimeWindow/None 无配额语义）。
    #[cfg_attr(not(feature = "cost-tracking"), allow(unused_variables))]
    async fn charge_success_usage(
        instance: &Arc<VendorInstance>,
        config_manager: &Arc<ConfigManager>,
        config: &VendorConfig,
        response: &UnifiedResponse,
    ) {
        if instance.advanced_limiter.accounts_tokens() {
            let tokens = response.usage.total_tokens as u64;
            let charged = instance.advanced_limiter.consume_tokens(tokens).await;
            if !charged {
                tracing::warn!(
                    vendor = %instance.vendor_id,
                    tokens,
                    "{}",
                    crate::i18n::t("vendor-token-quota-underflow")
                );
            }
        }

        // cost-tracking：按 (vendor, model) 从热配置 model_catalog 四档计价（含 cache 档，
        // 币种随条目；cache 语义按 vendor 协议：anthropic 不含 cache 档 / openai 含）；
        // 未登记模型不计价（防 0 价伪装免费）。
        #[cfg(feature = "cost-tracking")]
        {
            use crate::vendor::cost::{estimate_cost_from_catalog, prompt_cache_semantics};
            // cache 语义与分账/配额同一判定（经 prompt_cache_semantics 收敛）。
            let semantics = prompt_cache_semantics(&config.type_);
            let estimate = estimate_cost_from_catalog(
                &config_manager.read().model_catalog,
                &response.model,
                &response.usage,
                semantics,
            );
            match estimate {
                Some(est) => {
                    // CostTracker 单 USD 道：CNY 条目暂不落，warn 显性防报表少计。
                    if matches!(est.currency, crate::config::PricingCurrency::Usd) {
                        instance.cost_tracker.record(
                            &instance.vendor_id,
                            &response.model,
                            est.total_cost,
                        );
                    } else {
                        tracing::warn!(
                            vendor = %instance.vendor_id,
                            model = %response.model,
                            cost_cny = est.total_cost,
                            "CNY-priced entry not recorded into USD cost tracker (T011 bridge pending)"
                        );
                    }
                }
                None => {
                    // 未登记 model_catalog 不计价——成本报表少计需可观测
                    tracing::debug!(
                        vendor = %instance.vendor_id,
                        model = %response.model,
                        "{}",
                        crate::i18n::t("vendor-cost-unregistered-model")
                    );
                }
            }
        }
    }

    /// 参数超 clippy 阈值（转换器双向 + circuit_breaker/instance/credential/cache_policy 均必需），不打包重构。
    #[allow(clippy::too_many_arguments)]
    async fn perform_http_request(
        client: &reqwest::Client,
        config: &VendorConfig,
        request: UnifiedRequest,
        openai: &OpenAIConverter,
        anthropic: &AnthropicConverter,
        retry_cfg: &RetrySection,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker<GateError>>,
        metrics: &GlobalMetrics,
        instance: &VendorInstance,
        credential: &RequestCredential,
        cache_policy: Option<&crate::config::model_catalog::CacheBreakpointPolicy>,
    ) -> Result<UnifiedResponse, GateError> {
        let converter: &dyn ProtocolConverter = match config.type_.as_str() {
            "openai" => openai,
            "anthropic" => anthropic,
            _ => {
                return Err(GateError::Config {
                    message: crate::i18n::t("vendor-unknown-type"),
                })
            }
        };

        // anthropic 型走缓存断点注入入口（None = 非 anthropic，走原编码）
        let request_body = if let Some(policy) = cache_policy {
            anthropic.request_to_external_with_cache_policy(request, policy)?
        } else {
            converter.request_to_external(request)?
        };

        // 执行带重试的请求（resilience feature 启用时由 CircuitBreaker 包装）
        Self::execute_http_with_retry(
            client,
            config,
            request_body,
            converter,
            retry_cfg,
            #[cfg(feature = "resilience")]
            circuit_breaker,
            metrics,
            instance,
            credential,
        )
        .await
    }

    /// 解析上游 `Retry-After` 响应头：仅支持秒数形式（RFC 7231 delta-seconds），
    /// HTTP-date 形式降级 None。
    fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
        let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
        value.trim().parse::<u64>().ok().map(Duration::from_secs)
    }

    /// 构建 vendor API URL（path 可变版：chat/completions / messages / embeddings）；
    /// `pub(crate)` 供健康探针复用同一拼接规则。
    pub(crate) fn build_vendor_url_at(
        config: &VendorConfig,
        path: &str,
    ) -> Result<String, GateError> {
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => {
                    return Err(GateError::Config {
                        message: crate::i18n::t("vendor-unknown-type"),
                    })
                }
            }
        } else {
            &config.api_base
        };

        let url = if api_base.ends_with("/v1") {
            format!("{}/{}", api_base, path)
        } else {
            format!("{}/v1/{}", api_base, path)
        };

        Ok(url)
    }

    /// 构建 vendor API URL（chat/messages 默认端点）；`pub(crate)` 供健康探针复用。
    pub(crate) fn build_vendor_url(config: &VendorConfig) -> Result<String, GateError> {
        let path = if config.type_ == "anthropic" {
            "messages"
        } else {
            "chat/completions"
        };
        Self::build_vendor_url_at(config, path)
    }

    /// 获取 API key 并构建带认证 header 的 RequestBuilder（legacy 单 key：读 `api_key_env`，缺失报 Config 错误）。
    fn build_authed_request(
        client: &reqwest::Client,
        config: &VendorConfig,
        url: &str,
        include_content_type: bool,
    ) -> Result<reqwest::RequestBuilder, GateError> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| GateError::Config {
            message: crate::i18n::t_args(
                "vendor-api-key-env-not-set",
                &[("env", config.api_key_env.clone())],
            ),
        })?;
        Self::build_authed_request_with_key(client, config, url, include_content_type, &api_key)
    }

    /// 以指定 key 构建带认证 header 的 RequestBuilder（池命中路径）。
    ///
    /// 末尾统一注入 `config.extra_headers`；与网关已设置的头冲突时跳过并记 warn，不覆盖。
    fn build_authed_request_with_key(
        client: &reqwest::Client,
        config: &VendorConfig,
        url: &str,
        include_content_type: bool,
        api_key: &str,
    ) -> Result<reqwest::RequestBuilder, GateError> {
        let mut builder = client.post(url);

        if config.type_ == "anthropic" {
            builder = builder
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
            if include_content_type {
                builder = builder.header("content-type", "application/json");
            }
        } else {
            builder = builder.header("Authorization", format!("Bearer {}", api_key));
            if include_content_type {
                builder = builder.header("content-type", "application/json");
            }
        }

        Ok(Self::apply_extra_headers(builder, &config.extra_headers))
    }

    /// 网关已设置的出站头（小写）：`extra_headers` 同名时跳过注入并记 warn，不覆盖。
    const RESERVED_OUTBOUND_HEADERS: &'static [&'static str] = &[
        "authorization",
        "x-api-key",
        "content-length",
        "host",
        "content-type",
        "anthropic-version",
    ];

    /// `extra_headers` 统一注入（静态值，不做模板替换）；非法头名/值 warn 后跳过，键按字典序注入。
    fn apply_extra_headers(
        mut builder: reqwest::RequestBuilder,
        extra: &HashMap<String, String>,
    ) -> reqwest::RequestBuilder {
        if extra.is_empty() {
            return builder;
        }
        let mut keys: Vec<&String> = extra.keys().collect();
        keys.sort();
        for k in keys {
            let v = &extra[k];
            if Self::RESERVED_OUTBOUND_HEADERS.contains(&k.to_ascii_lowercase().as_str()) {
                tracing::warn!(
                    header = %k,
                    "{}",
                    crate::i18n::t("vendor-extra-header-conflict")
                );
                continue;
            }
            let name: reqwest::header::HeaderName = match k.parse() {
                Ok(n) => n,
                Err(_) => {
                    tracing::warn!(
                        header = %k,
                        "{}",
                        crate::i18n::t("vendor-extra-header-name-invalid")
                    );
                    continue;
                }
            };
            let value: reqwest::header::HeaderValue = match v.parse() {
                Ok(val) => val,
                Err(_) => {
                    tracing::warn!(
                        header = %k,
                        "{}",
                        crate::i18n::t("vendor-extra-header-value-invalid")
                    );
                    continue;
                }
            };
            builder = builder.header(name, value);
        }
        builder
    }

    /// per-key 熔断键的 endpoint 段 — 配置面 api_base 身份（空 = 默认端点）。
    /// 无 resilience 组合下不参与 CB 选路（仅签名统一）。
    #[cfg_attr(not(feature = "resilience"), allow(dead_code))]
    fn cb_endpoint(config: &VendorConfig) -> &str {
        if config.api_base.is_empty() {
            "<default>"
        } else {
            &config.api_base
        }
    }

    /// per-key 熔断键的 model 段 — 归一到配置面模型身份（vendor.models 的 model_id）：
    /// 先剥离 `{vendor_id}.` 前缀（hint 与 direct 形式须落同一键），再按 vendor.models 匹配。
    fn cb_model<'a>(vendor_id: &str, config: &'a VendorConfig, request_model: &'a str) -> &'a str {
        let stripped = request_model
            .strip_prefix(vendor_id)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(request_model);
        if stripped.is_empty() {
            return request_model;
        }
        config
            .models
            .iter()
            .find(|m| {
                m.enabled && (m.model_id == stripped || m.abstract_id.as_deref() == Some(stripped))
            })
            .map(|m| m.model_id.as_str())
            .unwrap_or(stripped)
    }

    /// 非流式 per-attempt 总超时 — 从 `send` 起算至响应体读完，每次重试各自附加，不含
    /// 本地排队。`timeout_ms == 0` = 不限时（零值直接透传 reqwest 语义为「立即超时」）。
    fn apply_attempt_timeout(
        builder: reqwest::RequestBuilder,
        config: &VendorConfig,
    ) -> reqwest::RequestBuilder {
        if config.timeout_ms > 0 {
            builder.timeout(Duration::from_millis(config.timeout_ms))
        } else {
            builder
        }
    }

    /// 出站错误分类 — 超时映射 `Timeout`（客户端可识别 504），其余保持 `VendorUnavailable`。
    fn outbound_send_error(vendor_id: &str, e: &reqwest::Error) -> GateError {
        if e.is_timeout() {
            GateError::Timeout {
                vendor: vendor_id.to_string(),
            }
        } else {
            GateError::VendorUnavailable {
                vendor: vendor_id.to_string(),
            }
        }
    }

    /// 执行带重试的 HTTP 请求。启用 `resilience` 时由 `CircuitBreaker<GateError>` 包装：
    /// Closed/HalfOpen 放行至内部重试；Open 直接拒绝，经 [`from_circuit_call`] 还原为
    /// `VendorUnavailable`。`credential` 固定本请求的 key（401/403 推进轮换游标）。
    #[allow(clippy::too_many_arguments)]
    async fn execute_http_with_retry(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
        retry_cfg: &RetrySection,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker<GateError>>,
        metrics: &GlobalMetrics,
        instance: &VendorInstance,
        credential: &RequestCredential,
    ) -> Result<UnifiedResponse, GateError> {
        #[cfg(feature = "resilience")]
        {
            let vendor_id = config.id.clone();
            let result = circuit_breaker
                .execute(|| async {
                    Self::execute_http_with_retry_policy(
                        client,
                        config,
                        request_body.clone(),
                        converter,
                        retry_cfg,
                        metrics,
                        instance,
                        credential,
                    )
                    .await
                })
                .await;
            result.map_err(|e| from_circuit_call(e, &vendor_id))
        }
        #[cfg(not(feature = "resilience"))]
        {
            let _ = retry_cfg;
            Self::execute_http_with_retry_policy(
                client,
                config,
                request_body,
                converter,
                retry_cfg,
                metrics,
                instance,
                credential,
            )
            .await
        }
    }

    /// HTTP 请求 + 库管重试循环（`limiteron RetryPolicy::execute_with`）。
    ///
    /// 机制/信号分离：attempt 计数、硬上限、睡眠归库；业务决策全在 `decide` 闭包（状态码
    /// 分类热读 `[retry]` 段、per-category 上限、时间预算、逐次延迟）。硬上限 =
    /// max([retry].max_retries, 2)；`credential` 池命中时所有尝试用同一 key。
    #[allow(clippy::too_many_arguments)]
    async fn execute_http_with_retry_policy(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
        retry_cfg: &RetrySection,
        metrics: &GlobalMetrics,
        instance: &VendorInstance,
        credential: &RequestCredential,
    ) -> Result<UnifiedResponse, GateError> {
        let url = Self::build_vendor_url(config)?;
        let policy = RetryDelayPolicy::from_config(retry_cfg);

        // 重试总时间预算（默认 0 = 不限时，行为与现状一致）。
        // 锚点统一 tokio::time::Instant —— std Instant 不随 paused clock
        // 推进，paused-clock 测试与生产同走 tokio 时钟域。
        let budget = if retry_cfg.retry_budget_secs > 0 {
            Some(Duration::from_secs(retry_cfg.retry_budget_secs))
        } else {
            None
        };
        let budget_anchor = tokio::time::Instant::now();

        // 硬上限 = max([retry].max_retries, 网络瞬时固定上限 2)——见函数 doc。
        let hard_max_retries = retry_cfg
            .max_retries
            .max(ErrorCategory::RetryableShort.max_retries());
        let retry_policy = RetryPolicy::new(hard_max_retries, policy.initial_delay)
            .with_factor(policy.factor)
            .with_max_delay(policy.max_delay)
            .with_jitter(policy.jitter);

        retry_policy
            .execute_with(
                || async {
                    let builder =
                        Self::build_authed_attempt_builder(client, config, &url, credential)?;
                    let response =
                        Self::send_attempt(builder, &request_body, config, instance, credential)
                            .await?;
                    let response_json = Self::read_attempt_json(response, config).await?;
                    converter
                        .parse_response(response_json)
                        .map_err(AttemptFailure::Fatal)
                },
                |attempt, failure| {
                    // 决策闭包：业务分类留应用（机制归库——见函数 doc）。
                    Self::decide_retry_delay(
                        attempt,
                        failure,
                        retry_cfg,
                        &policy,
                        budget,
                        budget_anchor,
                        metrics,
                        config,
                    )
                },
            )
            .await
            .map_err(|failure| match failure {
                AttemptFailure::Network { .. } => GateError::VendorUnavailable {
                    vendor: config.id.clone(),
                },
                AttemptFailure::Http { error, .. } => error,
                AttemptFailure::Fatal(error) => error,
            })
    }

    /// 单次尝试的带认证请求构建（per-attempt 总超时，从 send 起算）；legacy 回退在 build 时读 env。
    fn build_authed_attempt_builder(
        client: &reqwest::Client,
        config: &VendorConfig,
        url: &str,
        credential: &RequestCredential,
    ) -> Result<reqwest::RequestBuilder, AttemptFailure> {
        let builder = match credential.api_key() {
            Some(api_key) => Self::apply_attempt_timeout(
                Self::build_authed_request_with_key(client, config, url, true, api_key)
                    .map_err(AttemptFailure::Fatal)?,
                config,
            ),
            None => Self::apply_attempt_timeout(
                Self::build_authed_request(client, config, url, true)
                    .map_err(AttemptFailure::Fatal)?,
                config,
            ),
        };
        Ok(builder)
    }

    /// 单次尝试的发送与非 2xx 处理：Retry-After 头须在消费 body 前读取；key 错误按三态
    /// 处置（永久错误黑名单 + 游标推进，瞬时无动作），本请求剩余尝试仍用同一 key。
    async fn send_attempt(
        builder: reqwest::RequestBuilder,
        request_body: &Value,
        config: &VendorConfig,
        instance: &VendorInstance,
        credential: &RequestCredential,
    ) -> Result<reqwest::Response, AttemptFailure> {
        let response = match builder.json(request_body).send().await {
            Ok(resp) => resp,
            Err(e) => {
                return Err(AttemptFailure::Network {
                    transient: e.is_timeout() || e.is_connect(),
                });
            }
        };

        let status = response.status();
        if !status.is_success() {
            let retry_after = Self::parse_retry_after(response.headers());
            let text = response.text().await.unwrap_or_default();
            let (error_type, error_code) = upstream_error_category(&text);
            tracing::warn!(
                vendor = %config.id,
                status = %status,
                body_bytes = text.len(),
                error_type = ?error_type,
                error_code = ?error_code,
                "{}",
                crate::i18n::t("vendor-upstream-non-success")
            );
            // 永久错误黑名单 + 推进，瞬时错误无 key 动作。
            instance.handle_key_error(credential, status, &text);
            return Err(AttemptFailure::Http {
                status: status.as_u16(),
                retry_after,
                error: GateError::VendorUnavailable {
                    vendor: crate::i18n::t_args(
                        "vendor-upstream-status",
                        &[
                            ("vendor", config.id.clone()),
                            ("status", status.to_string()),
                        ],
                    ),
                },
            });
        }
        Ok(response)
    }

    /// 解析响应（body 读取阶段的超时映射 `Timeout`，与解析失败区分）。
    async fn read_attempt_json(
        response: reqwest::Response,
        config: &VendorConfig,
    ) -> Result<Value, AttemptFailure> {
        match response.json().await {
            Ok(json) => Ok(json),
            Err(e) if e.is_timeout() => Err(AttemptFailure::Fatal(GateError::Timeout {
                vendor: config.id.clone(),
            })),
            Err(e) => Err(AttemptFailure::Fatal(GateError::VendorUnavailable {
                vendor: crate::i18n::t_args("vendor-parse-error", &[("error", e.to_string())]),
            })),
        }
    }

    /// 重试决策：分类 → per-category 上限 → 计数 → 延迟（Retry-After 展宽优先 + 恶意值
    /// 封顶）→ 预算检查。`attempt` 从 1 起，指即将进行的第几次重试。
    #[allow(clippy::too_many_arguments)]
    fn decide_retry_delay(
        attempt: u32,
        failure: &AttemptFailure,
        retry_cfg: &RetrySection,
        policy: &RetryDelayPolicy,
        budget: Option<Duration>,
        budget_anchor: tokio::time::Instant,
        metrics: &GlobalMetrics,
        config: &VendorConfig,
    ) -> RetryDecision {
        // 分类：网络瞬时 → RetryableShort；状态码须在可重试集合内（集合外一律 Fatal）
        let (category, retry_after) = match failure {
            AttemptFailure::Network { transient: true } => {
                (Some(ErrorCategory::RetryableShort), None)
            }
            AttemptFailure::Http {
                status,
                retry_after,
                ..
            } => (
                if retry_cfg.retryable_status_codes.contains(status) {
                    Some(ErrorCategory::from_status_code(*status))
                } else {
                    None
                },
                *retry_after,
            ),
            // 非瞬时网络错误 / 终结失败不可重试
            _ => (None, None),
        };
        let Some(category) = category.filter(ErrorCategory::is_retryable) else {
            return RetryDecision::Stop;
        };

        // per-category 上限：网络瞬时固定 2 次（独立于 max_retries）；状态码路径收紧到 max_retries。
        let max_retries = match category {
            ErrorCategory::RetryableShort => ErrorCategory::RetryableShort.max_retries(),
            _ => retry_cfg.max_retries,
        };
        if attempt > max_retries {
            return RetryDecision::Stop;
        }

        // 重试维度计数 — 仅对实际将发生的重试记一笔（Stop 前置拦截不计数）
        metrics.record_retry_attempt();

        // 延迟：Retry-After 展宽优先，否则按分类退避（RNG 注入，不跨 await）
        let policy_delay = retry_delay_for(category, attempt, policy, &mut rand::thread_rng());
        let delay = match retry_after {
            Some(ra) => spread_delay(ra, policy, &mut rand::thread_rng()),
            None => policy_delay,
        };
        // 安全封顶：超大 Retry-After 会占住 worker + 并发许可，指示值截断至上限并记 warn。
        let delay = {
            const MAX_UPSTREAM_WAIT: Duration = Duration::from_secs(30);
            if delay > MAX_UPSTREAM_WAIT {
                tracing::warn!(
                    vendor = %config.id,
                    status = ?retry_after,
                    capped_secs = MAX_UPSTREAM_WAIT.as_secs(),
                    "upstream retry wait truncated",
                );
                MAX_UPSTREAM_WAIT
            } else {
                delay
            }
        };

        // 预算超限放弃：闭包内 Stop，库原样返回最后错误。
        if let Some(b) = budget {
            if crate::utils::retry::budget_exceeded(budget_anchor, b, delay) {
                return RetryDecision::Stop;
            }
        }

        // 全分支 RetryAfter 原样回传（库按该值精确睡眠，预算检查值与实际等待同值）。
        RetryDecision::RetryAfter(delay)
    }

    /// 读取 vendor 的实时路由状态（判定不读持久化的熔断/限流回写）：1. config
    /// enabled=false → Unavailable；2. CB Open 且超时未过 → Unavailable，已超时或 HalfOpen
    /// → 放行（探测流量是唯一恢复通路）；3. Token 窗口 peek 不过 → Throttled；4. 其余 → VendorState。
    pub async fn get_status(&self, vendor_id: &str) -> VendorStatus {
        let Some(vendor) = self.config.get_vendor(vendor_id) else {
            return VendorStatus::Unavailable;
        };
        if !vendor.enabled {
            return VendorStatus::Unavailable;
        }
        // guard 不跨 await（std RwLockReadGuard 非 Send）
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()
        };
        let Some(instance) = instance else {
            return VendorStatus::Available;
        };

        #[cfg(feature = "resilience")]
        {
            // 读当前 key 指纹 × 全部 model 通道的聚合判定：全部通道 Open 且无一越过
            // 冷却窗才判 Unavailable；表空回落 vendor 级熔断器。
            let fingerprint = instance.key_pool.peek_current_fingerprint();
            let breakers = instance.peek_circuit_breakers_for_fingerprint(fingerprint.as_deref());
            if breakers.is_empty() {
                let circuit_breaker = instance.circuit_breaker.clone();
                if let CircuitState::Open = circuit_breaker.get_state().await {
                    if !Self::circuit_probe_ready(&circuit_breaker).await {
                        return VendorStatus::Unavailable;
                    }
                }
            } else {
                let mut all_open = true;
                let mut any_ready = false;
                for circuit_breaker in &breakers {
                    match circuit_breaker.get_state().await {
                        CircuitState::Open => {
                            if Self::circuit_probe_ready(circuit_breaker).await {
                                any_ready = true;
                            }
                        }
                        // 半开 = 恢复探测窗口：放行探针（并发准入由 half_open_max_calls 控制）
                        CircuitState::HalfOpen => all_open = false,
                        CircuitState::Closed => all_open = false,
                    }
                }
                if all_open && !any_ready {
                    return VendorStatus::Unavailable;
                }
                // 超时已过或存在非 Open 通道：放行（Open→HalfOpen 转换由该请求在 CB execute 内完成）
            }
        }

        // advanced limiter 非消费 peek
        if !instance.advanced_limiter.check().await {
            return VendorStatus::Throttled;
        }
        instance.state.get_status()
    }

    /// CB Open 是否已过超时窗口、可放行探测（只读判定，不触发状态转换）：
    /// 用 `get_stats()` 的墙钟失败时间对齐库内 `duration_since(last_failure) >= timeout`。
    #[cfg(feature = "resilience")]
    async fn circuit_probe_ready(circuit_breaker: &Arc<CircuitBreaker<GateError>>) -> bool {
        let stats = circuit_breaker.get_stats().await;
        match stats.last_failure_time {
            Some(last_failure) => {
                let elapsed = chrono::Utc::now()
                    .signed_duration_since(last_failure)
                    .to_std()
                    .unwrap_or_default();
                elapsed >= circuit_breaker.config().timeout
            }
            None => false,
        }
    }

    /// 直读 vendor 实例的 VendorQueue 深度（`len()` 即 receiver 深度）。
    pub fn get_queue_length(&self, vendor_id: &str) -> usize {
        let guard = self.instances.read().unwrap();
        guard.get(vendor_id).map(|i| i.queue.len()).unwrap_or(0)
    }

    /// vendor 当前在途请求数（信号量已发放未释放的 permit 数；流式 permit 随流存活，
    /// 对长流真实生效）。实例未初始化时返回 0。
    pub fn get_in_flight(&self, vendor_id: &str) -> usize {
        let guard = self.instances.read().unwrap();
        guard
            .get(vendor_id)
            .map(|i| i.concurrency_limiter.in_flight())
            .unwrap_or(0)
    }

    /// 舰队并发占用信号 — (Σ in_flight, Σ 并发上限)，准入的拥塞判定输入；含全部已初始化
    /// 实例，无实例返回 (0, 0)（调用方按无拥塞处理）。
    pub fn fleet_concurrency(&self) -> (usize, usize) {
        let guard = self.instances.read().unwrap();
        let mut total_in_flight = 0usize;
        let mut total_capacity = 0usize;
        for inst in guard.values() {
            total_in_flight += inst.concurrency_limiter.in_flight();
            total_capacity += inst.concurrency_limiter.max_concurrency();
        }
        (total_in_flight, total_capacity)
    }

    /// 出站共享 Client — 探测路径复用与正常请求同一 Client（连接池/CONNECT_TIMEOUT 基线一致）。
    pub(crate) fn outbound_client(&self) -> reqwest::Client {
        self.client.clone()
    }

    /// 探测出站闸门：探测请求与正常请求共用同一 vendor 实例的 RPS 令牌与并发信号量
    /// （permit 持有至探测结束）。实例不存在返回 `None`——调用方跳过该目标。
    pub(crate) async fn acquire_probe_capacity(
        &self,
        vendor_id: &str,
    ) -> Option<ConcurrencyPermit> {
        // 先克隆实例句柄并释放读锁，await 不持锁（clippy await_holding_lock）。
        let inst = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()?
        };
        inst.rate_limiter.acquire().await;
        Some(inst.concurrency_limiter.acquire().await)
    }

    /// 直接获取 vendor 并发 permit — 准入测试钩子与运维排障入口；生产执行路径走
    /// worker 的 acquire。实例不存在返回 None。
    pub async fn acquire_permit(&self, vendor_id: &str) -> Option<ConcurrencyPermit> {
        // 先克隆实例句柄并释放读锁，await 不持锁（clippy await_holding_lock）
        let inst = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()?
        };
        Some(inst.concurrency_limiter.acquire().await)
    }

    /// 全部已初始化实例的在途均值 — `load_signal = "in_flight_relative"` 的归一化基准。
    /// 空表返回 0.0（消费侧分母 max(1, mean) 钳制兜底）。
    pub fn fleet_mean_in_flight(&self) -> f64 {
        let guard = self.instances.read().unwrap();
        if guard.is_empty() {
            return 0.0;
        }
        let total: usize = guard
            .values()
            .map(|i| i.concurrency_limiter.in_flight())
            .sum();
        total as f64 / guard.len() as f64
    }

    pub async fn execute_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, GateError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(GateError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        let (response_tx, response_rx) = oneshot::channel();

        let pending = PendingRequest {
            request,
            response_tx,
            // 延迟口径含排队时间（worker 完成分支以此记账）
            enqueued_at: Instant::now(),
            // 出队时刻由 VendorQueue::recv 单点盖戳，入队方恒 None
            dequeued_at: None,
        };

        instance.queue.send(pending).await?;

        // 总预算 = QUEUE_WAIT_BUDGET（排队段）+ timeout_ms（执行段，per-attempt 从 send
        // 起算）；timeout_ms=0（不限时）时不加执行段。timeout_ms 热读。
        let timeout_ms = self
            .config
            .get_vendor(vendor_id)
            .map(|c| c.timeout_ms)
            .unwrap_or(0);
        let timeout_duration = if timeout_ms > 0 {
            QUEUE_WAIT_BUDGET + Duration::from_millis(timeout_ms)
        } else {
            QUEUE_WAIT_BUDGET
        };
        timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| GateError::Timeout {
                vendor: vendor_id.to_string(),
            })??
    }

    /// 流式启动状态码校验 + 错误记账。非 2xx 时错误 JSON 正文原样透传会伪装成伪 200 流；
    /// 此处对齐非流式路径：429 → `RateLimited`，其余非 2xx → `VendorUnavailable`，
    /// 并补 metrics（此前流式路径零指标）。
    async fn check_stream_status(
        &self,
        vendor_id: &str,
        start: Instant,
        response: reqwest::Response,
        credential: &RequestCredential,
    ) -> Result<reqwest::Response, GateError> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let (error_type, error_code) = upstream_error_category(&text);
        tracing::warn!(
            vendor = %vendor_id,
            status = %status,
            body_bytes = text.len(),
            error_type = ?error_type,
            error_code = ?error_code,
            "{}",
            crate::i18n::t("vendor-stream-upstream-non-success")
        );
        // 永久错误黑名单 + 推进（流式单次尝试，生效于下次请求）；瞬时错误无 key 动作。
        if let Some(instance) = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()
        } {
            instance.handle_key_error(credential, status, &text);
        }
        let duration = start.elapsed();
        self.metrics.record_request(vendor_id, false, duration);
        // 失败率滑窗双记 — 429 亦计入 failures（独立于 CB 分类器）。
        self.metrics
            .record_vendor_outcome_at(vendor_id, false, Instant::now());
        let error = if status.as_u16() == 429 {
            GateError::RateLimited {
                vendor: vendor_id.to_string(),
            }
        } else {
            GateError::VendorUnavailable {
                vendor: crate::i18n::t_args(
                    "vendor-upstream-status",
                    &[
                        ("vendor", vendor_id.to_string()),
                        ("status", status.to_string()),
                    ],
                ),
            }
        };
        self.metrics.record_error(vendor_id, &error.to_string());
        Err(error)
    }

    /// 流式单次派发 = send（含 TTFT 超时）→ 响应头状态校验，两条流式路径的公共派发段。
    ///
    /// cover_all_paths 开启时由 per-vendor CircuitBreaker 包裹（Open 直接拒绝，失败计 CB
    /// 失败）；时长只计到响应头，流建立后的 chunk 传输不在 CB 调用内。
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_stream_once(
        &self,
        vendor_id: &str,
        config: &VendorConfig,
        credential: &RequestCredential,
        model: &str,
        builder: reqwest::RequestBuilder,
        request_body: &Value,
        start: Instant,
    ) -> Result<reqwest::Response, GateError> {
        // 无 resilience 组合下 model/credential 不参与 CB 选路（仅签名统一）
        #[cfg(not(feature = "resilience"))]
        let _ = (model, credential);
        let send = || async {
            let response = if config.stream_ttft_ms > 0 {
                // 流式 TTFT 超时 — 包住 send 发起至响应头返回的段落，流建立前超时以
                // Err(Timeout) 返回。reqwest RequestBuilder 无 Clone，此处 body 尚未附加必为 Some。
                let builder = builder.try_clone().ok_or_else(|| GateError::Config {
                    message: crate::i18n::t_args(
                        "vendor-builder-not-cloneable",
                        &[("vendor", vendor_id.to_string())],
                    ),
                })?;
                match timeout(
                    Duration::from_millis(config.stream_ttft_ms),
                    builder.json(request_body).send(),
                )
                .await
                {
                    Ok(Ok(resp)) => resp,
                    Ok(Err(e)) => return Err(Self::outbound_send_error(vendor_id, &e)),
                    Err(_) => {
                        return Err(GateError::Timeout {
                            vendor: vendor_id.to_string(),
                        })
                    }
                }
            } else {
                let builder = builder.try_clone().ok_or_else(|| GateError::Config {
                    message: crate::i18n::t_args(
                        "vendor-builder-not-cloneable",
                        &[("vendor", vendor_id.to_string())],
                    ),
                })?;
                builder.json(request_body).send().await.map_err(|_e| {
                    GateError::VendorUnavailable {
                        vendor: vendor_id.to_string(),
                    }
                })?
            };
            // 非 2xx 上游不得伪装成 SSE 流
            self.check_stream_status(vendor_id, start, response, credential)
                .await
        };

        #[cfg(feature = "resilience")]
        {
            if self.config.read().resilience.cover_all_paths {
                if let Some(circuit_breaker) = self.circuit_breaker_for(
                    vendor_id,
                    credential,
                    model,
                    Self::cb_endpoint(config),
                ) {
                    let result = circuit_breaker.execute(&send).await;
                    return result.map_err(|e| from_circuit_call(e, vendor_id));
                }
            }
        }
        send().await
    }

    /// 执行流式请求：返回上游 SSE 经分帧与反向解析后的 IR [`StreamChunk`] 流。
    ///
    /// send 后先校验状态码；字节流经 `SseFrameParser` 分帧（跨块缓冲 + 增量 UTF-8），每帧
    /// 经 `parse_stream_frame` 解析（`[DONE]` 提前收流）。permit 随流持有至流结束。
    pub async fn execute_stream_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<impl futures::Stream<Item = Result<StreamChunk, GateError>>, GateError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(GateError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        instance.rate_limiter.acquire().await;
        // token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_unified(&request, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };

        // 热读新鲜配置：api_base/api_key_env 热更新立即生效
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url(&config)?;

        // 请求级凭证选择 — key 在请求启动时固定（池空回退 legacy 读 env）
        let credential = instance.select_request_key(&config);

        // converter 以 Arc 进流（parse_stream_frame 按 vendor 协议分派）
        let converter: Arc<dyn ProtocolConverter> = match config.type_.as_str() {
            "openai" => self.openai_converter.clone(),
            "anthropic" => self.anthropic_converter.clone(),
            _ => {
                return Err(GateError::Config {
                    message: crate::i18n::t("vendor-unknown-type"),
                })
            }
        };

        // per-key 熔断键的 model 段（须在 request 被 move 前捕获）
        let request_model = Self::cb_model(vendor_id, &config, &request.model).to_string();

        // anthropic 转换流式出站同样注入缓存断点（透传流式不注入）
        let cache_policy = (config.type_ == "anthropic").then(|| {
            crate::config::model_catalog::cache_policy_for(
                &self.config.read().model_catalog,
                &request.model,
            )
        });
        let request_body = if let Some(policy) = &cache_policy {
            self.anthropic_converter
                .request_to_external_with_cache_policy(request, policy)?
        } else {
            converter.request_to_external(request)?
        };

        let request_builder = match credential.api_key() {
            Some(api_key) => {
                Self::build_authed_request_with_key(&self.client, &config, &url, true, api_key)?
            }
            None => Self::build_authed_request(&self.client, &config, &url, true)?,
        };

        let start = Instant::now();
        // cover_all_paths 开启时 send + 状态校验段由 CB 包裹（时长只计到响应头）
        let response = self
            .dispatch_stream_once(
                vendor_id,
                &config,
                &credential,
                &request_model,
                request_builder,
                &request_body,
                start,
            )
            .await?;

        // 流式「派发处计」——send 成功（2xx、流已建立）即记成功，错误分支已在
        // check_stream_status 内记账；流中错误不回溯改写派发计数。
        self.metrics
            .record_request(vendor_id, true, start.elapsed());
        // 失败率滑窗双记（send 成功记 total，纯观测，常开）
        self.metrics
            .record_vendor_outcome_at(vendor_id, true, Instant::now());
        // duration 是 send→响应头口径，落 TTFT 道（严格首 token 口径不可得，响应头近似）
        self.sync_ewma_tau();
        self.metrics
            .record_ttft(vendor_id, start.elapsed(), Instant::now());

        // 字节流与分帧反解析装配（与透传流式共用同一实现）
        let idle = Duration::from_millis(config.stream_idle_ms);
        let byte_stream = Self::idle_timeout_byte_stream(response, permit, idle, vendor_id);
        let framed = Self::framed_ir_stream(byte_stream, vendor_id, converter);
        let cooldown = Self::stream_cooldown(&instance);
        Ok(Self::guard_mid_stream_failure(instance, cooldown, framed))
    }

    /// 流中冷却时长 — 与 CB Open 冷却窗同源（实例构造期快照）。
    #[cfg(feature = "resilience")]
    fn stream_cooldown(instance: &Arc<VendorInstance>) -> Duration {
        instance.circuit_open_timeout
    }

    #[cfg(not(feature = "resilience"))]
    fn stream_cooldown(_instance: &Arc<VendorInstance>) -> Duration {
        Duration::from_secs(30)
    }

    /// 流中失败冷却（首字节后上游断流/错误帧）：vendor 置 Throttled，get_status 读侧
    /// TTL 到点惰性恢复。仅 Timeout / VendorUnavailable 触发，网关自身错误不冷却。
    fn guard_mid_stream_failure<T, S>(
        instance: Arc<VendorInstance>,
        cooldown: Duration,
        stream: S,
    ) -> impl futures::Stream<Item = Result<T, GateError>>
    where
        S: futures::Stream<Item = Result<T, GateError>>,
    {
        futures::StreamExt::map(stream, move |item| {
            if let Err(e) = &item {
                if matches!(
                    e,
                    GateError::Timeout { .. } | GateError::VendorUnavailable { .. }
                ) {
                    instance.state.mark_throttled(cooldown);
                    tracing::warn!(
                        vendor = %instance.vendor_id,
                        error = %e,
                        cooldown_ms = cooldown.as_millis() as u64,
                        "{}",
                        crate::i18n::t("vendor-stream-cooldown")
                    );
                }
            }
            item
        })
    }

    /// 上游响应体字节流装配 — permit move 进 unfold，流结束才释放；chunk 间 idle 超时
    /// （0 = 不限时）产出 Err(Timeout) 并由分帧层终止流。两路流式共用。
    fn idle_timeout_byte_stream(
        response: reqwest::Response,
        permit: ConcurrencyPermit,
        idle: Duration,
        vendor_id: &str,
    ) -> impl futures::Stream<Item = Result<Bytes, GateError>> {
        let vendor_template = vendor_id.to_string();
        let inner = Box::pin(response.bytes_stream());
        futures::stream::unfold((inner, permit), move |(mut inner, permit)| {
            // async move 块按值捕获，FnMut 闭包每轮 clone 出独立 owner
            let vendor_for_bytes = vendor_template.clone();
            async move {
                let next = if idle > Duration::ZERO {
                    match timeout(idle, futures::StreamExt::next(&mut inner)).await {
                        Ok(next) => next,
                        Err(_) => {
                            let vendor = vendor_for_bytes.clone();
                            return Some((Err(GateError::Timeout { vendor }), (inner, permit)));
                        }
                    }
                } else {
                    futures::StreamExt::next(&mut inner).await
                };
                match next {
                    Some(Ok(b)) => Some((Ok(b), (inner, permit))),
                    Some(Err(e)) => {
                        let vendor = vendor_for_bytes.clone();
                        Some((Err(Self::outbound_send_error(&vendor, &e)), (inner, permit)))
                    }
                    None => {
                        drop(permit);
                        None
                    }
                }
            }
        })
    }

    /// 上游 SSE 字节流 → 分帧 + 反向解析的 IR StreamChunk 流：[DONE] 提前收流，
    /// 中流错误语义化 Err 终止，EOF 冲刷残余帧。Unfold 闭包非 Unpin，Box::pin 后返回。
    fn framed_ir_stream(
        byte_stream: impl futures::Stream<Item = Result<Bytes, GateError>> + Send + 'static,
        vendor_id: &str,
        converter: Arc<dyn ProtocolConverter>,
    ) -> impl futures::Stream<Item = Result<StreamChunk, GateError>> {
        let vendor = vendor_id.to_string();
        let framed = futures::stream::unfold(
            (
                crate::protocol::SseFrameParser::new(),
                Box::pin(byte_stream),
                false,
                vendor,
                converter,
            ),
            |(mut parser, mut bytes, done, vendor, converter)| async move {
                loop {
                    if done {
                        return None;
                    }
                    // 倾倒 buffer 中已凑齐的帧（一次 unfold 一个 item）
                    if let Some(frame) = parser.next_frame() {
                        if frame.is_error() {
                            // 上游中流错误 → 语义化 Err 提前终止
                            let detail = frame.data.unwrap_or_else(|| {
                                frame
                                    .event
                                    .unwrap_or_else(|| crate::i18n::t("vendor-upstream-error"))
                            });
                            return Some((
                                Err(GateError::VendorUnavailable {
                                    vendor: crate::i18n::t_args(
                                        "vendor-stream-error",
                                        &[("vendor", vendor.clone()), ("detail", detail)],
                                    ),
                                }),
                                (parser, bytes, true, vendor, converter),
                            ));
                        }
                        let Some(data) = frame.data else {
                            continue; // 纯注释/空帧跳过
                        };
                        if data.trim() == "[DONE]" {
                            return None; // OpenAI 终止标记：提前收流
                        }
                        if let Some(chunk) = converter.parse_stream_frame(&data) {
                            return Some((Ok(chunk), (parser, bytes, false, vendor, converter)));
                        }
                        continue; // 无法映射 IR 的生命周期帧跳过
                    }
                    // buffer 无完整帧：拉取上游下一块
                    match futures::StreamExt::next(&mut bytes).await {
                        Some(Ok(b)) => parser.feed(&b),
                        Some(Err(e)) => {
                            tracing::warn!(
                                vendor = %vendor,
                                error = %e,
                                "{}",
                                crate::i18n::t("vendor-upstream-stream-interrupted")
                            );
                            // idle 超时（Timeout）原样透传，其余 IO 错误包装为 VendorUnavailable
                            let err = match e {
                                GateError::Timeout { .. } => e,
                                other => GateError::VendorUnavailable {
                                    vendor: crate::i18n::t_args(
                                        "vendor-stream-interrupted",
                                        &[
                                            ("vendor", vendor.clone()),
                                            ("detail", other.to_string()),
                                        ],
                                    ),
                                },
                            };
                            return Some((Err(err), (parser, bytes, true, vendor, converter)));
                        }
                        None => {
                            // EOF：冲刷残余帧后终止
                            if let Some(chunk) = Self::flush_eof_chunk(&mut parser, &converter) {
                                return Some((Ok(chunk), (parser, bytes, true, vendor, converter)));
                            }
                            return None;
                        }
                    }
                }
            },
        );
        Box::pin(framed)
    }

    /// EOF 冲刷：`parser.flush()` 取残余帧，非错误且非 [DONE] 的可解析帧产出为最后一个
    /// chunk；错误帧/终止标记/无 data 帧静默收流。
    fn flush_eof_chunk(
        parser: &mut crate::protocol::SseFrameParser,
        converter: &Arc<dyn ProtocolConverter>,
    ) -> Option<StreamChunk> {
        let frame = parser.flush()?;
        if frame.is_error() {
            return None;
        }
        let data = frame.data?;
        if data.trim() == "[DONE]" {
            return None;
        }
        converter.parse_stream_frame(&data)
    }

    /// 执行零拷贝透传请求（入站与出站协议相同时使用，避免序列化开销）。
    /// 默认端点按 vendor 协议分派（anthropic → `messages`，OpenAI → `chat/completions`）。
    pub async fn execute_passthrough_request(
        &self,
        vendor_id: &str,
        request_body: Value,
    ) -> Result<Value, GateError> {
        let vendor_type = self
            .fresh_config(vendor_id)
            .map(|c| c.type_)
            .unwrap_or_default();
        let path = Self::default_passthrough_path(&vendor_type);
        self.execute_passthrough_request_at(vendor_id, request_body, path)
            .await
    }

    /// 透传/chat 类请求的默认端点（按 vendor 协议分派）
    fn default_passthrough_path(vendor_type: &str) -> &'static str {
        if vendor_type == "anthropic" {
            "messages"
        } else {
            "chat/completions"
        }
    }

    /// 治理路径的端点可指定版 — embeddings 等 OpenAI 兼容端点经此获得与 chat 相同的
    /// 共享连接池、RPS/并发限流、token 记账与 metrics。
    pub async fn execute_passthrough_request_at(
        &self,
        vendor_id: &str,
        request_body: Value,
        path: &str,
    ) -> Result<Value, GateError> {
        let start = Instant::now();
        let result = self
            .execute_passthrough_request_covered(vendor_id, request_body, path)
            .await;
        let duration = start.elapsed();
        // 透传非流式为全程口径（含本地排队），落 e2e 道；成败都记
        self.sync_ewma_tau();
        self.metrics.record_e2e(vendor_id, duration, Instant::now());
        match &result {
            Ok(_) => {
                self.metrics.record_request(vendor_id, true, duration);
                // 失败率滑窗双记（纯观测，常开）
                self.metrics
                    .record_vendor_outcome_at(vendor_id, true, Instant::now());
            }
            Err(e) => {
                self.metrics.record_request(vendor_id, false, duration);
                // 失败率滑窗双记 — limiter 早退（RateLimited）与上游非 2xx 均计入 failures
                self.metrics
                    .record_vendor_outcome_at(vendor_id, false, Instant::now());
                self.metrics.record_error(vendor_id, &e.to_string());
            }
        }
        result
    }

    /// cover_all_paths 开启时，透传非流式由 per-vendor CircuitBreaker 包裹（CB 拒绝还原为
    /// `VendorUnavailable`，业务错误经 `Inner` 无损透传）；关闭时直连 inner。
    async fn execute_passthrough_request_covered(
        &self,
        vendor_id: &str,
        request_body: Value,
        path: &str,
    ) -> Result<Value, GateError> {
        // 请求级凭证选择（池空 → legacy 读 env）；实例不存在时不取配置，错误交由 inner。
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()
        };
        let resolved = match instance {
            Some(instance) => {
                let config = self.fresh_config(vendor_id)?;
                Some((instance.select_request_key(&config), config))
            }
            None => None,
        };
        #[cfg(feature = "resilience")]
        if let Some((ref credential, ref config)) = resolved {
            if self.config.read().resilience.cover_all_paths {
                let raw_model = request_body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unspecified>");
                let passthrough_model = Self::cb_model(vendor_id, config, raw_model).to_string();
                if let Some(circuit_breaker) = self.circuit_breaker_for(
                    vendor_id,
                    credential,
                    &passthrough_model,
                    Self::cb_endpoint(config),
                ) {
                    let result = circuit_breaker
                        .execute(|| async {
                            self.execute_passthrough_request_inner(
                                vendor_id,
                                request_body.clone(),
                                path,
                                credential,
                            )
                            .await
                        })
                        .await;
                    return result.map_err(|e| from_circuit_call(e, vendor_id));
                }
            }
        }
        let credential = resolved
            .map(|(credential, _config)| credential)
            .unwrap_or(RequestCredential::LegacyEnv);
        self.execute_passthrough_request_inner(vendor_id, request_body, path, &credential)
            .await
    }

    async fn execute_passthrough_request_inner(
        &self,
        vendor_id: &str,
        request_body: Value,
        path: &str,
        credential: &RequestCredential,
    ) -> Result<Value, GateError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(GateError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        instance.rate_limiter.acquire().await;

        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_json(&request_body, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };
        let _permit = permit; // Hold permit during request

        // 热读新鲜配置
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url_at(&config, path)?;

        // 池命中用请求启动时固定的 key；legacy 回退读 env
        let builder = match credential.api_key() {
            Some(api_key) => {
                Self::build_authed_request_with_key(&self.client, &config, &url, true, api_key)?
            }
            None => Self::build_authed_request(&self.client, &config, &url, true)?,
        };
        // per-attempt 总超时（从 send 起算；0=不限时）
        let builder = Self::apply_attempt_timeout(builder, &config);

        let response = match builder.json(&request_body).send().await {
            Ok(resp) => resp,
            // 超时映射 Timeout（客户端可识别 504 语义）
            Err(e) => return Err(Self::outbound_send_error(vendor_id, &e)),
        };

        if !response.status().is_success() {
            return Err(Self::handle_passthrough_non_success(
                vendor_id, &instance, response, credential,
            )
            .await);
        }

        // per-attempt 超时在 body 读取阶段触发时映射 Timeout，
        // 与真正的解析失败区分
        let response_json: Value = Self::read_response_json(vendor_id, response).await?;

        // 更新 Token 使用统计（CAS 全有全无，不足则拒绝记账；语义见 helper）
        Self::account_response_tokens(&instance, vendor_id, &response_json).await;

        Ok(response_json)
    }

    /// 读取透传响应体 JSON：body 阶段超时映射 Timeout，其余解析失败映射 VendorUnavailable。
    async fn read_response_json(
        vendor_id: &str,
        response: reqwest::Response,
    ) -> Result<Value, GateError> {
        match response.json().await {
            Ok(json) => Ok(json),
            Err(e) if e.is_timeout() => Err(GateError::Timeout {
                vendor: vendor_id.to_string(),
            }),
            Err(e) => Err(GateError::VendorUnavailable {
                vendor: crate::i18n::t_args("vendor-parse-error", &[("error", e.to_string())]),
            }),
        }
    }

    /// 透传响应 usage 记账：仅 Token 变体且配额充足时 consume_tokens，不足记 underflow
    /// 告警（CAS 全有全无；非 Token 变体恒 false，不应触发告警）。
    async fn account_response_tokens(
        instance: &VendorInstance,
        vendor_id: &str,
        response_json: &Value,
    ) {
        let Some(usage) = response_json.get("usage") else {
            return;
        };
        let Some(total) = usage.get("total_tokens").and_then(|v| v.as_u64()) else {
            return;
        };
        if !instance.advanced_limiter.accounts_tokens() {
            return;
        }
        let charged = instance.advanced_limiter.consume_tokens(total).await;
        if !charged {
            tracing::warn!(
                vendor = %vendor_id,
                total,
                "{}",
                crate::i18n::t("vendor-token-quota-underflow")
            );
        }
    }

    /// 透传非 2xx 收尾：读 body 记类别化告警、推进 key 轮换游标（生效于下次请求），抛 VendorUnavailable。
    async fn handle_passthrough_non_success(
        vendor_id: &str,
        instance: &VendorInstance,
        response: reqwest::Response,
        credential: &RequestCredential,
    ) -> GateError {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let (error_type, error_code) = upstream_error_category(&text);
        tracing::warn!(
            vendor = %vendor_id,
            status = %status,
            body_bytes = text.len(),
            error_type = ?error_type,
            error_code = ?error_code,
            "{}",
            crate::i18n::t("vendor-upstream-non-success")
        );
        // 永久错误黑名单 + 推进（生效于下次请求）；瞬时无动作。
        instance.handle_key_error(credential, status, &text);
        GateError::VendorUnavailable {
            vendor: crate::i18n::t_args(
                "vendor-upstream-status",
                &[
                    ("vendor", vendor_id.to_string()),
                    ("status", status.to_string()),
                ],
            ),
        }
    }

    /// 执行零拷贝透传流式请求。流错误类型统一为 [`GateError`]（idle 超时以 `Timeout`
    /// 产出），下游 `reframe_passthrough_stream` 泛型于 `E: Display`。
    pub async fn execute_passthrough_stream_request(
        &self,
        vendor_id: &str,
        request_body: Value,
    ) -> Result<impl futures::Stream<Item = Result<Bytes, GateError>>, GateError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(GateError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        instance.rate_limiter.acquire().await;

        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_json(&request_body, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };

        // 热读新鲜配置
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url(&config)?;

        // 请求级凭证选择 — key 在请求启动时固定（池空回退 legacy 读 env）
        let credential = instance.select_request_key(&config);

        let builder = match credential.api_key() {
            Some(api_key) => {
                Self::build_authed_request_with_key(&self.client, &config, &url, false, api_key)?
            }
            None => Self::build_authed_request(&self.client, &config, &url, false)?,
        };
        // per-key 熔断键的 model 段（透传 model 取自请求体，归一到配置模型身份）
        let raw_model = request_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("<unspecified>");
        let request_model = Self::cb_model(vendor_id, &config, raw_model).to_string();

        let start = Instant::now();
        // TTFT 超时与状态校验段收拢于 dispatch_stream_once（cover_all_paths 开启时由 CB 包裹）
        let response = self
            .dispatch_stream_once(
                vendor_id,
                &config,
                &credential,
                &request_model,
                builder,
                &request_body,
                start,
            )
            .await?;

        // 流式「派发处计」——send 成功即记成功（错误分支已在 check_stream_status 内记账）
        self.metrics
            .record_request(vendor_id, true, start.elapsed());
        // 失败率滑窗双记（send 成功记 total，纯观测，常开）
        self.metrics
            .record_vendor_outcome_at(vendor_id, true, Instant::now());
        // send→响应头口径落 TTFT 道（响应头近似，同上）
        self.sync_ewma_tau();
        self.metrics
            .record_ttft(vendor_id, start.elapsed(), Instant::now());

        // permit move 进返回的 Stream，流结束才释放（并发上限对流式生效）；idle 超时
        // （0 = 不限时）与流中失败冷却两路共用。
        let idle = Duration::from_millis(config.stream_idle_ms);
        let stream = Self::idle_timeout_byte_stream(response, permit, idle, vendor_id);
        let cooldown = Self::stream_cooldown(&instance);
        Ok(Box::pin(Self::guard_mid_stream_failure(
            instance, cooldown, stream,
        )))
    }

    pub fn update_status(&self, vendor_id: &str, status: VendorStatus) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.set_status(status);
        }
    }

    pub fn mark_busy(&self, vendor_id: &str) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_busy();
        }
    }

    pub fn mark_available(&self, vendor_id: &str) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_available();
        }
    }

    pub fn mark_throttled(&self, vendor_id: &str, backoff: Duration) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_throttled(backoff);
        }
    }

    /// 管理面重置：清空该 vendor 的 key 池黑名单（含游标）与冷却状态。未知 vendor
    /// 返回 false（调用方转 404 风格显性错误）。
    pub fn reset_key_pool_state(&self, vendor_id: &str) -> bool {
        let guard = self.instances.read().unwrap();
        match guard.get(vendor_id) {
            Some(instance) => {
                instance.reset_resilience_state();
                true
            }
            None => false,
        }
    }

    /// 健康探针目标采集：一次周期内只读快照（探测 HTTP 在锁外执行）；`suspects_only`
    /// 为 true 时仅返回非 Available 或有黑名单 key 的 vendor。
    pub(crate) async fn probe_targets(&self, suspects_only: bool) -> Vec<ProbeTarget> {
        let instances: Vec<(String, Arc<VendorInstance>)> = {
            let guard = self.instances.read().unwrap();
            guard
                .iter()
                .map(|(id, instance)| (id.clone(), instance.clone()))
                .collect()
        };
        let mut targets = Vec::new();
        for (vendor_id, instance) in instances {
            let status = self.get_status(&vendor_id).await;
            let blacklist = instance.key_pool.blacklisted_fingerprints();
            if suspects_only && status == VendorStatus::Available && blacklist.is_empty() {
                continue;
            }
            let config = self.config.read();
            let Some(vendor_config) = config.vendors.iter().find(|v| v.id == vendor_id).cloned()
            else {
                continue;
            };
            drop(config);
            let keys: Vec<(String, String)> = instance
                .key_pool
                .keys_snapshot()
                .into_iter()
                .map(|key| {
                    let fingerprint = crate::vendor::key_pool::fingerprint_of(&key);
                    (key, fingerprint)
                })
                .collect();
            targets.push(ProbeTarget {
                vendor_id,
                config: vendor_config,
                status,
                keys,
                blacklist,
                current: instance.key_pool.current_key(),
            });
        }
        targets
    }

    /// 健康探针 key 恢复（minimal_call 成功）：解黑名单 + 清冷却 + 重置该 key 名下模型级熔断计数。
    pub(crate) async fn probe_recover_key(&self, vendor_id: &str, fingerprint: &str) {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()
        };
        if let Some(instance) = instance {
            instance.probe_recover_key(fingerprint).await;
        }
    }

    pub fn inc_error(&self, vendor_id: &str) -> u8 {
        let guard = self.instances.read().unwrap();
        guard
            .get(vendor_id)
            .map(|i| i.state.inc_error())
            .unwrap_or(0)
    }

    pub fn get_all_status(&self) -> Vec<(String, VendorStatus, usize)> {
        let guard = self.instances.read().unwrap();
        let mut status = Vec::new();

        for (id, instance) in guard.iter() {
            // 直读实例队列深度（同 get_queue_length）
            status.push((
                id.clone(),
                instance.state.get_status(),
                instance.queue.len(),
            ));
        }

        status
    }

    /// 获取厂商的窗口信息，用于路由权重计算：Token/Interval 经 `window_snapshot()`
    /// 取精确 remaining / reset_secs；TimeWindow 保留时段窗口语义。
    pub async fn get_window_info(&self, vendor_id: &str) -> Option<WindowInfo> {
        // guard 不跨 await（std RwLockReadGuard 非 Send）
        let instance = {
            let guard = self.instances.read().ok()?;
            guard.get(vendor_id).cloned()
        }?;

        match &instance.advanced_limiter {
            super::AdvancedRateLimiter::Token { .. }
            | super::AdvancedRateLimiter::Interval { .. } => {
                let (limit, used, reset_secs) = instance.advanced_limiter.window_snapshot().await?;
                Some(WindowInfo {
                    max_tokens: Some(limit),
                    used_tokens: used,
                    window_end: Some(Instant::now() + Duration::from_secs(reset_secs)),
                })
            }
            super::AdvancedRateLimiter::TimeWindow { start, end } => {
                let now = chrono::Local::now().time();
                // 跨午夜判定统一委托 time_window_active；跨午夜窗口的 window_end 按同日内
                // 插值，signed_duration_since 为负时退化为 now（保守判「即将结束」，不再整段排除）。
                if !super::rate_limiter::time_window_active(now, *start, *end) {
                    Some(WindowInfo {
                        max_tokens: None,
                        used_tokens: 0,
                        window_end: None,
                    })
                } else {
                    let now_local = chrono::Local::now();
                    let end_datetime: chrono::DateTime<chrono::Local> = now_local
                        .date_naive()
                        .and_time(*end)
                        .and_local_timezone(chrono::Local)
                        .single()
                        .unwrap_or(now_local);
                    let end_dt = Instant::now()
                        + end_datetime
                            .signed_duration_since(now_local)
                            .to_std()
                            .unwrap_or_default();
                    Some(WindowInfo {
                        max_tokens: None,
                        used_tokens: 0,
                        window_end: Some(end_dt),
                    })
                }
            }
            super::AdvancedRateLimiter::None => Some(WindowInfo {
                max_tokens: None,
                used_tokens: 0,
                window_end: None,
            }),
        }
    }
}

/// 提取上游错误体的机器可分类字段（`error.type` / `error.code`），覆盖 OpenAI 与
/// Anthropic 两种形状；非 JSON 或形状不符返回 `(None, None)`。刻意不提取
/// `error.message`——厂商文案可能回显请求敏感数据，不得进入日志。
fn upstream_error_category(text: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return (None, None);
    };
    let Some(error) = value.get("error") else {
        return (None, None);
    };
    let category = |key: &str| {
        error
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    (category("type"), category("code"))
}

#[cfg(test)]
mod upstream_error_category_tests {
    use super::upstream_error_category;

    #[test]
    fn extracts_openai_error_type_and_code() {
        let body = r#"{"error":{"message":"Invalid API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#;
        assert_eq!(
            upstream_error_category(body),
            (
                Some("invalid_request_error".to_string()),
                Some("invalid_api_key".to_string())
            )
        );
    }

    #[test]
    fn extracts_anthropic_error_type_without_code() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(
            upstream_error_category(body),
            (Some("authentication_error".to_string()), None)
        );
    }

    #[test]
    fn non_json_body_yields_none_pair_no_leak_channel() {
        // 回显类响应体（含鉴权头样式文本）：无 JSON 类别可提取，
        // 且该文本不再进入任何日志字段（warn 只记 body_bytes）。
        let body = "Authorization: Bearer sk-test-ABCDEFGHIJKLMNOPQRSTUVWXYZ was invalid";
        assert_eq!(upstream_error_category(body), (None, None));
    }

    #[test]
    fn json_without_error_shape_yields_none_pair() {
        assert_eq!(upstream_error_category(r#"{"ok":true}"#), (None, None));
        assert_eq!(upstream_error_category(""), (None, None));
    }
}

#[cfg(test)]
mod tests {
    //! `WindowInfo` 单元测试 — 纯逻辑无 IO，覆盖三个公开方法的边界与典型场景
    //! （None 分支、恰好等于阈值、异常输入、数值精度）。

    use super::*;
    use crate::config::{Config, ModelCatalogEntry};
    use std::time::Duration;

    // ============ is_ending_soon ============

    #[test]
    fn test_is_ending_soon_returns_false_when_no_window_end() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 0,
            window_end: None,
        };
        assert!(!info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_false_when_far_in_future() {
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: Some(Instant::now() + Duration::from_secs(600)),
        };
        assert!(!info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_true_when_within_5_minutes() {
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: Some(Instant::now() + Duration::from_secs(60)),
        };
        assert!(info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_true_when_window_already_passed() {
        // 结束时间已过去：duration_since 饱和到 0 → 仍判即将结束（锁定语义）
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: Some(Instant::now() - Duration::from_secs(60)),
        };
        assert!(info.is_ending_soon());
    }

    // ============ has_budget ============

    #[test]
    fn test_has_budget_true_when_no_limit() {
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 1_000_000,
            window_end: None,
        };
        assert!(info.has_budget());
    }

    #[test]
    fn test_has_budget_true_when_partial_usage() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 500,
            window_end: None,
        };
        assert!(info.has_budget());
    }

    #[test]
    fn test_has_budget_false_when_exactly_at_limit() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 1000,
            window_end: None,
        };
        assert!(!info.has_budget());
    }

    #[test]
    fn test_has_budget_false_when_over_limit() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 2000,
            window_end: None,
        };
        assert!(!info.has_budget());
    }

    // ============ budget_percentage ============

    #[test]
    fn test_budget_percentage_one_when_no_limit() {
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 1.0);
    }

    #[test]
    fn test_budget_percentage_one_when_nothing_used() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 1.0);
    }

    #[test]
    fn test_budget_percentage_half_when_half_used() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 500,
            window_end: None,
        };
        let pct = info.budget_percentage();
        assert!((pct - 0.5).abs() < 1e-9, "expected 0.5, got {}", pct);
    }

    #[test]
    fn test_budget_percentage_zero_when_fully_used() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 1000,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_budget_percentage_clamped_to_zero_when_over_used() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 2000,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_budget_percentage_zero_when_max_is_zero() {
        // max=0 防除零特判
        let info = WindowInfo {
            max_tokens: Some(0),
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_window_info_debug_clone_roundtrip() {
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 500,
            window_end: Some(Instant::now() + Duration::from_secs(60)),
        };
        let cloned = info.clone();
        assert_eq!(cloned.max_tokens, info.max_tokens);
        assert_eq!(cloned.used_tokens, info.used_tokens);
        let _ = format!("{:?}", info);
    }

    // ======== per-model 能力目录查询 ========

    /// 带 model_catalog 的 Config 基座（单 vendor，type 可调）
    fn catalog_test_config(vendor_type: &str, catalog: Vec<ModelCatalogEntry>) -> Config {
        let mut vendor = pool_vendor_config("NEXUSFLOW_T006_CAP", "NEXUSFLOW_T006_CAP");
        vendor.type_ = vendor_type.into();
        vendor.enabled = true;
        Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![vendor],
            global_model: None,
            pricing: vec![],
            model_catalog: catalog,
            auth: None,
            cors: None,
            retry: crate::config::RetrySection::default(),
            routing: crate::config::RoutingSection::default(),
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
            docs: Default::default(),
        }
    }

    /// 登记模型返回目录真实值（context_window 非 0），未登记回落 per-flavor 默认。
    #[test]
    fn model_capabilities_registered_real_value_and_flavor_fallback() {
        let config = catalog_test_config(
            "anthropic",
            vec![ModelCatalogEntry {
                model_id: "claude-sonnet-4-20250514".into(),
                context_window: 200_000,
                max_output_tokens: Some(8192),
                supports_tools: true,
                supports_thinking: true,
                ..Default::default()
            }],
        );
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);

        let caps = manager.model_capabilities("anthropic", "claude-sonnet-4-20250514");
        assert_eq!(caps.max_context_tokens, 200_000);
        assert_eq!(caps.max_output_tokens, Some(8192));
        assert!(caps.supports_thinking);

        // 未登记：per-flavor 默认（openai 128k / anthropic 200k / 未知类型保守档）
        assert_eq!(
            manager
                .model_capabilities("openai", "unregistered-model")
                .max_context_tokens,
            128_000
        );
        assert_eq!(
            manager
                .model_capabilities("anthropic", "unregistered-model")
                .max_context_tokens,
            200_000
        );
        assert_eq!(
            manager
                .model_capabilities("ollama", "local-model")
                .max_context_tokens,
            4_096
        );
    }

    /// 热更新登记条目后 capabilities 查询立即反映（DynamicField 热读）。
    #[test]
    fn model_capabilities_reflect_hot_update_immediately() {
        let config = catalog_test_config("openai", vec![]);
        let cm = Arc::new(ConfigManager::new(config));
        let dir = tempfile::TempDir::new().expect("tempdir");
        cm.set_path(dir.path().join("config.toml"));
        let manager = VendorManager::new(cm.clone());

        // 登记前：未登记回落 openai 默认 128k
        assert_eq!(
            manager
                .model_capabilities("openai", "gpt-9-turbo")
                .max_context_tokens,
            128_000
        );

        let mut updated = cm.get_config();
        updated.model_catalog = vec![ModelCatalogEntry {
            model_id: "gpt-9-turbo".into(),
            context_window: 400_000,
            ..Default::default()
        }];
        cm.update_config(updated).expect("hot update");

        assert_eq!(
            manager
                .model_capabilities("openai", "gpt-9-turbo")
                .max_context_tokens,
            400_000,
            "热更新登记后 capabilities 立即反映"
        );
    }

    /// vendor_id 便捷查询：按 type_ 分派 per-flavor 默认；未知 vendor 返回 None。
    #[tokio::test]
    async fn model_capabilities_for_vendor_resolves_type() {
        let config = catalog_test_config("anthropic", vec![]);
        let cm = Arc::new(ConfigManager::new(config));
        let manager = VendorManager::new(cm);
        manager.initialize().await.expect("init");

        let caps = manager
            .model_capabilities_for_vendor("v-pool-test", "unregistered-model")
            .expect("vendor known");
        assert_eq!(
            caps.max_context_tokens, 200_000,
            "未登记模型回落 vendor type 的默认"
        );
        assert!(
            manager
                .model_capabilities_for_vendor("ghost", "m")
                .is_none(),
            "未知 vendor 返回 None"
        );
    }

    // ======== 按 key 指纹隔离的熔断计数（单测） ========

    fn pool_vendor_config(env_a: &str, env_b: &str) -> VendorConfig {
        VendorConfig {
            id: "v-pool-test".into(),
            name: "Pool Test".into(),
            type_: "openai".into(),
            api_key_env: String::new(),
            api_key_envs: vec![env_a.to_string(), env_b.to_string()],
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: crate::config::RateLimitConfig::Token {
                max_tokens: 1000,
                window_hours: None,
                sliding: false,
            },
            concurrency: crate::config::ConcurrencyConfig::default(),
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
        }
    }

    /// 同 key 跨请求共享计数（Arc 同源，5 次失败累计至阈值 → Open）；不同 key 隔离
    /// （独立构造且保持 Closed）。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn per_key_circuit_breaker_shared_for_same_key_isolated_across_keys() {
        const ENV_A: &str = "NEXUSFLOW_MGR_POOL_A";
        const ENV_B: &str = "NEXUSFLOW_MGR_POOL_B";
        std::env::set_var(ENV_A, "mgr-key-a");
        std::env::set_var(ENV_B, "mgr-key-b");

        let config = pool_vendor_config(ENV_A, ENV_B);
        let instance = VendorInstance::new(
            &config,
            10,
            100,
            #[cfg(feature = "resilience")]
            Duration::from_secs(30),
            #[cfg(feature = "cost-tracking")]
            Arc::new(CostTracker::new()),
        );

        // 同 key 跨请求共享计数：同 key 两次凭证选择的指纹一致，取到同一 CB
        const MODEL: &str = "gpt-4";
        const ENDPOINT: &str = "https://unit.test/v1";
        let credential_a = instance.select_request_key(&config);
        let fp_a = credential_a.fingerprint().unwrap().to_string();
        let cb_a1 = instance.circuit_breaker_for(&credential_a, MODEL, ENDPOINT);
        let cb_a2 = instance.circuit_breaker_for(&credential_a, MODEL, ENDPOINT);
        assert!(
            Arc::ptr_eq(&cb_a1, &cb_a2),
            "同 key 同 model 跨请求必须共享同一熔断器（共享计数）"
        );

        // 不同 key 隔离：401 推进游标后下一请求用第二把 key，指纹互异、熔断器独立
        assert!(instance.key_pool.advance("v-pool-test", 401));
        let credential_b = instance.select_request_key(&config);
        let cb_b = instance.circuit_breaker_for(&credential_b, MODEL, ENDPOINT);
        assert_ne!(
            credential_a.fingerprint(),
            credential_b.fingerprint(),
            "不同 key 指纹互异"
        );
        assert!(
            !Arc::ptr_eq(&cb_a1, &cb_b),
            "不同 key 必须使用独立熔断器（隔离计数）"
        );

        // 语义后果：key A 触阈 Open，key B 全程 Closed 不受污染
        for _ in 0..5 {
            let _: Result<u32, limiteron::circuit::CircuitCallError<GateError>> = cb_a1
                .execute(|| async {
                    Err(GateError::VendorUnavailable {
                        vendor: "v-pool-test".into(),
                    })
                })
                .await;
        }
        assert_eq!(
            cb_a1.get_state().await,
            CircuitState::Open,
            "5 次失败在同 key 熔断器上累计至阈值（failure_threshold=5）"
        );
        assert_eq!(
            cb_b.get_state().await,
            CircuitState::Closed,
            "不同 key 的熔断计数不被污染（隔离）"
        );

        // 指纹跨请求稳定：游标停在新位后再选择仍得同 key 与同一熔断器
        let credential_b_again = instance.select_request_key(&config);
        assert_eq!(credential_b_again.fingerprint(), credential_b.fingerprint());
        assert!(Arc::ptr_eq(
            &instance.circuit_breaker_for(&credential_b_again, MODEL, ENDPOINT),
            &cb_b
        ));
        let _ = fp_a;
    }

    /// per-key 熔断键含 model 维度——同 vendor 同 key 不同 model 的失败计数互不污染。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn per_key_circuit_breaker_isolated_across_models() {
        const ENV_A: &str = "NEXUSFLOW_MGR_POOL_MODELA";
        std::env::set_var(ENV_A, "mgr-key-models");
        let config = pool_vendor_config(ENV_A, ENV_A);
        let instance = VendorInstance::new(
            &config,
            10,
            100,
            #[cfg(feature = "resilience")]
            Duration::from_secs(30),
            #[cfg(feature = "cost-tracking")]
            Arc::new(CostTracker::new()),
        );

        let credential = instance.select_request_key(&config);
        let fp = credential.fingerprint().unwrap().to_string();

        // 同 key 同 model 同 endpoint → 同一熔断器（共享计数）
        let cb_m1 = instance.circuit_breaker_for(&credential, "model-a", "https://unit.test/v1");
        let cb_m1_again =
            instance.circuit_breaker_for(&credential, "model-a", "https://unit.test/v1");
        assert!(Arc::ptr_eq(&cb_m1, &cb_m1_again), "同 model 通道共享计数");

        // 同 key 不同 model → 独立熔断器（互不污染）
        let cb_m2 = instance.circuit_breaker_for(&credential, "model-b", "https://unit.test/v1");
        assert!(
            !Arc::ptr_eq(&cb_m1, &cb_m2),
            "不同 model 必须使用独立熔断器（键含 model 维度）"
        );

        // 语义后果：model-a 通道 5 次失败触阈 Open，model-b 通道保持 Closed
        for _ in 0..5 {
            let _: Result<u32, limiteron::circuit::CircuitCallError<GateError>> = cb_m1
                .execute(|| async {
                    Err(GateError::VendorUnavailable {
                        vendor: "v-pool-test".into(),
                    })
                })
                .await;
        }
        assert_eq!(
            cb_m1.get_state().await,
            CircuitState::Open,
            "model-a 通道按自身计数熔断"
        );
        assert_eq!(
            cb_m2.get_state().await,
            CircuitState::Closed,
            "model-b 通道计数不被 model-a 污染（互不污染）"
        );

        // 同 key 同 model 不同 endpoint → 独立熔断器（键含 endpoint 维度）
        let cb_m1_other =
            instance.circuit_breaker_for(&credential, "model-a", "https://other.test/v1");
        assert!(
            !Arc::ptr_eq(&cb_m1, &cb_m1_other),
            "不同 endpoint 必须使用独立熔断器"
        );
        assert_eq!(cb_m1_other.get_state().await, CircuitState::Closed);
        let _ = fp;
    }

    /// legacy 回退（池空）走 vendor 级熔断器——不进 per-key 表。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn legacy_env_fallback_uses_vendor_level_breaker() {
        const MISSING: &str = "NEXUSFLOW_MGR_POOL_MISSING_Q7";
        std::env::remove_var(MISSING);
        let config = pool_vendor_config(MISSING, MISSING);
        let instance = VendorInstance::new(
            &config,
            10,
            100,
            #[cfg(feature = "resilience")]
            Duration::from_secs(30),
            #[cfg(feature = "cost-tracking")]
            Arc::new(CostTracker::new()),
        );
        let credential = instance.select_request_key(&config);
        assert_eq!(
            credential,
            RequestCredential::LegacyEnv,
            "全缺 env 回退 legacy"
        );
        assert!(
            Arc::ptr_eq(
                &instance.circuit_breaker_for(&credential, "gpt-4", "https://unit.test/v1"),
                &instance.circuit_breaker
            ),
            "legacy 回退使用 vendor 级熔断器（与历史逐位一致）"
        );
        assert!(instance.key_pool_info().size == 0);
    }

    // ======== 熔断器 slow-call-rate 生产构造证明 ========

    /// 生产构造证明：经 [`gate_circuit_config`] 建 CB，3 次 800ms 成功调用必须保持
    /// Closed。真睡眠 3×800ms 是必要成本（MockClock 无法放大 execute 实测时长）。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn slow_ok_calls_do_not_open_production_circuit() {
        let cb = CircuitBreaker::new(gate_circuit_config(Duration::from_secs(30)));

        for i in 0..3 {
            // Open 态拒绝以 CircuitCallError::Open 独立承载
            let result: Result<u32, limiteron::circuit::CircuitCallError<GateError>> = cb
                .execute(|| async {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    Ok::<u32, GateError>(1)
                })
                .await;
            assert!(result.is_ok(), "slow call {} should succeed", i);
        }

        assert_eq!(
            cb.get_state().await,
            limiteron::error::CircuitState::Closed,
            "800ms 成功调用是 LLM 延迟常态，不得触发慢调用率熔断"
        );
    }

    // ======== 出站自定义头（单测） ========

    fn extra_headers_config(pairs: &[(&str, &str)]) -> VendorConfig {
        let mut c = pool_vendor_config("NEXUSFLOW_MGR_POOL_A", "NEXUSFLOW_MGR_POOL_B");
        c.extra_headers = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        c
    }

    fn built_headers(config: &VendorConfig) -> reqwest::header::HeaderMap {
        let client = reqwest::Client::new();
        VendorManager::build_authed_request_with_key(
            &client,
            config,
            "http://127.0.0.1:9/v1/chat/completions",
            true,
            "test-key",
        )
        .expect("构建必须成功")
        .build()
        .expect("Request 可构建")
        .headers()
        .clone()
    }

    /// 自定义头注入：配置的额外头出现在出站请求上，网关鉴权头不受影响。
    #[test]
    fn extra_headers_are_injected_into_outbound_request() {
        let config =
            extra_headers_config(&[("x-tenant-id", "acme"), ("x-trace-namespace", "prod")]);
        let headers = built_headers(&config);
        assert_eq!(headers["x-tenant-id"], "acme");
        assert_eq!(headers["x-trace-namespace"], "prod");
        assert_eq!(headers["authorization"], "Bearer test-key");
    }

    /// 冲突跳过：与网关头同名（大小写不敏感）的配置项被跳过并保留网关值。
    #[test]
    fn extra_headers_conflicting_with_gateway_headers_are_skipped() {
        let config = extra_headers_config(&[
            ("Authorization", "Bearer evil"),
            ("X-API-KEY", "evil"),
            ("content-length", "999"),
            ("x-tenant-id", "acme"),
        ]);
        let headers = built_headers(&config);
        assert_eq!(
            headers["authorization"], "Bearer test-key",
            "网关鉴权头不得被覆盖"
        );
        assert_eq!(headers["x-tenant-id"], "acme", "非冲突头正常注入");
    }

    /// 空表零行为变化：无 extra_headers 时出站头不变。
    #[test]
    fn empty_extra_headers_leave_outbound_headers_unchanged() {
        let config = extra_headers_config(&[]);
        let headers = built_headers(&config);
        assert_eq!(headers["authorization"], "Bearer test-key");
        assert!(headers.get("x-tenant-id").is_none());
    }

    /// helper 参数值断言：默认 30s 窗口与 [resilience].circuit_open_timeout_ms 缺省一致；
    /// 短窗口注入后仅 timeout 变化、其余参数不动。
    #[cfg(feature = "resilience")]
    #[test]
    fn gate_circuit_config_values() {
        let config = gate_circuit_config(Duration::from_secs(30));
        assert_eq!(config.failure_threshold, 5);
        assert_eq!(config.success_threshold, 3);
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(
            config.slow_call_duration_threshold,
            Duration::from_secs(60),
            "慢调用时长阈值必须抬至 60s（recId=1）"
        );
        assert!(
            (config.slow_call_rate_threshold - 1.0).abs() < f64::EPSILON,
            "慢调用率阈值必须为 1.0（recId=1）：混合流量下慢调用路径失活"
        );

        let short = gate_circuit_config(Duration::from_millis(300));
        assert_eq!(short.timeout, Duration::from_millis(300));
        assert_eq!(short.failure_threshold, config.failure_threshold);
        assert_eq!(short.slow_call_duration_threshold, Duration::from_secs(60));

        // 配置缺省 ↔ 熔断构造缺省必须同源（改一侧不同步另一侧时此处翻红）
        assert_eq!(
            Duration::from_millis(
                crate::config::ResilienceSection::default().circuit_open_timeout_ms
            ),
            Duration::from_secs(30)
        );
    }
}
