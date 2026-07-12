// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{ConfigManager, VendorConfig};
use crate::protocol::unified::{UnifiedRequest, UnifiedResponse};
use crate::protocol::{ProtocolConverter, OpenAIConverter, AnthropicConverter};
use crate::error::ProxyError;
use crate::utils::retry::{ErrorCategory, RetryStrategy, extract_status_code, is_retryable_error};
use super::{VendorState, VendorStatus, VendorQueue, QueueManager, TokenBucket, ConcurrencyLimiter, PendingRequest, AdvancedRateLimiter};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use tokio::time::timeout;
use bytes::Bytes;
use reqwest;
use serde_json::Value;

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
}

impl VendorInstance {
    fn new(config: VendorConfig, queue_size: usize, rps: u32) -> Self {
        Self {
            advanced_limiter: AdvancedRateLimiter::new(&config.rate_limit),
            config: config.clone(),
            state: VendorState::new(),
            queue: Arc::new(VendorQueue::new(queue_size)),
            rate_limiter: TokenBucket::new(queue_size, rps),
            concurrency_limiter: ConcurrencyLimiter::new(queue_size),
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
}

impl VendorManager {
    pub fn new(config: Arc<ConfigManager>) -> Self {
        Self {
            config,
            instances: Arc::new(RwLock::new(std::collections::HashMap::new())),
            queue_manager: Arc::new(QueueManager::new()),
            client: reqwest::Client::new(),
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
        }
    }
    
    #[allow(dead_code)]
    pub async fn initialize(&self) -> Result<(), ProxyError> {
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
        
        let instance = Arc::new(VendorInstance::new(vendor.clone(), queue_size, rps));
        guard.insert(vendor.id.clone(), instance.clone());
        
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
                    let _ = pending.response_tx.send(Err(ProxyError::RateLimited { 
                        vendor: instance.config.id.clone() 
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
                        &anthropic
                    ).await;
                    
                    // Update Token Usage if successful
                    if let Ok(ref response) = result {
                        instance.advanced_limiter.consume_tokens(response.usage.total_tokens as u64);
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
    ) -> Result<UnifiedResponse, ProxyError> {
        let converter: &dyn ProtocolConverter = match config.type_.as_str() {
            "openai" => openai,
            "anthropic" => anthropic,
            _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
        };

        // Convert UnifiedRequest to Vendor Request (JSON)
        let request_body = converter.request_to_external(request)?;

        // 执行带重试的请求
        Self::execute_http_with_retry(client, config, request_body, converter).await
    }

    /// 执行带重试的 HTTP 请求
    async fn execute_http_with_retry(
        client: &reqwest::Client,
        config: &VendorConfig,
        request_body: Value,
        converter: &dyn ProtocolConverter,
    ) -> Result<UnifiedResponse, ProxyError> {
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
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
        } else {
            if api_base.ends_with("/v1") {
                format!("{}/chat/completions", api_base)
            } else {
                format!("{}/v1/chat/completions", api_base)
            }
        };

        // 记录当前重试次数
        let mut retry_count = 0u32;
        let max_retries = 5u32;

        loop {
            // 构建请求
            let mut builder = client.post(&url);

            if config.type_ == "anthropic" {
                builder = builder
                    .header("x-api-key", std::env::var(&config.api_key_env).unwrap_or_default())
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json");
            } else {
                builder = builder
                    .header("Authorization", format!("Bearer {}", std::env::var(&config.api_key_env).unwrap_or_default()))
                    .header("content-type", "application/json");
            }

            // 发送请求
            let response = match builder
                .json(&request_body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    // 网络错误 - 判断是否可重试
                    let is_timeout = e.is_timeout() || e.is_connect();
                    if is_timeout && retry_count < 2 {
                        retry_count += 1;
                        tokio::time::sleep(Duration::from_millis(500 * retry_count as u64)).await;
                        continue;
                    }
                    return Err(ProxyError::VendorUnavailable { vendor: config.id.clone() });
                }
            };

            let status = response.status();

            // 判断响应状态并决定是否重试
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let error = ProxyError::VendorUnavailable {
                    vendor: format!("{} ({}: {})", config.id, status, text)
                };

                // 根据状态码判断错误分类
                let category = ErrorCategory::from_status_code(status.as_u16());

                if category.is_retryable() && retry_count < max_retries {
                    retry_count += 1;

                    // 根据错误类型计算后退延迟
                    let delay = match category {
                        ErrorCategory::RetryableExponential => {
                            // 指数后退: 1s, 2s, 4s, 8s, 16s
                            Duration::from_millis(1000 * 2u64.pow(retry_count.min(4) as u32))
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
                    return Err(ProxyError::VendorUnavailable {
                        vendor: format!("Parse error: {}", e)
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
        self.queue_manager.get(vendor_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }
    
    pub async fn execute_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, ProxyError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id)
                .ok_or(ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?
                .clone()
        };
        
        // Remove direct rate limiter check, as it's handled by worker
        
        let (response_tx, response_rx) = oneshot::channel();
        
        let pending = PendingRequest { request, response_tx };
        
        instance.queue.send(pending).await?;
        
        let timeout_duration = Duration::from_secs(60);
        timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| ProxyError::Timeout { vendor: vendor_id.to_string() })?
            .map_err(|e| e)?
    }

    /// 使用降级机制执行请求
    ///
    /// 按权重顺序尝试可用的厂商，失败时自动切换到下一个
    pub async fn request_with_fallback(
        &self,
        abstract_model: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, ProxyError> {
        // 获取模型对应的所有厂商（resolve_model 定义在 ConfigManager 上）
        let candidates: Vec<(VendorConfig, crate::config::ModelConfig)> =
            self.config.resolve_model(abstract_model);

        if candidates.is_empty() {
            return Err(ProxyError::Validation {
                field: format!("模型 {} 未找到", abstract_model),
            });
        }

        // 按权重排序
        let mut sorted_candidates: Vec<_> = candidates
            .into_iter()
            .map(|(vendor, model)| {
                let weight = model.weight.max(0.1);
                (weight, vendor, model)
            })
            .collect();
        sorted_candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut last_error_msg = String::new();

        for (_, vendor, _model) in sorted_candidates {
            // 检查厂商状态
            let status = self.get_status(&vendor.id);
            if status == crate::vendor::VendorStatus::Unavailable {
                continue;
            }

            // 尝试执行请求
            match self.execute_request(&vendor.id, request.clone()).await {
                Ok(response) => {
                    return Ok(response);
                }
                Err(e) => {
                    last_error_msg = e.to_string();

                    // 根据错误类型决定是否标记厂商不可用
                    match &e {
                        ProxyError::RateLimited { .. } | ProxyError::Timeout { .. } => {
                            // 暂时性错误，可以重试当前厂商或继续下一厂商
                            continue;
                        }
                        ProxyError::Protocol { source, .. } => {
                            // 协议错误可能是暂时性的，也可能是永久性的
                            // 检查是否包含明确不可重试的错误
                            if source.contains("401") || source.contains("403") || source.contains("404") {
                                // 永久性错误，标记厂商不可用
                                self.mark_vendor_unavailable(&vendor.id);
                            }
                            continue;
                        }
                        ProxyError::VendorUnavailable { .. } => {
                            // 厂商不可用
                            continue;
                        }
                        _ => {
                            // 其他错误，继续尝试下一厂商
                            continue;
                        }
                    }
                }
            }
        }

        // 所有厂商都失败了
        if !last_error_msg.is_empty() {
            Err(ProxyError::Protocol {
                source: last_error_msg,
                inner: None,
            })
        } else {
            Err(ProxyError::AllVendorsUnavailable)
        }
    }

    /// 标记厂商为不可用状态
    fn mark_vendor_unavailable(&self, vendor_id: &str) {
        let instances = self.instances.read().unwrap();
        if let Some(instance) = instances.get(vendor_id) {
            instance.state.mark_unavailable();
        }
    }

    pub async fn execute_stream_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<impl futures::Stream<Item = Result<Bytes, reqwest::Error>>, ProxyError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id)
                .ok_or(ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?
                .clone()
        };
        
        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(ProxyError::RateLimited { vendor: vendor_id.to_string() });
        }

        // Enforce limits manually for stream requests
        instance.rate_limiter.acquire().await;
        let permit = instance.concurrency_limiter.acquire().await; // Hold permit?
        
        let config = &instance.config;
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
            }
        } else {
            &config.api_base
        };
        
        // Need to convert request here too!
        let converter: &dyn ProtocolConverter = match config.type_.as_str() {
            "openai" => &*self.openai_converter,
            "anthropic" => &*self.anthropic_converter,
            _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
        };
        
        let request_body = converter.request_to_external(request)?;

        let url = if config.type_ == "anthropic" {
            if api_base.ends_with("/v1") {
                format!("{}/messages", api_base)
            } else {
                format!("{}/v1/messages", api_base)
            }
        } else {
             if api_base.ends_with("/v1") {
                format!("{}/chat/completions", api_base)
            } else {
                format!("{}/v1/chat/completions", api_base)
            }
        };
        
        let mut request_builder = self.client
            .post(&url);
            
        if config.type_ == "anthropic" {
            request_builder = request_builder
                .header("x-api-key", std::env::var(&config.api_key_env).map_err(|_| ProxyError::Config { message: format!("API key env {} not set", config.api_key_env) })?)
                .header("anthropic-version", "2023-06-01");
        } else {
            request_builder = request_builder
                .header("Authorization", format!("Bearer {}", std::env::var(&config.api_key_env).map_err(|_| ProxyError::Config { message: format!("API key env {} not set", config.api_key_env) })?));
        }
        
        let response = request_builder
            .json(&request_body)
            .send()
            .await
            .map_err(|_e| ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?;
        
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
    ) -> Result<Value, ProxyError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id)
                .ok_or(ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?
                .clone()
        };

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(ProxyError::RateLimited { vendor: vendor_id.to_string() });
        }

        let permit = instance.concurrency_limiter.acquire().await;
        let _permit = permit; // Hold permit during request

        let config = &instance.config;
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
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
        } else {
            if api_base.ends_with("/v1") {
                format!("{}/chat/completions", api_base)
            } else {
                format!("{}/v1/chat/completions", api_base)
            }
        };

        let mut builder = self.client.post(&url);

        if config.type_ == "anthropic" {
            builder = builder
                .header("x-api-key", std::env::var(&config.api_key_env).unwrap_or_default())
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json");
        } else {
            builder = builder
                .header("Authorization", format!("Bearer {}", std::env::var(&config.api_key_env).unwrap_or_default()))
                .header("content-type", "application/json");
        }

        let response = builder
            .json(&request_body)
            .send()
            .await
            .map_err(|_| ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProxyError::VendorUnavailable {
                vendor: format!("{} ({}: {})", vendor_id, status, text)
            });
        }

        let response_json: Value = response.json().await
            .map_err(|e| ProxyError::VendorUnavailable { vendor: format!("Parse error: {}", e) })?;

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
    ) -> Result<impl futures::Stream<Item = Result<Bytes, reqwest::Error>>, ProxyError> {
        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id)
                .ok_or(ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?
                .clone()
        };

        // Check rate limiter
        instance.rate_limiter.acquire().await;

        // Check Advanced Rate Limiter
        if !instance.advanced_limiter.check() {
            return Err(ProxyError::RateLimited { vendor: vendor_id.to_string() });
        }

        let permit = instance.concurrency_limiter.acquire().await;
        let _permit = permit;

        let config = &instance.config;
        let api_base = if config.api_base.is_empty() {
            match config.type_.as_str() {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                _ => return Err(ProxyError::Config { message: "Unknown vendor type".into() }),
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
        } else {
            if api_base.ends_with("/v1") {
                format!("{}/chat/completions", api_base)
            } else {
                format!("{}/v1/chat/completions", api_base)
            }
        };

        let mut builder = self.client.post(&url);

        if config.type_ == "anthropic" {
            builder = builder
                .header("x-api-key", std::env::var(&config.api_key_env).unwrap_or_default())
                .header("anthropic-version", "2023-06-01");
        } else {
            builder = builder
                .header("Authorization", format!("Bearer {}", std::env::var(&config.api_key_env).unwrap_or_default()));
        }

        let response = builder
            .json(&request_body)
            .send()
            .await
            .map_err(|_| ProxyError::VendorUnavailable { vendor: vendor_id.to_string() })?;

        Ok(response.bytes_stream())
    }

    #[allow(dead_code)]
    pub fn update_status(&self, vendor_id: &str, status: VendorStatus) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.set_status(status);
        }
    }
    
    #[allow(dead_code)]
    pub fn mark_busy(&self, vendor_id: &str) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_busy();
        }
    }
    
    #[allow(dead_code)]
    pub fn mark_available(&self, vendor_id: &str) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_available();
        }
    }
    
    #[allow(dead_code)]
    pub fn mark_throttled(&self, vendor_id: &str, backoff: Duration) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_throttled(backoff);
        }
    }
    
    #[allow(dead_code)]
    pub fn inc_error(&self, vendor_id: &str) -> u8 {
        let guard = self.instances.read().unwrap();
        guard.get(vendor_id)
            .map(|i| i.state.inc_error())
            .unwrap_or(0)
    }
    
    #[allow(dead_code)]
    pub fn reset_status(&self, vendor_id: &str) {
        let guard = self.instances.read().unwrap();
        if let Some(instance) = guard.get(vendor_id) {
            instance.state.mark_available();
            instance.state.reset_error();
            instance.rate_limiter.reset();
        }
    }
    
    pub fn get_all_status(&self) -> Vec<(String, VendorStatus, usize)> {
        let guard = self.instances.read().unwrap();
        let mut status = Vec::new();

        for (id, instance) in guard.iter() {
            let queue_len = self.queue_manager.get(id)
                .map(|q| q.len())
                .unwrap_or(0);
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
                            .unwrap_or_else(|| now_local);
                        let end_dt = Instant::now() + end_datetime.signed_duration_since(now_local).to_std().unwrap_or_default();
                        (None, Some(end_dt))
                    }
                }
                super::AdvancedRateLimiter::Interval { interval_secs, last_reset } => {
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
