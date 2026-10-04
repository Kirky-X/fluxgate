// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(feature = "cost-tracking")]
use super::CostCalculator;
use super::{
    AdvancedRateLimiter, ConcurrencyLimiter, ConcurrencyPermit, PendingRequest, TokenBucket,
    VendorQueue, VendorState, VendorStatus,
};
use crate::config::{ConfigManager, RetrySection, VendorConfig};
use crate::error::GateError;
use crate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
use crate::protocol::{StreamChunk, UnifiedRequest, UnifiedResponse};
use crate::utils::metrics::GlobalMetrics;
use crate::utils::retry::{retry_delay_for, ErrorCategory, RetryDelayPolicy};
#[cfg(feature = "cost-tracking")]
use crate::utils::CostTracker;
#[cfg(feature = "resilience")]
use crate::vendor::resilience::error_map::{from_circuit_call, to_limiteron, GateErrorClassifier};
use bytes::Bytes;
#[cfg(feature = "resilience")]
use limiteron::circuit::{CircuitBreaker, CircuitBreakerConfig};
#[cfg(feature = "resilience")]
use limiteron::error::CircuitState;
use reqwest;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::time::timeout;

/// 窗口信息
#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// 最大 Token 数量限制
    pub max_tokens: Option<u64>,
    /// 已使用 Token 数量
    pub used_tokens: u64,
    /// 窗口结束时间（如果是时间段窗口）
    pub window_end: Option<Instant>,
}

impl WindowInfo {
    /// 检查时间窗口是否即将结束（剩余时间 < 5 分钟）
    pub fn is_ending_soon(&self) -> bool {
        if let Some(end) = self.window_end {
            let remaining = end.duration_since(Instant::now());
            remaining.as_secs() < 300
        } else {
            false
        }
    }

    /// 检查是否还有预算
    pub fn has_budget(&self) -> bool {
        if let Some(max) = self.max_tokens {
            self.used_tokens < max
        } else {
            true
        }
    }

    /// 计算预算百分比 (0.0 - 1.0)
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

/// 厂商运行时有状态件。设计变更（recId=13）：不再持有 `config: VendorConfig`
/// 启动快照 — 执行路径每次请求经 `ConfigManager::get_vendor` 热读新鲜配置，
/// config.toml 热更新（api_base/api_key_env/enabled 等）立即生效；
/// queue/limiter/circuit_breaker 等有状态件保留启动时构造。
/// recId=18：token 加权票据策略 — VendorConfig.concurrency 的
/// token_ticket_* 三元（vendor 实例构造期定死，restart-required）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TicketPolicy {
    pub min: u64,
    pub max: u64,
    pub default: u64,
}

/// recId=18：token 加权票据成本估算（Unified 形态，worker/转换流路径）——
/// cost = clamp(messages 序列化字节/4 + max_tokens, min, max)。**估算
/// 口径声明**：序列化字节/4 对英文文本近似 token 数，对中文/多模态
/// 偏差大（约 1.5-3×）——保守方向可调低 token_ticket_max；max_tokens
/// 缺省用 policy.default 兜底（容量规划前提）。
pub fn estimate_cost_unified(req: &UnifiedRequest, policy: &TicketPolicy) -> usize {
    let bytes = serde_json::to_string(&req.messages)
        .map(|s| s.len())
        .unwrap_or(0);
    let max_tokens = req.max_tokens.map(|v| v as u64).unwrap_or(policy.default);
    ((bytes / 4) as u64 + max_tokens).clamp(policy.min, policy.max) as usize
}

/// recId=18：token 加权票据成本估算（JSON 形态，透传路径）——
/// 序列化字节/4 + max_tokens 字段（缺省兜底同上）。口径声明同上。
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
    /// recId=18：token 加权票据策略 — None = 未开启（单票据，现状）
    ticket: Option<TicketPolicy>,
    advanced_limiter: AdvancedRateLimiter,
    #[cfg(feature = "resilience")]
    circuit_breaker: Arc<CircuitBreaker>,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
}

/// recId=1：生产熔断器配置的单一构造点（tests/circuit_breaker_test.rs 与
/// manager 测试同源引用，生产接线与测试证明合一）。
///
/// 缺陷背景：`CircuitBreakerConfig::new` 之后慢调用参数取 limiteron 默认
/// duration=500ms / rate=0.5（../limiteron src/constants.rs:106/111），
/// 而 LLM 成功调用常态超 500ms——混合流量下慢调用率轻易过阈，熔断与
/// 真实失败无关。修复取双参数，语义如实声明（均为 ≥ 比较）：
/// - `slow_call_duration_threshold(60s)`：只是抬高慢调用判定线
///   （limiteron src/circuit/types.rs:581 `elapsed >= duration_threshold`），
///   >60s 的成功调用仍计慢；
/// - `slow_call_rate_threshold(1.0)`：types.rs:603 `rate >= rate_threshold`，
///   慢调用路径仅在「窗口内 100% 调用皆慢」时可能触发；混合快慢流量下
///   该路径失活，熔断由真实失败（failure_threshold）驱动。
///
/// 与 recId=4（超时预算）的自洽性：W2 落地后非流式/TTFT 默认 60s 超时
/// 使成功调用恒 <60s（不计慢），>60s 调用即超时失败走 failure_threshold
/// 路径；但本修复先于 W2，且运维可调大 timeout_ms，rate=1.0 是必要防线。
/// 残余触发场景「窗口内 100% 调用皆 >60s 且全成功」在 W2 默认值下与
/// 失败路径触发条件重合，行为一致。
#[cfg(feature = "resilience")]
pub(crate) fn nexusflow_circuit_config(open_timeout: std::time::Duration) -> CircuitBreakerConfig {
    CircuitBreakerConfig::new(5, 3, open_timeout)
        .slow_call_duration_threshold(Duration::from_secs(60))
        .slow_call_rate_threshold(1.0)
        .error_classifier(Arc::new(GateErrorClassifier))
}

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
            // recId=5：并发上限可配置（缺省沿用队列容量）；流式 permit
            // 生命周期修复后 permit 真实持有至流结束，该上限对流式生效
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
                // recId=2：limiteron 三态机（默认 half_open_max_calls=3，
                // 探针计数在每次状态转换统一清零——limiteron
                // src/circuit/types.rs finalize_transition），无手写版
                // "熔断一次即永久拒绝"的死锁。
                // recId=1：构造统一收敛至 nexusflow_circuit_config()
                //（慢调用阈值修复见该函数 doc）；Open 窗口经
                // [resilience].circuit_open_timeout_ms 可配，缺省 30s
                // 与历史硬编码逐位一致。
                nexusflow_circuit_config(circuit_open_timeout),
            )),
            #[cfg(feature = "cost-tracking")]
            cost_tracker,
        }
    }
}

#[derive(Clone)]
pub struct VendorManager {
    config: Arc<ConfigManager>,
    instances: Arc<RwLock<std::collections::HashMap<String, Arc<VendorInstance>>>>,
    client: reqwest::Client,
    openai_converter: Arc<OpenAIConverter>,
    anthropic_converter: Arc<AnthropicConverter>,
    metrics: Arc<GlobalMetrics>,
    #[cfg(feature = "resilience")]
    circuit_breakers: Arc<RwLock<std::collections::HashMap<String, Arc<CircuitBreaker>>>>,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
}

/// recId=4（W2）：TCP 连接建立超时 — Client 级共享常量。此前出站 Client
/// 零超时（`reqwest::Client::new()`），对端不响应时连接阶段可无限挂起。
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// recId=4（W2）：`execute_request` 外层的排队等待预算 — 覆盖「入队 →
/// worker 取走 → RPS/并发限流放行」的本地排队段。执行段超时由
/// `VendorConfig.timeout_ms`（per-attempt，从 send 起算）承担，两者解耦：
/// 总预算 = 排队预算 + 执行预算（timeout_ms=0 时不加执行段）。
const QUEUE_WAIT_BUDGET: Duration = Duration::from_secs(60);

impl VendorManager {
    pub fn new(config: Arc<ConfigManager>) -> Self {
        Self {
            config,
            instances: Arc::new(RwLock::new(std::collections::HashMap::new())),
            // recId=4（W2）：连接阶段 10s 超时（此前 Client 零超时）
            client: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("reqwest Client build failed"),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            metrics: Arc::new(GlobalMetrics::new()),
            #[cfg(feature = "resilience")]
            circuit_breakers: Arc::new(RwLock::new(std::collections::HashMap::new())),
            #[cfg(feature = "cost-tracking")]
            cost_tracker: Arc::new(CostTracker::new()),
        }
    }

    /// 热读厂商配置 — 执行路径统一入口。
    /// vendor 已从配置中移除时返回 Err（该请求以 VendorUnavailable 拒绝）。
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

    /// recId=8：`[routing].ewma_tau_ms` 热同步到 metrics — 每次延迟道落账前
    /// 刷新，POST /admin/config 更新后下一次采样即用新 τ（GlobalMetrics
    /// 不持 ConfigManager，由本辅助桥接热配置）。
    fn sync_ewma_tau(&self) {
        self.metrics
            .set_ewma_tau_ms(self.config.read().routing.ewma_tau_ms);
    }

    /// 全局成本追踪器访问器 — 用于 /admin/cost 端点。
    /// 仅在启用 `cost-tracking` feature 时可用。
    #[cfg(feature = "cost-tracking")]
    pub fn cost_tracker(&self) -> &Arc<CostTracker> {
        &self.cost_tracker
    }

    /// 读取 vendor 的 CircuitBreaker 当前状态（用于运维观测/测试断言）。
    /// 未启用 resilience feature 或 vendor 未初始化时返回 `None`。
    ///
    /// recId=2：迁移为 `limiteron::circuit::CircuitBreaker::get_state`。
    /// 路由过滤不调用本方法（async 不扩散到 router）——熔断状态由
    /// worker 完成分支同步回写 VendorState（见 process_vendor_queue），
    /// router 的 `== Available` 过滤借同步状态自动生效。
    #[cfg(feature = "resilience")]
    pub async fn get_circuit_state(&self, vendor_id: &str) -> Option<CircuitState> {
        let cb = {
            let guard = self.circuit_breakers.read().unwrap();
            guard.get(vendor_id).cloned()
        }?;
        Some(cb.get_state().await)
    }

    /// recId=2：vendor 的 CircuitBreaker 句柄（instances 未初始化时 None）。
    /// 仅供 cover_all_paths 覆盖路径取用；路由过滤不读 CB（见 get_status doc）。
    #[cfg(feature = "resilience")]
    fn circuit_breaker_for(&self, vendor_id: &str) -> Option<Arc<CircuitBreaker>> {
        let guard = self.instances.read().unwrap();
        guard.get(vendor_id).map(|i| i.circuit_breaker.clone())
    }

    pub async fn initialize(&self) -> Result<(), GateError> {
        let vendors = self.config.get_enabled_vendors();
        for vendor in &vendors {
            self.get_or_create_instance(vendor);
        }

        // recId=5：原「后台 reset worker」（每 60s 轮询 should_reset/reset）已删除
        // —— Token/Interval 窗口迁移 limiteron FixedWindowLimiter 后由
        // check_and_reset_window 惰性重置（窗口翻转在下一次 allow/peek 时自动完成），
        // 轮询重置机制整体退役。

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

        // 同步注册到 manager 级 CB map（cfg-gated）
        #[cfg(feature = "resilience")]
        {
            let cb = instance.circuit_breaker.clone();
            self.circuit_breakers
                .write()
                .unwrap()
                .insert(vendor.id.clone(), cb);
        }

        // Spawn worker
        let instance_clone = instance.clone();
        let client_clone = self.client.clone();
        let openai = self.openai_converter.clone();
        let anthropic = self.anthropic_converter.clone();
        let config_manager = self.config.clone();
        // recId=4③：metrics 下沉到 worker — 转换路径（execute_request 经队列）
        // 此前零指标，全仓唯一生产记账点在透传非流式
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
            // recId=5⑤ 顺序对调：先从队列取请求、再等 RPS 令牌。
            // 旧顺序（先 acquire 后 recv）在空闲期空烧 RPS 令牌，突发期
            // 请求还要平白忍受最长 100ms 的轮询间隔。
            let pending = match instance.queue.recv().await {
                Some(pending) => pending,
                None => break,
            };

            // Wait for rate limiter (RPS)
            instance.rate_limiter.acquire().await;

            // Check Advanced Rate Limiter（非消费 peek）
            if !instance.advanced_limiter.check().await {
                // 路由状态不在此回写（评审 blocker）：Token 耗尽的
                // Throttled 判定已由 get_status 的实时 peek 承担，回写
                // 持久状态会造成「路由排除→无请求→窗口重置后仍被排除」
                let _ = pending.response_tx.send(Err(GateError::RateLimited {
                    vendor: instance.vendor_id.clone(),
                }));
                continue;
            }

            // 3. Acquire concurrency permit
            // recId=18：token_weighted 开启时按预估 token 量计费
            let permit_result = match instance.ticket {
                Some(policy) => {
                    let cost = estimate_cost_unified(&pending.request, &policy);
                    instance.concurrency_limiter.acquire_weighted(cost).await
                }
                None => instance.concurrency_limiter.acquire().await,
            };

            // 4. 热读新鲜配置（recId=13：config.toml 热更新立即生效；
            //    vendor 已被移除时以 VendorUnavailable 拒绝该请求）
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

            let client = client.clone();
            let instance = instance.clone();
            let openai = openai.clone();
            let anthropic = anthropic.clone();
            let config_manager = config_manager.clone();
            let metrics = metrics.clone();

            tokio::spawn(async move {
                // Hold permit
                let _permit = permit_result;

                // recId=7④：[retry] 段热读 —— 每请求现读，POST /admin/config
                // 更新后下一次重试循环即用新参数
                let retry_cfg = config_manager.read().retry.clone();

                let result = Self::perform_http_request(
                    &client,
                    &config,
                    pending.request,
                    &openai,
                    &anthropic,
                    &retry_cfg,
                    #[cfg(feature = "resilience")]
                    &instance.circuit_breaker,
                    &metrics,
                )
                .await;

                // 执行结局记账（观测信号）：成功清错误计数；失败累加错误计数。
                // 路由状态不在此回写（评审 blocker 实测复现的恢复死锁）：回写
                // mark_unavailable/mark_throttled 会让 router 的 ==Available 过滤
                // 永久排除该 vendor，而恢复通路（探针/窗口重置）都要求有请求先
                // 到达 —— 恶性循环。熔断/限流判定已全部实时化于 get_status。
                //
                // recId=4③：metrics 同点记账 —— 延迟口径 = enqueued_at 起算
                // （排队 + 限流 + HTTP 全程），成功/失败各记一次 record_request，
                // 失败另记 record_error。此前转换路径零指标。
                //
                // recId=8（口径分道）：本点 duration 是「排队+执行全程」口径，
                // 落 e2e 道；排队段（入队 → worker 取走）单列 queue 道。
                // 成败都记（与 record_request 同拍，不影响成败计数）。
                metrics.set_ewma_tau_ms(config_manager.read().routing.ewma_tau_ms);
                let duration = pending.enqueued_at.elapsed();
                let lane_now = Instant::now();
                if let Some(dequeued_at) = pending.dequeued_at {
                    metrics.record_queue(
                        &instance.vendor_id,
                        dequeued_at.saturating_duration_since(pending.enqueued_at),
                        lane_now,
                    );
                }
                metrics.record_e2e(&instance.vendor_id, duration, lane_now);
                match &result {
                    Ok(_) => {
                        instance.state.mark_available();
                        instance.state.reset_error();
                        metrics.record_request(&instance.vendor_id, true, duration);
                        // recId=7（W5）：失败率滑窗双记（纯观测，常开）
                        metrics.record_vendor_outcome_at(&instance.vendor_id, true, Instant::now());
                    }
                    Err(e) => {
                        instance.state.inc_error();
                        metrics.record_request(&instance.vendor_id, false, duration);
                        metrics.record_error(&instance.vendor_id, &e.to_string());
                        // recId=7（W5）：失败率滑窗双记 —— 失败（含 429）计入
                        // failures。429 感知：该计数独立于 CB 分类器
                        //（error_map.rs 对 RateLimited 不计 CB 失败），
                        // 不影响 CB 自身的 failure_count。
                        metrics.record_vendor_outcome_at(
                            &instance.vendor_id,
                            false,
                            Instant::now(),
                        );
                        // recId=9（W5）：重试维度失败计数（守门器 r 的分母）—
                        // 仅转换路径（有 per-vendor 重试循环）的记账点累加，
                        // 与 retried_attempts 同路径口径；透传/流式无重试
                        // 循环，不计入。
                        metrics.record_failed_request_before_retry();
                    }
                }

                // 成功路径：原子扣减 token 使用统计（recId=5：CAS 全有全无，
                // 剩余不足时拒绝记账不超卖）+ 累加成本。
                // 仅 Token 变体记账（评审 major）：Interval/TimeWindow/None 无
                // 配额语义，跳过以免误导性 underflow 告警。
                if let Ok(ref response) = result {
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

                    // cost-tracking：按 (vendor, model) 计算 USD 并累加。
                    // pricing 读取点改为热配置（recId=13）：每次计算时从
                    // ConfigManager 现读 pricing 构建查找表，热更新的单价
                    // 立即生效；表规模为条目级（<100），每请求一次构建开销可忽略。
                    // 解析失败时 calculate 返回 0.0（CostTracker 静默 no-op）
                    #[cfg(feature = "cost-tracking")]
                    {
                        let calculator =
                            CostCalculator::from_config(&config_manager.read().pricing);
                        let cost_usd = calculator.calculate(
                            &instance.vendor_id,
                            &response.model,
                            response.usage.prompt_tokens,
                            response.usage.completion_tokens,
                        );
                        instance.cost_tracker.record(
                            &instance.vendor_id,
                            &response.model,
                            cost_usd,
                        );
                    }
                }

                let _ = pending.response_tx.send(result);
            });
        }
    }

    /// （`--all-features` 下含 cfg(resilience) 门控的 circuit_breaker 共 8 参，
    /// 超 clippy 默认阈值 7——转换器双向入参为协议转换所需，不打包重构。）
    #[allow(clippy::too_many_arguments)]
    async fn perform_http_request(
        client: &reqwest::Client,
        config: &VendorConfig,
        request: UnifiedRequest,
        openai: &OpenAIConverter,
        anthropic: &AnthropicConverter,
        retry_cfg: &RetrySection,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker>,
        metrics: &GlobalMetrics,
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

        // Convert UnifiedRequest to Vendor Request (JSON)
        let request_body = converter.request_to_external(request)?;

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
        )
        .await
    }

    /// 解析上游 `Retry-After` 响应头（recId=7③）。
    /// 仅支持秒数形式（RFC 7231 的 delta-seconds）；HTTP-date 形式降级 None
    /// （自管薄循环不值得为此引入日历解析）。
    fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
        let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
        value.trim().parse::<u64>().ok().map(Duration::from_secs)
    }

    /// 构建 vendor API URL（统一 4 处重复的 URL 拼接逻辑）。
    /// recId=15：path 可变版（chat/completions / messages / embeddings）。
    fn build_vendor_url_at(config: &VendorConfig, path: &str) -> Result<String, GateError> {
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

    /// 构建 vendor API URL（chat/messages 默认端点 — 既有调用点语义不变）
    fn build_vendor_url(config: &VendorConfig) -> Result<String, GateError> {
        let path = if config.type_ == "anthropic" {
            "messages"
        } else {
            "chat/completions"
        };
        Self::build_vendor_url_at(config, path)
    }

    /// 获取 API key 并构建带认证 header 的 RequestBuilder
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

        Ok(builder)
    }

    /// recId=4（W2）：非流式 per-attempt 总超时 — 从 `send` 发起起算至响应
    /// 体读完（reqwest 请求级超时语义），每次重试重建 builder 时各自附加，
    /// 不含本地排队等待（排队预算见 [`QUEUE_WAIT_BUDGET`]）。
    ///
    /// `timeout_ms == 0` = 不限时：显式跳过包裹（`Duration::from_millis(0)`
    /// 的语义是「立即超时」，不能把零值直接透传给 reqwest）。
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

    /// recId=4（W2）：出站错误分类 — reqwest 超时错误映射 `Timeout`
    /// （客户端可识别 504 语义），其余网络错误保持 `VendorUnavailable`。
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

    /// 执行带重试的 HTTP 请求。
    ///
    /// 启用 `resilience` feature 时由 `limiteron::circuit::CircuitBreaker`
    /// 包装（recId=2）：
    /// - Closed/HalfOpen 状态下放行至内部重试逻辑；
    /// - Open（含 half-open 准入超限）状态下直接拒绝，经
    ///   [`from_limiteron`] 还原为 `VendorUnavailable{... (circuit open)}`，
    ///   无 HTTP 调用；
    /// - 执行路径错误是 GateError，经 [`to_limiteron`] 编码进库签名
    ///   要求的 `Result<T, LimiteronError>`，计失败与否由
    ///   [`GateErrorClassifier`] 按 variant 语义判定。
    async fn execute_http_with_retry(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
        retry_cfg: &RetrySection,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker>,
        metrics: &GlobalMetrics,
    ) -> Result<UnifiedResponse, GateError> {
        #[cfg(feature = "resilience")]
        {
            let vendor_id = config.id.clone();
            let result = circuit_breaker
                .execute(|| async {
                    Self::execute_http_with_retry_inner(
                        client,
                        config,
                        request_body.clone(),
                        converter,
                        retry_cfg,
                        metrics,
                    )
                    .await
                    .map_err(|e| to_limiteron(&e))
                })
                .await;
            result.map_err(|e| from_circuit_call(e, &vendor_id))
        }
        #[cfg(not(feature = "resilience"))]
        {
            let _ = retry_cfg;
            Self::execute_http_with_retry_inner(
                client,
                config,
                request_body,
                converter,
                retry_cfg,
                metrics,
            )
            .await
        }
    }

    /// HTTP 请求 + 自管重试薄循环（recId=7）。
    /// 在 `resilience` feature 启用时由 `execute_http_with_retry` 经 CB 包装后调用。
    ///
    /// recId=7 变更：
    /// - 延迟计算迁 `limiteron::retry::delay_for_attempt`（经
    ///   `retry_delay_for` 纯函数），修掉旧实现「先自增再取指数」的
    ///   2s 起步偏移，且 429 退避叠加 jitter；
    /// - 429 优先采用上游 `Retry-After` 头；
    /// - 可重试状态码集合与 max_retries 热读 Config `[retry]` 段
    ///   （POST /admin/config 更新后立即生效）。
    ///
    /// 注：不走 `RetryPolicy::execute`（limiteron src/retry.rs:147 签名
    /// 无 per-attempt 延迟注入点），Retry-After 分支需要自管薄循环。
    async fn execute_http_with_retry_inner(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
        retry_cfg: &RetrySection,
        metrics: &GlobalMetrics,
    ) -> Result<UnifiedResponse, GateError> {
        let url = Self::build_vendor_url(config)?;
        let policy = RetryDelayPolicy::from_config(retry_cfg);

        // recId=9（W5）：重试总时间预算（默认 0 = 不限时，行为与现状一致）。
        // 锚点统一 tokio::time::Instant —— std Instant 不随 paused clock
        // 推进，paused-clock 测试与生产同走 tokio 时钟域。
        let budget = if retry_cfg.retry_budget_secs > 0 {
            Some(Duration::from_secs(retry_cfg.retry_budget_secs))
        } else {
            None
        };
        let budget_anchor = tokio::time::Instant::now();

        // attempt = 已执行的重试次数（首次调用为 0）
        let mut attempt = 0u32;

        loop {
            // 构建带认证的请求（recId=4：per-attempt 总超时，从 send 起算）
            let builder = Self::build_authed_request(client, config, &url, true)?;
            let builder = Self::apply_attempt_timeout(builder, config);

            // 发送请求
            let response = match builder.json(&request_body).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    // 网络错误 - 判断是否可重试（RetryableShort：最多 2 次）
                    let is_transient = e.is_timeout() || e.is_connect();
                    if is_transient && attempt < ErrorCategory::RetryableShort.max_retries() {
                        attempt += 1;
                        // recId=9（W5）：重试维度计数（守门器 r 的分子）
                        metrics.record_retry_attempt();
                        // recId=5：RNG 注入（RetryableShort 分支不消耗 RNG，
                        // 签名统一）；临时 ThreadRng 不跨 await
                        let delay = retry_delay_for(
                            ErrorCategory::RetryableShort,
                            attempt,
                            None,
                            &policy,
                            &mut rand::thread_rng(),
                        );
                        // recId=9（W5）：预算超限放弃 —— 返回最后错误
                        //（此处即网络错误的 VendorUnavailable 映射）
                        if let Some(b) = budget {
                            if crate::utils::retry::budget_exceeded(budget_anchor, b, delay) {
                                return Err(GateError::VendorUnavailable {
                                    vendor: config.id.clone(),
                                });
                            }
                        }
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(GateError::VendorUnavailable {
                        vendor: config.id.clone(),
                    });
                }
            };

            let status = response.status();

            // 判断响应状态并决定是否重试
            if !status.is_success() {
                // Retry-After 头须在消费 body 前读取
                let retry_after = Self::parse_retry_after(response.headers());
                let text = response.text().await.unwrap_or_default();
                tracing::warn!(
                    vendor = %config.id,
                    status = %status,
                    body = %text,
                    "{}",
                    crate::i18n::t("vendor-upstream-non-success")
                );
                let error = GateError::VendorUnavailable {
                    vendor: crate::i18n::t_args(
                        "vendor-upstream-status",
                        &[
                            ("vendor", config.id.clone()),
                            ("status", status.to_string()),
                        ],
                    ),
                };

                // 可重试集合进配置：状态码在集合内才参与分类（集合外一律 Fatal）
                let category = if retry_cfg.retryable_status_codes.contains(&status.as_u16()) {
                    ErrorCategory::from_status_code(status.as_u16())
                } else {
                    ErrorCategory::Fatal
                };

                if category.is_retryable() && attempt < retry_cfg.max_retries {
                    attempt += 1;
                    // recId=9（W5）：重试维度计数（守门器 r 的分子）
                    metrics.record_retry_attempt();
                    // recId=5：Retry-After / 5xx 分支全范围展宽（RNG 注入，
                    // 临时 ThreadRng 不跨 await）
                    let delay = retry_delay_for(
                        category,
                        attempt,
                        retry_after,
                        &policy,
                        &mut rand::thread_rng(),
                    );
                    // recId=9（W5）：预算超限放弃 —— 返回最后错误（保持其
                    // 5xx/429 语义）
                    if let Some(b) = budget {
                        if crate::utils::retry::budget_exceeded(budget_anchor, b, delay) {
                            return Err(error);
                        }
                    }
                    tokio::time::sleep(delay).await;
                    continue;
                }

                return Err(error);
            }

            // 解析响应（recId=4：per-attempt 超时在 body 读取阶段触发时
            // 映射 Timeout，与真正的解析失败区分）
            let response_json: Value = match response.json().await {
                Ok(json) => json,
                Err(e) if e.is_timeout() => {
                    return Err(GateError::Timeout {
                        vendor: config.id.clone(),
                    });
                }
                Err(e) => {
                    return Err(GateError::VendorUnavailable {
                        vendor: crate::i18n::t_args(
                            "vendor-parse-error",
                            &[("error", e.to_string())],
                        ),
                    });
                }
            };

            // 转换为统一响应格式
            return converter.parse_response(response_json);
        }
    }

    /// 读取 vendor 的实时路由状态（recId=5② async 化；评审 blocker 后全面
    /// 实时化）。
    ///
    /// 判定优先级（全部实时，不读持久化的熔断/限流回写）：
    /// 1. config enabled=false → Unavailable
    /// 2. CB Open 且超时未过 → Unavailable；Open 已超时或 HalfOpen → 放行
    ///    （探测流量是唯一的恢复通路：limiteron 的 Open→HalfOpen 转换与
    ///    探针准入都发生在 execute() 内部，路由若排除则探针永远无人触发；
    ///    评审探针实测：排除状态下真实睡眠 31s 越过 CB 超时后 route 仍
    ///    0/30 选中，直发一次成功后才恢复）
    /// 3. Token 窗口 peek 不过 → Throttled（窗口惰性重置后本判定自动恢复）
    /// 4. 其余 → 持久 VendorState（mark_busy/update_status 等人工运维态）
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
            match instance.circuit_breaker.get_state().await {
                CircuitState::Open => {
                    if !Self::circuit_probe_ready(&instance.circuit_breaker).await {
                        return VendorStatus::Unavailable;
                    }
                    // 超时已过：放行探测流量；真实的 Open→HalfOpen 转换将由
                    // 该请求在 CB execute 内完成（limiteron src/circuit/types.rs:396-433）
                }
                // 半开 = 恢复探测窗口：放行探针（并发准入由 CB 的
                // half_open_max_calls 控制），探针失败会立即重新熔断
                CircuitState::HalfOpen => {}
                CircuitState::Closed => {}
            }
        }

        // Check advanced limiter（非消费 peek）
        if !instance.advanced_limiter.check().await {
            return VendorStatus::Throttled;
        }
        instance.state.get_status()
    }

    /// CB Open 是否已过超时窗口、可放行探测（评审 blocker 修复）。
    ///
    /// limiteron 的状态转换只发生在 execute() 内部，此处做只读判定：
    /// 用 `get_stats()` 的墙钟失败时间（limiteron src/error/mod.rs:176，
    /// `chrono::DateTime<Utc>`，与 lib clock 域无关）对齐库内转换条件
    /// `duration_since(last_failure) >= timeout`（limiteron
    /// src/circuit/types.rs:397）。Open 而无失败时间戳（异常态）时对齐
    /// 库内保守拒绝语义（types.rs:420-428）。
    #[cfg(feature = "resilience")]
    async fn circuit_probe_ready(circuit_breaker: &Arc<CircuitBreaker>) -> bool {
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

    /// recId=4①：直读 vendor 实例的 VendorQueue（async-channel bounded，
    /// `len()` 即 receiver 深度）。此前读无人写入的 QueueManager，恒 0，
    /// queue_factor 成为死因子、3D 权重退化 2D。
    pub fn get_queue_length(&self, vendor_id: &str) -> usize {
        let guard = self.instances.read().unwrap();
        guard.get(vendor_id).map(|i| i.queue.len()).unwrap_or(0)
    }

    /// recId=6：vendor 当前在途请求数（并发信号量已发放未释放的 permit 数，
    /// 流式 permit 随流存活故对长流真实生效）。镜像 [`Self::get_queue_length`]
    /// 的锁模式；实例未初始化时返回 0。
    pub fn get_in_flight(&self, vendor_id: &str) -> usize {
        let guard = self.instances.read().unwrap();
        guard
            .get(vendor_id)
            .map(|i| i.concurrency_limiter.in_flight())
            .unwrap_or(0)
    }

    /// recId=16：舰队并发占用信号 — (Σ in_flight, Σ 并发上限)，准入的
    /// 拥塞判定输入（Σ in_flight 复用 [`ConcurrencyLimiter::in_flight`]，
    /// 流式 permit 随流存活故对长流真实）。含全部已初始化实例；容量
    /// 合计 0 的退化（无实例）返回 (0, 0)，调用方按无拥塞处理。
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

    /// recId=16：直接获取 vendor 并发 permit — 准入集成的测试钩子（占满
    /// 并发驱动拥塞信号）与运维排障入口；生产执行路径走 worker 的
    /// acquire。实例不存在返回 None。
    pub async fn acquire_permit(&self, vendor_id: &str) -> Option<ConcurrencyPermit> {
        // 先克隆实例句柄并释放读锁，await 不持锁（clippy await_holding_lock）
        let inst = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id).cloned()?
        };
        Some(inst.concurrency_limiter.acquire().await)
    }

    /// recId=6：全部已初始化实例的在途均值 — `load_signal =
    /// "in_flight_relative"` 的归一化基准。fleet = instances 表（initialize
    /// 时为 enabled vendor 构建，与 get_queue_length 同一观测域）。
    /// 空表返回 0.0（消费侧 relative 公式分母 max(1, mean) 钳制兜底）。
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

        // Remove direct rate limiter check, as it's handled by worker

        let (response_tx, response_rx) = oneshot::channel();

        let pending = PendingRequest {
            request,
            response_tx,
            // recId=4③：延迟口径含排队时间（worker 完成分支以此记账）
            enqueued_at: Instant::now(),
            // recId=8：出队时刻由 VendorQueue::recv 单点盖戳，入队方恒 None
            dequeued_at: None,
        };

        instance.queue.send(pending).await?;

        // recId=4（W2）：排队预算与执行预算解耦 —— 此前 60s 一刀切地覆盖
        // 「排队 + 执行」全程，排队饥饿与长执行互相挤占预算。现改为
        // QUEUE_WAIT_BUDGET（排队段）+ timeout_ms（执行段，per-attempt 从
        // send 起算）；timeout_ms=0（不限时）时不加执行段，仅剩排队预算。
        // timeout_ms 热读（vendor 配置热更新立即生效）。
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

    /// recId=14①③：流式启动状态码校验 + 错误记账。
    ///
    /// 流式路径此前不查上游状态码：非 2xx 时错误 JSON 正文会被当作 SSE 流
    /// 原样透传给客户端（伪 200 流），客户端与重试逻辑无法分辨成败。
    /// 此处对齐非流式路径（`execute_http_with_retry_inner`）的既有写法：
    /// 读 body 记 warn 转 `GateError`，经 error.rs 固定 IntoResponse
    /// 映射 —— 429 → `RateLimited`（映射 429，客户端可退避重试），
    /// 其余非 2xx → `VendorUnavailable`（映射 503）。
    /// 错误分支同步补 metrics（此前流式路径零指标，全仓唯一生产调用点
    /// 在透传非流式）。
    async fn check_stream_status(
        &self,
        vendor_id: &str,
        start: Instant,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, GateError> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        tracing::warn!(
            vendor = %vendor_id,
            status = %status,
            body = %text,
            "{}",
            crate::i18n::t("vendor-stream-upstream-non-success")
        );
        let duration = start.elapsed();
        self.metrics.record_request(vendor_id, false, duration);
        // recId=7（W5）：失败率滑窗双记 —— 上游 429 → RateLimited 亦计入
        // failures（429 感知：独立于 CB 分类器，不影响 CB failure_count）
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

    /// recId=2（二期）：流式单次派发 = send（含 TTFT 超时，recId=4）→
    /// 响应头状态校验（recId=14①）。两条流式路径（[`Self::execute_stream_request`]
    /// / [`Self::execute_passthrough_stream_request`]）的公共派发段。
    ///
    /// cover_all_paths 开启时由 per-vendor CircuitBreaker 包裹：Open 直接
    /// 拒绝（无 HTTP），失败（非 2xx/超时）计 CB 失败；关闭时直连，行为
    /// 与历史逐位一致。
    ///
    /// 时长语义：只计到响应头判定（[`Self::check_stream_status`]）为止，
    /// 流建立后的 chunk 传输不在 CB 调用内 —— 与「首字节前才 failover」
    /// 边界一致（OpenRouter 语义）。
    ///
    /// `builder` 在 CB 之外构建（构建失败的 Config 错误不计 CB 失败，与
    /// 既有放置位置一致）；`include_content_type` 差异（转换流 true /
    /// 透传流 false）由调用方构建 builder 时保留。
    async fn dispatch_stream_once(
        &self,
        vendor_id: &str,
        config: &VendorConfig,
        builder: reqwest::RequestBuilder,
        request_body: &Value,
        start: Instant,
    ) -> Result<reqwest::Response, GateError> {
        let send = || async {
            let response = if config.stream_ttft_ms > 0 {
                // recId=4（W2）：流式 TTFT 超时 — 包住 `send` 发起至上游
                // 响应头返回的段落；流建立前超时以 Err(Timeout) 返回。
                // reqwest 0.13 RequestBuilder 无 Clone（try_clone：body 为
                // 流时 None）—— 此处 body 尚未附加，必为 Some
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
            // recId=14①：非 2xx 上游不再以 200 正文伪装成 SSE 流
            self.check_stream_status(vendor_id, start, response).await
        };

        #[cfg(feature = "resilience")]
        {
            if self.config.read().resilience.cover_all_paths {
                if let Some(circuit_breaker) = self.circuit_breaker_for(vendor_id) {
                    let result = circuit_breaker
                        .execute(|| async { send().await.map_err(|e| to_limiteron(&e)) })
                        .await;
                    return result.map_err(|e| from_circuit_call(e, vendor_id));
                }
            }
        }
        send().await
    }

    /// 执行流式请求（recId=3 阶段二）。
    ///
    /// 返回上游 SSE 事件流经增量分帧与反向解析后的 IR
    /// [`StreamChunk`] 流 —— 不再原样返回 `bytes_stream()`（旧实现跨协议
    /// 流式必然失效且无跨块帧缓冲）：
    /// - send 后先校验上游状态码（recId=14①，见 [`Self::check_stream_status`]）；
    /// - 字节流经 `SseFrameParser` 分帧（跨块缓冲 + 增量 UTF-8）；
    /// - 每帧按 vendor 协议经 `ProtocolConverter::parse_stream_frame`
    ///   解析为 IR chunk（无法映射的帧跳过，`[DONE]` 提前收流）；
    /// - 上游 `event: error` / 错误 JSON 帧 → 流中产出
    ///   `Err(VendorUnavailable)`（recId=14②），api 层转为带语义的
    ///   SSE error 事件提前终止；
    /// - 出站编码由 api 层经存量 `stream_chunk_to_external` 完成。
    ///
    /// recId=5④：permit move 进返回的 Stream（unfold 持有），流结束才释放
    /// —— 并发上限对流式请求真实生效。
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

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // Enforce limits manually for stream requests
        instance.rate_limiter.acquire().await;
        // recId=18：token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_unified(&request, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };

        // 热读新鲜配置（recId=13）：api_base/api_key_env 热更新立即生效
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url(&config)?;

        // recId=3：converter 以 Arc 进流 —— 流式反向解析
        // （parse_stream_frame）按 vendor 协议分派，生命周期与流一致
        let converter: Arc<dyn ProtocolConverter> = match config.type_.as_str() {
            "openai" => self.openai_converter.clone(),
            "anthropic" => self.anthropic_converter.clone(),
            _ => {
                return Err(GateError::Config {
                    message: crate::i18n::t("vendor-unknown-type"),
                })
            }
        };

        let request_body = converter.request_to_external(request)?;

        let request_builder = Self::build_authed_request(&self.client, &config, &url, true)?;

        let start = Instant::now();
        // recId=2（二期）：cover_all_paths 开启时 send + 状态校验段由
        // CircuitBreaker 包裹（时长只计到响应头，与首字节边界一致）；
        // 关闭时直连，行为与历史逐位一致
        let response = self
            .dispatch_stream_once(vendor_id, &config, request_builder, &request_body, start)
            .await?;

        // recId=4④：流式口径为「派发处计」——send 成功（2xx、流已建立）即记
        // 成功，错误分支已在 check_stream_status 内记账。流中错误在 api 层以
        // SSE error 事件终止，属长流生命周期，不回溯改写派发计数（Drop 计有
        // panic 不安全风险，弃用；口径见 recId=4 方案说明）。
        self.metrics
            .record_request(vendor_id, true, start.elapsed());
        // recId=7（W5）：失败率滑窗双记（send 成功记 total，纯观测，常开）
        self.metrics
            .record_vendor_outcome_at(vendor_id, true, Instant::now());
        // recId=8（口径分道）：本点 duration 是 send→响应头口径，落 TTFT 道
        //（严格首 token 口径网关侧不可得，响应头近似——config.toml 有如实标注）
        self.sync_ewma_tau();
        self.metrics
            .record_ttft(vendor_id, start.elapsed(), Instant::now());

        // 字节流：permit move 进 unfold，流结束才释放（recId=5④）。
        // recId=4（W2）：chunk 间 idle 超时 — bytes.next() 按 0-约定包
        // timeout(stream_idle_ms)，超时产出 Err(Timeout) 并由下游分帧层
        // 终止流（permit 随终止释放）；item 错误类型统一为 GateError。
        let idle = Duration::from_millis(config.stream_idle_ms);
        let vendor_template = vendor_id.to_string();
        let inner = Box::pin(response.bytes_stream());
        let byte_stream = futures::stream::unfold((inner, permit), move |(mut inner, permit)| {
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
        });

        // 分帧 + 反向解析：上游 SSE → IR StreamChunk（recId=3 阶段二）
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
                            // recId=14②：上游中流错误 → 语义化 Err 提前终止
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
                            // recId=4（W2）：上游错误已统一为 GateError ——
                            // idle 超时（Timeout）原样透传（api 层转 SSE error
                            // 事件终止）；其余 IO 错误保持既有包装语义
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
                            if let Some(frame) = parser.flush() {
                                if !frame.is_error() {
                                    if let Some(data) = frame.data {
                                        if data.trim() != "[DONE]" {
                                            if let Some(chunk) = converter.parse_stream_frame(&data)
                                            {
                                                return Some((
                                                    Ok(chunk),
                                                    (parser, bytes, true, vendor, converter),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            return None;
                        }
                    }
                }
            },
        );

        // Unfold 的 async 闭包非 Unpin —— Box::pin 后再返回，
        // 外层调用方（StreamExt::next）要求流自身 Unpin
        Ok(Box::pin(framed))
    }

    /// 执行零拷贝透传请求
    /// 当入站协议与出站协议相同时使用，避免序列化/反序列化开销。
    /// 默认端点按 vendor 协议分派（anthropic → `messages`、OpenAI →
    /// `chat/completions`）—— 与历史 `build_vendor_url` 行为一致
    ///（recId=15① 回归：常量默认会把 anthropic 透传打到错误路径）。
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

    /// recId=15①：治理路径的端点可指定版 — embeddings 等 OpenAI 兼容端点
    /// 经此获得与 chat 相同的共享连接池、RPS/并发限流、advanced limiter、
    /// token 记账与 metrics.record_request（旧 embeddings 实现每请求
    /// `reqwest::Client::new()` 绕过全部治理）。
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
        // recId=8（口径分道）：透传非流式为全程口径（含本地排队），落 e2e 道；
        // 成败都记（与下方 record_request 同拍）
        self.sync_ewma_tau();
        self.metrics.record_e2e(vendor_id, duration, Instant::now());
        match &result {
            Ok(_) => {
                self.metrics.record_request(vendor_id, true, duration);
                // recId=7（W5）：失败率滑窗双记（纯观测，常开）
                self.metrics
                    .record_vendor_outcome_at(vendor_id, true, Instant::now());
            }
            Err(e) => {
                self.metrics.record_request(vendor_id, false, duration);
                // recId=7（W5）：失败率滑窗双记 —— inner 的 advanced limiter
                // 早退（RateLimited）与上游非 2xx 均计入 failures（429 感知）
                self.metrics
                    .record_vendor_outcome_at(vendor_id, false, Instant::now());
                self.metrics.record_error(vendor_id, &e.to_string());
            }
        }
        result
    }

    /// recId=2（一期）：cover_all_paths 开启时，透传非流式由 per-vendor
    /// CircuitBreaker 包裹 —— 与 [`Self::execute_http_with_retry`] 同款
    /// `execute` + [`to_limiteron`]/[`from_limiteron`] 包装（CB 拒绝还原为
    /// `VendorUnavailable{vendor: "{id} (circuit open)"}`，其余错误往返
    /// 无损）；关闭时直连 inner，行为与历史逐位一致。
    async fn execute_passthrough_request_covered(
        &self,
        vendor_id: &str,
        request_body: Value,
        path: &str,
    ) -> Result<Value, GateError> {
        #[cfg(feature = "resilience")]
        {
            if self.config.read().resilience.cover_all_paths {
                if let Some(circuit_breaker) = self.circuit_breaker_for(vendor_id) {
                    let result = circuit_breaker
                        .execute(|| async {
                            self.execute_passthrough_request_inner(
                                vendor_id,
                                request_body.clone(),
                                path,
                            )
                            .await
                            .map_err(|e| to_limiteron(&e))
                        })
                        .await;
                    return result.map_err(|e| from_circuit_call(e, vendor_id));
                }
            }
        }
        self.execute_passthrough_request_inner(vendor_id, request_body, path)
            .await
    }

    async fn execute_passthrough_request_inner(
        &self,
        vendor_id: &str,
        request_body: Value,
        path: &str,
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

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // recId=18：token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_json(&request_body, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };
        let _permit = permit; // Hold permit during request

        // 热读新鲜配置（recId=13）
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url_at(&config, path)?;

        let builder = Self::build_authed_request(&self.client, &config, &url, true)?;
        // recId=4（W2）：per-attempt 总超时（从 send 起算；0=不限时）
        let builder = Self::apply_attempt_timeout(builder, &config);

        let response = match builder.json(&request_body).send().await {
            Ok(resp) => resp,
            // recId=4（W2）：超时映射 Timeout（客户端可识别 504 语义）
            Err(e) => return Err(Self::outbound_send_error(vendor_id, &e)),
        };

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(
                vendor = %vendor_id,
                status = %status,
                body = %text,
                "{}",
                crate::i18n::t("vendor-upstream-non-success")
            );
            return Err(GateError::VendorUnavailable {
                vendor: crate::i18n::t_args(
                    "vendor-upstream-status",
                    &[
                        ("vendor", vendor_id.to_string()),
                        ("status", status.to_string()),
                    ],
                ),
            });
        }

        // recId=4（W2）：per-attempt 超时在 body 读取阶段触发时映射 Timeout，
        // 与真正的解析失败区分
        let response_json: Value = match response.json().await {
            Ok(json) => json,
            Err(e) if e.is_timeout() => {
                return Err(GateError::Timeout {
                    vendor: vendor_id.to_string(),
                });
            }
            Err(e) => {
                return Err(GateError::VendorUnavailable {
                    vendor: crate::i18n::t_args("vendor-parse-error", &[("error", e.to_string())]),
                });
            }
        };

        // 更新 Token 使用统计（recId=5：CAS 全有全无，不足则拒绝记账）。
        // 仅 Token 变体记账（评审 major）：非 Token 变体无配额语义，
        // consume_tokens 恒 false 不应触发 underflow 告警。
        if let Some(usage) = response_json.get("usage") {
            if let Some(total) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
                if instance.advanced_limiter.accounts_tokens() {
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
            }
        }

        Ok(response_json)
    }

    /// 执行零拷贝透传流式请求
    ///
    /// recId=4（W2）：流错误类型由 `reqwest::Error` 统一为
    /// [`GateError`]（idle 超时以 `Timeout` 产出）；下游
    /// `reframe_passthrough_stream` 泛型于 `E: Display`，无需改动。
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

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check().await {
            return Err(GateError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // recId=18：token_weighted 开启时按预估 token 量计费
        let permit = match instance.ticket {
            Some(policy) => {
                let cost = estimate_cost_json(&request_body, &policy);
                instance.concurrency_limiter.acquire_weighted(cost).await
            }
            None => instance.concurrency_limiter.acquire().await,
        };

        // 热读新鲜配置（recId=13）
        let config = self.fresh_config(vendor_id)?;
        let url = Self::build_vendor_url(&config)?;

        let builder = Self::build_authed_request(&self.client, &config, &url, false)?;

        let start = Instant::now();
        // recId=2（二期）：TTFT 超时与状态校验段收拢于 dispatch_stream_once
        //（cover_all_paths 开启时由 CB 包裹，关闭时直连，行为逐位一致）
        let response = self
            .dispatch_stream_once(vendor_id, &config, builder, &request_body, start)
            .await?;

        // recId=4④：流式「派发处计」——send 成功即记成功，与转换流式同口径
        //（错误分支已在 check_stream_status 内记账）
        self.metrics
            .record_request(vendor_id, true, start.elapsed());
        // recId=7（W5）：失败率滑窗双记（send 成功记 total，纯观测，常开）
        self.metrics
            .record_vendor_outcome_at(vendor_id, true, Instant::now());
        // recId=8（口径分道）：send→响应头口径落 TTFT 道（响应头近似，同上）
        self.sync_ewma_tau();
        self.metrics
            .record_ttft(vendor_id, start.elapsed(), Instant::now());

        // recId=5④：与 execute_stream_request 同款 permit 生命周期 ——
        // permit move 进返回的 Stream，流结束才释放，并发上限对流式生效。
        // recId=4（W2）：chunk 间 idle 超时（0=不限时）与转换流式同口径。
        let idle = Duration::from_millis(config.stream_idle_ms);
        let vendor_template = vendor_id.to_string();
        let inner = Box::pin(response.bytes_stream());
        let stream = futures::stream::unfold((inner, permit), move |(mut inner, permit)| {
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
        });

        // Unfold 的 async 闭包非 Unpin —— Box::pin 后再返回，
        // 外层调用方（StreamExt::next）要求流自身 Unpin
        Ok(Box::pin(stream))
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
            // recId=4①：直读实例队列深度（同 get_queue_length）
            status.push((
                id.clone(),
                instance.state.get_status(),
                instance.queue.len(),
            ));
        }

        status
    }

    /// 获取厂商的窗口信息，用于路由权重计算（recId=5②：async 化）。
    ///
    /// Token/Interval 窗口经 `window_snapshot()`（FixedWindowLimiter peek）
    /// 取精确 remaining / reset_secs，替代旧实现的手读原子计数；
    /// TimeWindow 保留时段窗口语义。
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
                // recId=13：跨午夜判定统一委托 time_window_active（旧同日
                // 判定 now < *start || now >= *end 使 22:00–06:00 类配置
                // 整段失效，window_factor 恒按窗口外参与路由权重）。
                // 注：跨午夜窗口内的 window_end 精确「次日 end」时刻为
                // 进阶项，本轮不做——当前 else 分支按同日内插值，跨午夜时
                // signed_duration_since 为负 → to_std 失败 → 退化为
                // Instant::now()（window_factor 判「即将结束」减半，保守
                // 但不再是整段排除）。
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

#[cfg(test)]
mod tests {
    //! `WindowInfo` 单元测试 — 纯逻辑无 IO，覆盖三个公开方法的边界与典型场景。
    //!
    //! 测试设计原则（规则9）：每个用例断言具体值或具体行为，
    //! 而非"调用未 panic"。包括 None 分支、边界（恰好等于阈值）、
    //! 异常输入（max=0、used > max）和数值精度。

    use super::*;
    use std::time::Duration;

    // ============ is_ending_soon ============

    #[test]
    fn test_is_ending_soon_returns_false_when_no_window_end() {
        // 无结束时间 → 永久窗口，永远不会"即将结束"
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 0,
            window_end: None,
        };
        assert!(!info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_false_when_far_in_future() {
        // 窗口结束时间在远未来（10 分钟后）→ 不算即将结束
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: Some(Instant::now() + Duration::from_secs(600)),
        };
        assert!(!info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_true_when_within_5_minutes() {
        // 窗口剩余 60 秒（< 300 秒阈值）→ 即将结束
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: Some(Instant::now() + Duration::from_secs(60)),
        };
        assert!(info.is_ending_soon());
    }

    #[test]
    fn test_is_ending_soon_true_when_window_already_passed() {
        // 窗口结束时间已过去 → duration_since 饱和到 0，0 < 300 = true
        // 此为现有实现行为，断言之以锁定语义
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
        // max_tokens=None → 无限制，无论 used 多少都有预算
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 1_000_000,
            window_end: None,
        };
        assert!(info.has_budget());
    }

    #[test]
    fn test_has_budget_true_when_partial_usage() {
        // 500 < 1000 → true
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 500,
            window_end: None,
        };
        assert!(info.has_budget());
    }

    #[test]
    fn test_has_budget_false_when_exactly_at_limit() {
        // used == max → 严格小于失败 → false（边界）
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 1000,
            window_end: None,
        };
        assert!(!info.has_budget());
    }

    #[test]
    fn test_has_budget_false_when_over_limit() {
        // used > max → false
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
        // 无限制 → 满预算 1.0
        let info = WindowInfo {
            max_tokens: None,
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 1.0);
    }

    #[test]
    fn test_budget_percentage_one_when_nothing_used() {
        // max=Some(1000), used=0 → 1.0
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 1.0);
    }

    #[test]
    fn test_budget_percentage_half_when_half_used() {
        // used=500, max=1000 → 1 - 0.5 = 0.5
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
        // used=max → 1 - 1.0 = 0.0
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 1000,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_budget_percentage_clamped_to_zero_when_over_used() {
        // used > max → (1 - 2.0).max(0.0) = 0.0（不允许负数）
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 2000,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_budget_percentage_zero_when_max_is_zero() {
        // max_tokens=Some(0) → 防除零特判 → 0.0
        let info = WindowInfo {
            max_tokens: Some(0),
            used_tokens: 0,
            window_end: None,
        };
        assert_eq!(info.budget_percentage(), 0.0);
    }

    #[test]
    fn test_window_info_debug_clone_roundtrip() {
        // 验证 Debug + Clone 派生行为，确保 WindowInfo 可跨任务克隆传递
        let info = WindowInfo {
            max_tokens: Some(1000),
            used_tokens: 500,
            window_end: Some(Instant::now() + Duration::from_secs(60)),
        };
        let cloned = info.clone();
        assert_eq!(cloned.max_tokens, info.max_tokens);
        assert_eq!(cloned.used_tokens, info.used_tokens);
        // Debug 不 panic
        let _ = format!("{:?}", info);
    }

    // ======== recId=1：熔断器 slow-call-rate 误触发修复（生产构造证明） ========

    /// 生产构造证明：经 [`nexusflow_circuit_config`]（生产同款参数）建 CB，
    /// 3 次 800ms 成功调用必须保持 Closed。修复前（limiteron 默认
    /// duration=500ms / rate=0.5，../limiteron src/constants.rs:106/111）
    /// 单次 800ms 成功调用即 1/1 慢、rate 1.0 ≥ 0.5 → Open（Red 实测：
    /// 第 2 次 execute 已被 Open 拒绝），即 LLM 延迟形态误触发熔断。
    ///
    /// MockClock 无法放大 execute() 实测时长（limiteron src/circuit/types.rs
    /// 的 elapsed 取真实墙钟），真睡眠 3×800ms 是必要成本。
    #[cfg(feature = "resilience")]
    #[tokio::test]
    async fn slow_ok_calls_do_not_open_production_circuit() {
        let cb = CircuitBreaker::new(nexusflow_circuit_config(Duration::from_secs(30)));

        for i in 0..3 {
            // execute 的 Open 态拒绝以 CircuitCallError::Open 独立承载（rc.6）
            let result: Result<
                u32,
                limiteron::circuit::CircuitCallError<limiteron::error::LimiteronError>,
            > = cb
                .execute(|| async {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    Ok::<u32, limiteron::error::LimiteronError>(1)
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

    /// helper 参数值断言（模式对齐 limiteron
    /// src/circuit/types.rs:1401 test_builder_with_slow_call_config）：
    /// 默认 30s 窗口与 [resilience].circuit_open_timeout_ms 缺省一致，
    /// 短窗口经参数注入后仅 timeout 变化、其余参数不动。
    #[cfg(feature = "resilience")]
    #[test]
    fn nexusflow_circuit_config_values() {
        let config = nexusflow_circuit_config(Duration::from_secs(30));
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

        let short = nexusflow_circuit_config(Duration::from_millis(300));
        assert_eq!(short.timeout, Duration::from_millis(300));
        assert_eq!(short.failure_threshold, config.failure_threshold);
        assert_eq!(short.slow_call_duration_threshold, Duration::from_secs(60));

        // 配置缺省 ↔ 熔断构造缺省必须同源：[resilience] 段默认 30s，
        // 改任一侧而不同步另一侧时此处立即翻红
        assert_eq!(
            Duration::from_millis(
                crate::config::ResilienceSection::default().circuit_open_timeout_ms
            ),
            Duration::from_secs(30)
        );
    }
}
