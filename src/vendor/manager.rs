// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{
    AdvancedRateLimiter, ConcurrencyLimiter, PendingRequest, QueueManager, TokenBucket,
    VendorQueue, VendorState, VendorStatus,
};
use crate::config::{ConfigManager, VendorConfig};
use crate::error::NexusFlowError;
use crate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
use crate::protocol::{UnifiedRequest, UnifiedResponse};
use crate::utils::metrics::GlobalMetrics;
use crate::utils::retry::ErrorCategory;
#[cfg(feature = "cost-tracking")]
use crate::utils::CostTracker;
#[cfg(feature = "resilience")]
use crate::vendor::resilience::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError,
};
#[cfg(feature = "cost-tracking")]
use super::CostCalculator;
use bytes::Bytes;
use reqwest;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

struct VendorInstance {
    config: VendorConfig,
    state: VendorState,
    queue: Arc<VendorQueue>,
    rate_limiter: TokenBucket,
    concurrency_limiter: ConcurrencyLimiter,
    advanced_limiter: AdvancedRateLimiter,
    #[cfg(feature = "resilience")]
    circuit_breaker: Arc<CircuitBreaker>,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
    #[cfg(feature = "cost-tracking")]
    cost_calculator: Arc<CostCalculator>,
}

impl VendorInstance {
    fn new(
        config: VendorConfig,
        queue_size: usize,
        rps: u32,
        #[cfg(feature = "cost-tracking")] cost_tracker: Arc<CostTracker>,
        #[cfg(feature = "cost-tracking")] cost_calculator: Arc<CostCalculator>,
    ) -> Self {
        Self {
            advanced_limiter: AdvancedRateLimiter::new(&config.rate_limit),
            config: config.clone(),
            state: VendorState::new(),
            queue: Arc::new(VendorQueue::new(queue_size)),
            rate_limiter: TokenBucket::new(queue_size, rps),
            concurrency_limiter: ConcurrencyLimiter::new(queue_size),
            #[cfg(feature = "resilience")]
            circuit_breaker: Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())),
            #[cfg(feature = "cost-tracking")]
            cost_tracker,
            #[cfg(feature = "cost-tracking")]
            cost_calculator,
        }
    }
}

#[derive(Clone)]
pub struct VendorManager {
    config: Arc<ConfigManager>,
    instances: Arc<RwLock<std::collections::HashMap<String, Arc<VendorInstance>>>>,
    queue_manager: Arc<QueueManager>,
    client: reqwest::Client,
    openai_converter: Arc<OpenAIConverter>,
    anthropic_converter: Arc<AnthropicConverter>,
    metrics: Arc<GlobalMetrics>,
    #[cfg(feature = "resilience")]
    circuit_breakers:
        Arc<RwLock<std::collections::HashMap<String, Arc<CircuitBreaker>>>>,
    #[cfg(feature = "cost-tracking")]
    cost_tracker: Arc<CostTracker>,
    #[cfg(feature = "cost-tracking")]
    cost_calculator: Arc<CostCalculator>,
}

impl VendorManager {
    pub fn new(config: Arc<ConfigManager>) -> Self {
        // 启用 cost-tracking 时从 config.pricing 构建定价表
        #[cfg(feature = "cost-tracking")]
        let cost_calculator = {
            let pricing = config.read().pricing.clone();
            Arc::new(CostCalculator::from_config(&pricing))
        };

        Self {
            config,
            instances: Arc::new(RwLock::new(std::collections::HashMap::new())),
            queue_manager: Arc::new(QueueManager::new()),
            client: reqwest::Client::new(),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            metrics: Arc::new(GlobalMetrics::new()),
            #[cfg(feature = "resilience")]
            circuit_breakers: Arc::new(RwLock::new(std::collections::HashMap::new())),
            #[cfg(feature = "cost-tracking")]
            cost_tracker: Arc::new(CostTracker::new()),
            #[cfg(feature = "cost-tracking")]
            cost_calculator,
        }
    }

    /// 全局指标访问器 — 用于 /admin/metrics 端点
    pub fn metrics(&self) -> &Arc<GlobalMetrics> {
        &self.metrics
    }

    /// 全局成本追踪器访问器 — 用于 /admin/cost 端点。
    /// 仅在启用 `cost-tracking` feature 时可用。
    #[cfg(feature = "cost-tracking")]
    pub fn cost_tracker(&self) -> &Arc<CostTracker> {
        &self.cost_tracker
    }

    /// 读取 vendor 的 CircuitBreaker 当前状态（用于运维观测/测试断言）。
    /// 未启用 resilience feature 或 vendor 未初始化时返回 `None`。
    #[cfg(feature = "resilience")]
    pub fn get_circuit_state(
        &self,
        vendor_id: &str,
    ) -> Option<crate::vendor::resilience::State> {
        let guard = self.circuit_breakers.read().unwrap();
        guard.get(vendor_id).map(|cb| cb.state())
    }

    /// 强制将指定 vendor 的 CircuitBreaker 切到 Open 状态。
    /// 仅供集成测试使用 — 跳过"触发 5 次真实失败"的慢路径，避免
    /// `tokio::time::pausable` runtime 与真实 mockito HTTP IO 交互时
    /// 因 auto-advance 提前触发 execute_request 的 60s 超时。
    /// 业务代码不应调用。
    #[cfg(feature = "resilience")]
    #[doc(hidden)]
    pub fn force_circuit_open(&self, vendor_id: &str) {
        let guard = self.circuit_breakers.read().unwrap();
        if let Some(cb) = guard.get(vendor_id) {
            cb.force_open();
        }
    }

    pub async fn initialize(&self) -> Result<(), NexusFlowError> {
        let vendors = self.config.get_enabled_vendors();
        for vendor in &vendors {
            self.get_or_create_instance(vendor);
        }

        // Start background reset worker
        let instances = self.instances.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let guard = instances.read().unwrap();
                for instance in guard.values() {
                    if instance.advanced_limiter.should_reset() {
                        instance.advanced_limiter.reset();
                    }
                }
            }
        });

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
            vendor.clone(),
            queue_size,
            rps,
            #[cfg(feature = "cost-tracking")]
            self.cost_tracker.clone(),
            #[cfg(feature = "cost-tracking")]
            self.cost_calculator.clone(),
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

        tokio::spawn(async move {
            Self::process_vendor_queue(instance_clone, client_clone, openai, anthropic).await;
        });

        instance
    }

    async fn process_vendor_queue(
        instance: Arc<VendorInstance>,
        client: reqwest::Client,
        openai: Arc<OpenAIConverter>,
        anthropic: Arc<AnthropicConverter>,
    ) {
        loop {
            // 1. Wait for rate limiter (RPS)
            instance.rate_limiter.acquire().await;

            // 2. Get request from queue
            if let Some(pending) = instance.queue.recv().await {
                // Check Advanced Rate Limiter
                if !instance.advanced_limiter.check() {
                    let _ = pending.response_tx.send(Err(NexusFlowError::RateLimited {
                        vendor: instance.config.id.clone(),
                    }));
                    continue;
                }

                // 3. Acquire concurrency permit
                let permit_result = instance.concurrency_limiter.acquire().await;

                let client = client.clone();
                let instance = instance.clone();
                let openai = openai.clone();
                let anthropic = anthropic.clone();

                tokio::spawn(async move {
                    // Hold permit
                    let _permit = permit_result;

                    let result = Self::perform_http_request(
                        &client,
                        &instance.config,
                        pending.request,
                        &openai,
                        &anthropic,
                        #[cfg(feature = "resilience")]
                        &instance.circuit_breaker,
                    )
                    .await;

                    // 成功路径：更新 token 使用统计 + 累加成本
                    if let Ok(ref response) = result {
                        instance
                            .advanced_limiter
                            .consume_tokens(response.usage.total_tokens as u64);

                        // cost-tracking：按 (vendor, model) 计算 USD 并累加
                        // 解析失败时 calculate 返回 0.0（CostTracker 静默 no-op）
                        #[cfg(feature = "cost-tracking")]
                        {
                            let cost_usd = instance.cost_calculator.calculate(
                                &instance.config.id,
                                &response.model,
                                response.usage.prompt_tokens,
                                response.usage.completion_tokens,
                            );
                            instance.cost_tracker.record(
                                &instance.config.id,
                                &response.model,
                                cost_usd,
                            );
                        }
                    }

                    let _ = pending.response_tx.send(result);
                });
            } else {
                break;
            }
        }
    }

    async fn perform_http_request(
        client: &reqwest::Client,
        config: &VendorConfig,
        request: UnifiedRequest,
        openai: &OpenAIConverter,
        anthropic: &AnthropicConverter,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker>,
    ) -> Result<UnifiedResponse, NexusFlowError> {
        let converter: &dyn ProtocolConverter = match config.type_.as_str() {
            "openai" => openai,
            "anthropic" => anthropic,
            _ => {
                return Err(NexusFlowError::Config {
                    message: "Unknown vendor type".into(),
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
            #[cfg(feature = "resilience")]
            circuit_breaker,
        )
        .await
    }

    /// 构建 vendor API URL（统一 4 处重复的 URL 拼接逻辑）
    fn build_vendor_url(config: &VendorConfig) -> Result<String, NexusFlowError> {
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => {
                    return Err(NexusFlowError::Config {
                        message: "Unknown vendor type".into(),
                    })
                }
            }
        } else {
            &config.api_base
        };

        let url = if config.type_ == "anthropic" {
            if api_base.ends_with("/v1") {
                format!("{}/messages", api_base)
            } else {
                format!("{}/v1/messages", api_base)
            }
        } else if api_base.ends_with("/v1") {
            format!("{}/chat/completions", api_base)
        } else {
            format!("{}/v1/chat/completions", api_base)
        };

        Ok(url)
    }

    /// 获取 API key 并构建带认证 header 的 RequestBuilder
    fn build_authed_request(
        client: &reqwest::Client,
        config: &VendorConfig,
        url: &str,
        include_content_type: bool,
    ) -> Result<reqwest::RequestBuilder, NexusFlowError> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| NexusFlowError::Config {
            message: format!("API key env {} not set", config.api_key_env),
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

    /// 执行带重试的 HTTP 请求。
    ///
    /// 启用 `resilience` feature 时由 `CircuitBreaker` 包装：
    /// - Closed/HalfOpen 状态下放行至内部重试逻辑；
    /// - Open 状态下直接返回 `VendorUnavailable`，无 HTTP 调用；
    /// - 调用结果由 `IsRetryable` 决定是否计入失败计数。
    async fn execute_http_with_retry(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
        #[cfg(feature = "resilience")] circuit_breaker: &Arc<CircuitBreaker>,
    ) -> Result<UnifiedResponse, NexusFlowError> {
        #[cfg(feature = "resilience")]
        {
            let vendor_id = config.id.clone();
            let result = circuit_breaker
                .call(|| async {
                    Self::execute_http_with_retry_inner(
                        client,
                        config,
                        request_body.clone(),
                        converter,
                    )
                    .await
                })
                .await;
            result.map_err(|e| match e {
                CircuitBreakerError::Open => NexusFlowError::VendorUnavailable {
                    vendor: format!("{} (circuit open)", vendor_id),
                },
                CircuitBreakerError::Inner(e) => e,
            })
        }
        #[cfg(not(feature = "resilience"))]
        {
            Self::execute_http_with_retry_inner(client, config, request_body, converter).await
        }
    }

    /// HTTP 请求 + 内部手写重试循环（v0.1 行为保留）。
    /// 在 `resilience` feature 启用时由 `execute_http_with_retry` 经 CB 包装后调用。
    async fn execute_http_with_retry_inner(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
    ) -> Result<UnifiedResponse, NexusFlowError> {
        let url = Self::build_vendor_url(config)?;

        // 记录当前重试次数
        let mut retry_count = 0u32;
        let max_retries = 5u32;

        loop {
            // 构建带认证的请求
            let builder = Self::build_authed_request(client, config, &url, true)?;

            // 发送请求
            let response = match builder.json(&request_body).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    // 网络错误 - 判断是否可重试
                    let is_timeout = e.is_timeout() || e.is_connect();
                    if is_timeout && retry_count < 2 {
                        retry_count += 1;
                        tokio::time::sleep(Duration::from_millis(500 * retry_count as u64)).await;
                        continue;
                    }
                    return Err(NexusFlowError::VendorUnavailable {
                        vendor: config.id.clone(),
                    });
                }
            };

            let status = response.status();

            // 判断响应状态并决定是否重试
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                tracing::warn!(
                    vendor = %config.id,
                    status = %status,
                    body = %text,
                    "upstream vendor returned non-success status"
                );
                let error = NexusFlowError::VendorUnavailable {
                    vendor: format!("{} ({})", config.id, status),
                };

                // 根据状态码判断错误分类
                let category = ErrorCategory::from_status_code(status.as_u16());

                if category.is_retryable() && retry_count < max_retries {
                    retry_count += 1;

                    // 根据错误类型计算后退延迟
                    let delay = match category {
                        ErrorCategory::RetryableExponential => {
                            // 指数后退: 1s, 2s, 4s, 8s, 16s
                            Duration::from_millis(1000 * 2u64.pow(retry_count.min(4)))
                        }
                        ErrorCategory::RetryableFixed => {
                            // 固定间隔: 1s
                            Duration::from_secs(1)
                        }
                        _ => Duration::from_secs(1),
                    };

                    tokio::time::sleep(delay).await;
                    continue;
                }

                return Err(error);
            }

            // 解析响应
            let response_json: Value = match response.json().await {
                Ok(json) => json,
                Err(e) => {
                    return Err(NexusFlowError::VendorUnavailable {
                        vendor: format!("Parse error: {}", e),
                    });
                }
            };

            // 转换为统一响应格式
            return converter.parse_response(response_json);
        }
    }

    pub fn get_status(&self, vendor_id: &str) -> VendorStatus {
        if let Some(vendor) = self.config.get_vendor(vendor_id) {
            if !vendor.enabled {
                return VendorStatus::Unavailable;
            }
            let guard = self.instances.read().unwrap();
            if let Some(instance) = guard.get(vendor_id) {
                // Check advanced limiter too
                if !instance.advanced_limiter.check() {
                    // Ideally should return Throttled, but VendorStatus enum is simple.
                    // Let's check if Throttled is appropriate.
                    // Yes, Throttled is a valid status.
                    return VendorStatus::Throttled;
                }
                return instance.state.get_status();
            }
            VendorStatus::Available
        } else {
            VendorStatus::Unavailable
        }
    }

    pub fn get_queue_length(&self, vendor_id: &str) -> usize {
        self.queue_manager
            .get(vendor_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    pub async fn execute_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, NexusFlowError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(NexusFlowError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        // Remove direct rate limiter check, as it's handled by worker

        let (response_tx, response_rx) = oneshot::channel();

        let pending = PendingRequest {
            request,
            response_tx,
        };

        instance.queue.send(pending).await?;

        let timeout_duration = Duration::from_secs(60);
        timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| NexusFlowError::Timeout {
                vendor: vendor_id.to_string(),
            })??
    }

    pub async fn execute_stream_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<impl futures::Stream<Item = Result<Bytes, reqwest::Error>>, NexusFlowError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(NexusFlowError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(NexusFlowError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        // Enforce limits manually for stream requests
        instance.rate_limiter.acquire().await;
        let permit = instance.concurrency_limiter.acquire().await; // Hold permit?

        let config = &instance.config;
        let url = Self::build_vendor_url(config)?;

        // Need to convert request here too!
        let converter: &dyn ProtocolConverter = match config.type_.as_str() {
            "openai" => &*self.openai_converter,
            "anthropic" => &*self.anthropic_converter,
            _ => {
                return Err(NexusFlowError::Config {
                    message: "Unknown vendor type".into(),
                })
            }
        };

        let request_body = converter.request_to_external(request)?;

        let request_builder = Self::build_authed_request(&self.client, config, &url, true)?;

        let response = request_builder
            .json(&request_body)
            .send()
            .await
            .map_err(|_e| NexusFlowError::VendorUnavailable {
                vendor: vendor_id.to_string(),
            })?;

        // We need to keep permit alive if we want real concurrency limit.
        // For now, we drop it.
        drop(permit);

        Ok(response.bytes_stream())
    }

    /// 执行零拷贝透传请求
    /// 当入站协议与出站协议相同时使用，避免序列化/反序列化开销
    pub async fn execute_passthrough_request(
        &self,
        vendor_id: &str,
        request_body: Value,
    ) -> Result<Value, NexusFlowError> {
        let start = Instant::now();
        let result = self
            .execute_passthrough_request_inner(vendor_id, request_body)
            .await;
        let duration = start.elapsed();
        match &result {
            Ok(_) => self.metrics.record_request(vendor_id, true, duration),
            Err(e) => {
                self.metrics.record_request(vendor_id, false, duration);
                self.metrics.record_error(vendor_id, &e.to_string());
            }
        }
        result
    }

    async fn execute_passthrough_request_inner(
        &self,
        vendor_id: &str,
        request_body: Value,
    ) -> Result<Value, NexusFlowError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(NexusFlowError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(NexusFlowError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        let permit = instance.concurrency_limiter.acquire().await;
        let _permit = permit; // Hold permit during request

        let config = &instance.config;
        let url = Self::build_vendor_url(config)?;

        let builder = Self::build_authed_request(&self.client, config, &url, true)?;

        let response = builder.json(&request_body).send().await.map_err(|_| {
            NexusFlowError::VendorUnavailable {
                vendor: vendor_id.to_string(),
            }
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(
                vendor = %vendor_id,
                status = %status,
                body = %text,
                "upstream vendor returned non-success status"
            );
            return Err(NexusFlowError::VendorUnavailable {
                vendor: format!("{} ({})", vendor_id, status),
            });
        }

        let response_json: Value =
            response
                .json()
                .await
                .map_err(|e| NexusFlowError::VendorUnavailable {
                    vendor: format!("Parse error: {}", e),
                })?;

        // 更新 Token 使用统计
        if let Some(usage) = response_json.get("usage") {
            if let Some(total) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
                instance.advanced_limiter.consume_tokens(total);
            }
        }

        Ok(response_json)
    }

    /// 执行零拷贝透传流式请求
    pub async fn execute_passthrough_stream_request(
        &self,
        vendor_id: &str,
        request_body: Value,
    ) -> Result<impl futures::Stream<Item = Result<Bytes, reqwest::Error>>, NexusFlowError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard
                .get(vendor_id)
                .ok_or(NexusFlowError::VendorUnavailable {
                    vendor: vendor_id.to_string(),
                })?
                .clone()
        };

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(NexusFlowError::RateLimited {
                vendor: vendor_id.to_string(),
            });
        }

        let permit = instance.concurrency_limiter.acquire().await;
        let _permit = permit;

        let config = &instance.config;
        let url = Self::build_vendor_url(config)?;

        let builder = Self::build_authed_request(&self.client, config, &url, false)?;

        let response = builder.json(&request_body).send().await.map_err(|_| {
            NexusFlowError::VendorUnavailable {
                vendor: vendor_id.to_string(),
            }
        })?;

        Ok(response.bytes_stream())
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
            let queue_len = self.queue_manager.get(id).map(|q| q.len()).unwrap_or(0);
            status.push((id.clone(), instance.state.get_status(), queue_len));
        }

        status
    }

    /// 获取厂商的窗口信息，用于路由权重计算
    pub fn get_window_info(&self, vendor_id: &str) -> Option<WindowInfo> {
        let guard = self.instances.read().ok()?;

        if let Some(instance) = guard.get(vendor_id) {
            let (max_tokens, window_end) = match &instance.advanced_limiter {
                super::AdvancedRateLimiter::Token { max_tokens, .. } => (Some(*max_tokens), None),
                super::AdvancedRateLimiter::TimeWindow { start, end } => {
                    let now = chrono::Local::now().time();
                    if now < *start || now >= *end {
                        (None, None)
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
                        (None, Some(end_dt))
                    }
                }
                super::AdvancedRateLimiter::Interval {
                    interval_secs,
                    last_reset,
                } => {
                    let last = last_reset.load(std::sync::atomic::Ordering::Relaxed);
                    let elapsed = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .saturating_sub(last);
                    if elapsed < *interval_secs {
                        let remaining = Duration::from_secs(*interval_secs - elapsed);
                        (None, Some(Instant::now() + remaining))
                    } else {
                        (None, None)
                    }
                }
                super::AdvancedRateLimiter::None => (None, None),
            };

            let used_tokens = match &instance.advanced_limiter {
                super::AdvancedRateLimiter::Token { used_tokens, .. } => {
                    used_tokens.load(std::sync::atomic::Ordering::Relaxed)
                }
                _ => 0,
            };

            Some(WindowInfo {
                max_tokens,
                used_tokens,
                window_end,
            })
        } else {
            None
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
        assert!(
            (pct - 0.5).abs() < 1e-9,
            "expected 0.5, got {}",
            pct
        );
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
}
