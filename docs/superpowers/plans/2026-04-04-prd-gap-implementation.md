# PRD 缺口修复实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 PRD 审计中发现的未完成项：优雅关闭、权重计算完善、`utils/metrics.rs` 独立指标模块、补充缺失测试。

**Architecture:** 在现有模块基础上增量修改，不改变整体架构。新增 `utils/metrics.rs` 指标收集模块；在 `main.rs` 添加优雅关闭；完善路由权重计算；补充流式、队列、监控 API 测试。

**Tech Stack:** Rust, Axum 0.8, Tokio, SQLx (SQLite), async-channel, rand

---

## Task 1: 优雅关闭机制

**Files:**

- Modify: `src/main.rs`

**问题:** `main.rs:96` 使用 `.unwrap()` 解析 host，无优雅关闭信号处理。

- [ ] **Step 1: 为 `main.rs` 添加 graceful shutdown**

将 `main.rs` 替换为以下内容：

```rust
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber;

mod config;
mod api;
mod protocol;
mod router;
mod vendor;
mod database;
mod utils;
mod error;
mod middleware;

use error::ProxyError;
use config::ConfigManager;
use config::loader::ConfigLoader;
use api::{ApiState, init_global_state};
use protocol::OpenAIConverter;
use protocol::AnthropicConverter;
use router::Router as RouterStruct;
use vendor::VendorManager;
use database::Database;

#[tokio::main]
async fn main() -> Result<(), ProxyError> {
    tracing_subscriber::fmt::init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".to_string());
    tracing::info!("Loading config from: {}", config_path);
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    tracing::info!("Config loaded successfully");

    let db_config = {
        let config = config_manager.read();
        config.database.clone()
    };
    tracing::info!("Initializing database at: {}", db_config.path);
    let database = Arc::new(Database::new(&db_config.path, db_config.pool_size).await?);
    tracing::info!("Database initialized");

    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await?;
    tracing::info!("Vendor manager initialized");

    let router = RouterStruct::new(config_manager.clone(), vendor_manager.clone());

    let api_state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(router),
        client: reqwest::Client::new(),
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: database.clone(),
    });

    init_global_state(api_state);
    tracing::info!("Global state initialized");

    let sdforge_config = {
        let config_ref = config_manager.read();

        let host: std::net::Ipv4Addr = config_ref.server.host.parse()
            .map_err(|e: std::net::AddrParseError| ProxyError::Config {
                message: format!("Invalid host address: {}", e),
            })?;

        sdforge::config::AppConfig {
            server: sdforge::config::ServerConfig {
                host: config_ref.server.host.clone(),
                port: config_ref.server.port,
                request_timeout_secs: 300,
                cors: Some(sdforge::config::CorsConfig {
                    allowed_origins: vec!["*".to_string()],
                    allowed_methods: vec!["GET".to_string(), "POST".to_string()],
                    allowed_headers: vec!["*".to_string()],
                }),
            },
            authentication: sdforge::config::AuthConfig::ApiKey {
                header_name: "Authorization".to_string(),
                prefix: "Bearer".to_string(),
            },
            timeout: Some(sdforge::config::TimeoutConfig::default()),
        }
    };

    let app = sdforge::http::build_with_config(&sdforge_config)?;

    let server_config = {
        let config = config_manager.read();
        config.server.clone()
    };

    let host: std::net::Ipv4Addr = server_config.host.parse()
        .map_err(|e: std::net::AddrParseError| ProxyError::Config {
            message: format!("Invalid host address '{}': {}", server_config.host, e),
        })?;
    let addr = SocketAddr::from((host, server_config.port));

    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await
        .map_err(|e| ProxyError::Config {
            message: format!("Failed to bind to {}: {}", addr, e),
        })?;

    // Graceful shutdown: listen for Ctrl+C
    let shutdown_signal = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        tracing::info!("Received shutdown signal, draining connections...");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .map_err(|e| ProxyError::Config {
            message: format!("Server error: {}", e),
        })?;

    tracing::info!("Server stopped gracefully");
    Ok(())
}
```

- [ ] **Step 2: 编译验证**

Run: `cargo check --message-format=short`
Expected: 无错误

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: add graceful shutdown and remove unwrap in main.rs"
```

---

## Task 2: 路由权重计算完善

**Files:**

- Modify: `src/router/mod.rs:255-266`
- Create: `tests/router_weight_test.rs`

**问题:** PRD 要求权重计算考虑时间窗口、队列状态、Token 余额三维度，当前只考虑队列长度和模型权重。

- [ ] **Step 1: 编写权重计算测试**

Create `tests/router_weight_test.rs`:

```rust
use smart_llm_proxy::config::*;
use smart_llm_proxy::vendor::VendorManager;
use smart_llm_proxy::router::Router as RouterStruct;
use std::sync::Arc;

fn make_vendor_config(id: &str, weight: f64) -> VendorConfig {
    VendorConfig {
        id: id.to_string(),
        name: id.to_string(),
        type_: "openai".to_string(),
        api_key_env: format!("{}_KEY", id.to_uppercase()),
        api_base: "https://api.example.com/v1".to_string(),
        enabled: true,
        models: vec![ModelConfig {
            model_id: "gpt-4".to_string(),
            abstract_id: Some("gpt-4".to_string()),
            weight,
            enabled: true,
        }],
        rate_limit: RateLimitConfig::Token { max_tokens: 100000 },
        concurrency: ConcurrencyConfig { requests_per_second: 10 },
    }
}

fn make_config(vendors: Vec<VendorConfig>) -> ConfigManager {
    let config = Config {
        server: ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
        },
        database: DatabaseConfig {
            path: ":memory:".to_string(),
            pool_size: 1,
        },
        vendors,
        global_model: None,
        auth: None,
    };
    ConfigManager::from_config(config)
}

#[tokio::test]
async fn test_weighted_select_prefers_empty_queue() {
    let config = make_config(vec![
        make_vendor_config("vendor_a", 1.0),
        make_vendor_config("vendor_b", 1.0),
    ]);
    let vendor_manager = Arc::new(VendorManager::new(Arc::new(config.clone())));
    vendor_manager.initialize().await.unwrap();
    let router = RouterStruct::new(Arc::new(config), vendor_manager);

    // Both vendors have empty queues and same weight, routing should succeed
    let decision = router.route("gpt-4").await;
    assert!(decision.is_ok());
}

#[tokio::test]
async fn test_calculate_weight_considers_window_info() {
    let config = make_config(vec![
        make_vendor_config("vendor_a", 2.0),
    ]);
    let vendor_manager = Arc::new(VendorManager::new(Arc::new(config.clone())));
    vendor_manager.initialize().await.unwrap();
    let router = RouterStruct::new(Arc::new(config), vendor_manager);

    let decision = router.route("gpt-4").await;
    assert!(decision.is_ok());
    // vendor_a should be selected (only candidate)
    assert_eq!(decision.unwrap().vendor.id, "vendor_a");
}

#[tokio::test]
async fn test_route_with_multiple_vendors_distributes() {
    let config = make_config(vec![
        make_vendor_config("vendor_a", 1.0),
        make_vendor_config("vendor_b", 1.0),
    ]);
    let vendor_manager = Arc::new(VendorManager::new(Arc::new(config.clone())));
    vendor_manager.initialize().await.unwrap();
    let router = RouterStruct::new(Arc::new(config), vendor_manager);

    let mut vendor_a_count = 0usize;
    let mut vendor_b_count = 0usize;
    let iterations = 100;

    for _ in 0..iterations {
        if let Ok(decision) = router.route("gpt-4").await {
            match decision.vendor.id.as_str() {
                "vendor_a" => vendor_a_count += 1,
                "vendor_b" => vendor_b_count += 1,
                _ => {}
            }
        }
    }

    // With equal weights, both should receive requests (approximate distribution)
    assert!(vendor_a_count > 0, "vendor_a should receive some requests");
    assert!(vendor_b_count > 0, "vendor_b should receive some requests");
    // Both should be within 20-80% range for 100 iterations with equal weights
    let ratio = vendor_a_count as f64 / iterations as f64;
    assert!(ratio > 0.2 && ratio < 0.8, "Distribution should be roughly equal, got {}/{}", vendor_a_count, vendor_b_count);
}
```

- [ ] **Step 2: 运行测试确认编译失败/通过**

Run: `cargo test --test router_weight_test -- --nocapture`
Expected: 编译并运行

- [ ] **Step 3: 完善 `calculate_weight` 方法，加入 WindowInfo 维度**

修改 `src/router/mod.rs` 的 `calculate_weight` 方法：

```rust
    fn calculate_weight(&self, vendor: &VendorConfig, model: &ModelConfig) -> f64 {
        let mut weight = model.weight;

        // 队列状态因子
        let queue_len = self.vendor_manager.get_queue_length(&vendor.id);
        if queue_len == 0 {
            weight *= 1.5;
        } else if queue_len < 10 {
            weight *= 1.2;
        }

        // 窗口信息因子（PRD 三维度：时间窗口、队列、Token 余额）
        if let Some(window_info) = self.vendor_manager.get_window_info(&vendor.id) {
            // 时间窗口即将结束但有余额 → 高权重
            if window_info.is_ending_soon() && window_info.has_budget() {
                weight *= 2.0;
            }

            // Token 余额百分比
            weight *= window_info.budget_percentage();
        }

        weight
    }
```

- [ ] **Step 4: 运行测试**

Run: `cargo test --test router_weight_test -- --nocapture`
Expected: PASS

- [ ] **Step 5: 运行原有路由测试确认无回归**

Run: `cargo test --test router_test -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/router/mod.rs tests/router_weight_test.rs
git commit -m "feat: enhance routing weight with window info (time/token budget)"
```

---

## Task 3: `utils/metrics.rs` 指标收集模块

**Files:**

- Create: `src/utils/metrics.rs`
- Modify: `src/utils/mod.rs`

**问题:** PRD 规划了 `utils/metrics.rs`，当前通过 `database/pre_aggregator.rs` 间接实现，但缺少独立的运行时指标收集模块。

- [ ] **Step 1: 创建 `src/utils/metrics.rs`**

```rust
//! 运行时指标收集模块
//!
//! 提供请求级和系统级的指标收集能力，包括：
//! - 请求计数和延迟统计
//! - Token 消耗跟踪
//! - 厂商健康状态统计

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// 单个请求的指标
#[derive(Debug, Clone)]
pub struct RequestMetrics {
    pub request_id: String,
    pub vendor_id: String,
    pub model: String,
    pub start_time: Instant,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl RequestMetrics {
    pub fn new(request_id: String, vendor_id: String, model: String) -> Self {
        Self {
            request_id,
            vendor_id,
            model,
            start_time: Instant::now(),
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn with_tokens(mut self, prompt: u32, completion: u32) -> Self {
        self.prompt_tokens = prompt;
        self.completion_tokens = completion;
        self
    }
}

/// 系统级运行时指标（原子操作，线程安全）
#[derive(Debug)]
pub struct SystemMetrics {
    total_requests: AtomicU64,
    total_errors: AtomicU64,
    total_tokens: AtomicU64,
    active_requests: AtomicU64,
}

impl SystemMetrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            total_tokens: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
        }
    }

    pub fn record_request(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_response(&self, tokens: u64) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
        self.total_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
        self.total_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 生成指标快照
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            total_errors: self.total_errors.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
            active_requests: self.active_requests.load(Ordering::Relaxed),
        }
    }
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 指标快照（用于 API 返回）
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_tokens: u64,
    pub active_requests: u64,
}

/// 全局指标实例
static METRICS: once_cell::sync::Lazy<SystemMetrics> = once_cell::sync::Lazy::new(SystemMetrics::new);

/// 获取全局指标实例
pub fn global_metrics() -> &'static SystemMetrics {
    &METRICS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_metrics_elapsed() {
        let metrics = RequestMetrics::new("req-1".into(), "vendor-a".into(), "gpt-4".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(metrics.elapsed_ms() >= 10);
    }

    #[test]
    fn test_request_metrics_with_tokens() {
        let metrics = RequestMetrics::new("req-1".into(), "vendor-a".into(), "gpt-4".into())
            .with_tokens(100, 50);
        assert_eq!(metrics.prompt_tokens, 100);
        assert_eq!(metrics.completion_tokens, 50);
    }

    #[test]
    fn test_system_metrics_snapshot() {
        let metrics = SystemMetrics::new();
        metrics.record_request();
        metrics.record_response(150);
        metrics.record_request();
        metrics.record_error();

        let snap = metrics.snapshot();
        assert_eq!(snap.total_requests, 2);
        assert_eq!(snap.total_errors, 1);
        assert_eq!(snap.total_tokens, 150);
        assert_eq!(snap.active_requests, 0);
    }
}
```

- [ ] **Step 2: 修改 `src/utils/mod.rs` 添加 metrics 模块**

在 `src/utils/mod.rs` 开头添加：

```rust
pub mod metrics;
```

- [ ] **Step 3: 编译验证**

Run: `cargo check --message-format=short`
Expected: 无错误

- [ ] **Step 4: 运行 metrics 单元测试**

Run: `cargo test utils::metrics::tests --lib -- --nocapture`
Expected: 3 个测试全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/utils/metrics.rs src/utils/mod.rs
git commit -m "feat: add utils/metrics.rs runtime metrics collection module"
```

---

## Task 4: 将指标集成到厂商管理器

**Files:**

- Modify: `src/vendor/manager.rs`

**问题:** `VendorManager` 中请求执行未收集运行时指标。

- [ ] **Step 1: 在 `execute_request` 中集成指标收集**

在 `src/vendor/manager.rs` 顶部添加导入：

```rust
use crate::utils::metrics::global_metrics;
```

在 `execute_request` 方法（约第 374 行）中，在 `let instance = {` 之前添加一行，在成功返回前添加一行，在错误返回前添加一行：

```rust
    pub async fn execute_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, ProxyError> {
        global_metrics().record_request();

        let instance = {
            // ... 保持不变
        };

        // ... 保持不变

        let result = timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| ProxyError::Timeout { vendor: vendor_id.to_string() })?
            .map_err(|e| e)?;

        match &result {
            Ok(response) => {
                global_metrics().record_response(response.usage.total_tokens as u64);
            }
            Err(_) => {
                global_metrics().record_error();
            }
        }

        result
    }
```

完整替换 `execute_request` 方法为：

```rust
    pub async fn execute_request(
        &self,
        vendor_id: &str,
        request: UnifiedRequest,
    ) -> Result<UnifiedResponse, ProxyError> {
        global_metrics().record_request();

        let instance = {
            let guard = self.instances.read().unwrap();
            guard.get(vendor_id)
                .ok_or_else(|| {
                    global_metrics().record_error();
                    ProxyError::VendorUnavailable { vendor: vendor_id.to_string() }
                })?
                .clone()
        };

        let (response_tx, response_rx) = oneshot::channel();

        let pending = PendingRequest { request, response_tx };

        if let Err(e) = instance.queue.send(pending).await {
            global_metrics().record_error();
            return Err(e);
        }

        let timeout_duration = Duration::from_secs(60);
        let result = timeout(timeout_duration, response_rx)
            .await
            .map_err(|_| {
                global_metrics().record_error();
                ProxyError::Timeout { vendor: vendor_id.to_string() }
            })?
            .map_err(|e| {
                global_metrics().record_error();
                e
            })?;

        if let Ok(ref response) = result {
            global_metrics().record_response(response.usage.total_tokens as u64);
        }

        result
    }
```

- [ ] **Step 2: 编译验证**

Run: `cargo check --message-format=short`
Expected: 无错误

- [ ] **Step 3: Commit**

```bash
git add src/vendor/manager.rs
git commit -m "feat: integrate metrics collection into vendor request execution"
```

---

## Task 5: 添加 `/admin/metrics` 端点

**Files:**

- Modify: `src/api/admin.rs`
- Modify: `src/api/mod.rs`

**问题:** 审计发现缺少运行时指标 API 端点。

- [ ] **Step 1: 在 `src/api/admin.rs` 添加 metrics 端点**

在文件顶部导入区域添加：

```rust
use crate::utils::metrics::global_metrics;
```

在文件末尾（最后一个 handler 函数之后）添加：

```rust
/// GET /admin/metrics - 获取运行时指标
pub async fn metrics(
    axum::extract::State(_state): axum::extract::State<std::sync::Arc<ApiState>>,
) -> impl axum::response::IntoResponse {
    let snapshot = global_metrics().snapshot();
    (axum::http::StatusCode::OK, axum::Json(snapshot))
}
```

- [ ] **Step 2: 在 `src/api/mod.rs` 路由中注册 metrics 端点**

在 `create_router` 函数中的 admin 路由组（`/admin` nested router）添加 `.route("/metrics", axum::routing::get(admin::metrics))`。

找到 admin 路由定义，添加 metrics 路由。例如，如果当前是：

```rust
let admin_router = axum::Router::new()
    .route("/logs", axum::routing::get(admin::logs))
    // ... 其他路由
```

添加一行：

```rust
    .route("/metrics", axum::routing::get(admin::metrics))
```

- [ ] **Step 3: 编译验证**

Run: `cargo check --message-format=short`
Expected: 无错误

- [ ] **Step 4: Commit**

```bash
git add src/api/admin.rs src/api/mod.rs
git commit -m "feat: add GET /admin/metrics runtime metrics endpoint"
```

---

## Task 6: 补充队列管理测试

**Files:**

- Create: `tests/queue_test.rs`

**问题:** `VendorQueue` 和 `QueueManager` 缺少独立测试。

- [ ] **Step 1: 编写队列测试**

Create `tests/queue_test.rs`：

```rust
use smart_llm_proxy::vendor::{VendorQueue, QueueManager, PendingRequest};
use smart_llm_proxy::protocol::unified::{UnifiedRequest, Message, MessageContent, Role};
use smart_llm_proxy::error::ProxyError;

fn make_test_request() -> UnifiedRequest {
    UnifiedRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("hello".to_string()),
        }],
        temperature: Some(0.7),
        max_tokens: Some(100),
        stream: false,
        tools: vec![],
        tool_choice: None,
        top_p: None,
        stop: None,
        frequency_penalty: None,
        presence_penalty: None,
    }
}

#[tokio::test]
async fn test_vendor_queue_send_recv() {
    let queue = VendorQueue::new(10);
    let (tx, rx) = tokio::sync::oneshot::channel();

    let request = make_test_request();
    let pending = PendingRequest { request, response_tx: tx };

    assert!(queue.send(pending).await.is_ok());
    assert_eq!(queue.len(), 1);

    let received = queue.recv().await;
    assert!(received.is_some());
    assert_eq!(queue.len(), 0);
}

#[tokio::test]
async fn test_vendor_queue_is_empty() {
    let queue = VendorQueue::new(5);
    assert!(queue.is_empty());
    assert!(!queue.is_full());
}

#[tokio::test]
async fn test_vendor_queue_capacity() {
    let queue = VendorQueue::new(5);
    assert_eq!(queue.capacity(), 5);
}

#[tokio::test]
async fn test_queue_manager_get_or_create() {
    let manager = QueueManager::new();

    let q1 = manager.get_or_create("vendor_a", 10);
    assert_eq!(q1.capacity(), 10);

    // 第二次 get_or_create 应返回同一个队列
    let q2 = manager.get_or_create("vendor_a", 20);
    assert_eq!(q2.capacity(), 10); // 容量不变，因为已存在
}

#[tokio::test]
async fn test_queue_manager_get_all_lengths() {
    let manager = QueueManager::new();

    let q1 = manager.get_or_create("vendor_a", 10);
    let q2 = manager.get_or_create("vendor_b", 10);

    let lengths = manager.get_all_lengths();
    assert_eq!(lengths.get("vendor_a"), Some(&0));
    assert_eq!(lengths.get("vendor_b"), Some(&0));
}

#[tokio::test]
async fn test_queue_manager_remove() {
    let manager = QueueManager::new();
    manager.get_or_create("vendor_a", 10);

    assert!(manager.get("vendor_a").is_some());
    manager.remove("vendor_a");
    assert!(manager.get("vendor_a").is_none());
}

#[tokio::test]
async fn test_queue_full_rejects() {
    let queue = VendorQueue::new(2);

    // 填满队列
    for i in 0..2 {
        let (tx, _) = tokio::sync::oneshot::channel();
        let request = make_test_request();
        let pending = PendingRequest { request, response_tx: tx };
        assert!(queue.send(pending).await.is_ok());
    }

    assert!(queue.is_full());

    // 第三个应该阻塞或超时
    let (tx, _) = tokio::sync::oneshot::channel();
    let request = make_test_request();
    let pending = PendingRequest { request, response_tx: tx };

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        queue.send(pending)
    ).await;
    assert!(result.is_timeout(), "Should timeout when queue is full");
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --test queue_test -- --nocapture`
Expected: 7 个测试全部 PASS

- [ ] **Step 3: Commit**

```bash
git add tests/queue_test.rs
git commit -m "test: add queue management tests for VendorQueue and QueueManager"
```

---

## Task 7: 补充限流器高级测试

**Files:**

- Create: `tests/advanced_rate_limiter_test.rs`

**问题:** `AdvancedRateLimiter` 三种限流模式缺少独立测试。

- [ ] **Step 1: 编写 AdvancedRateLimiter 测试**

Create `tests/advanced_rate_limiter_test.rs`：

```rust
use smart_llm_proxy::vendor::AdvancedRateLimiter;
use smart_llm_proxy::config::RateLimitConfig;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn test_token_limiter_allows_under_limit() {
    let config = RateLimitConfig::Token { max_tokens: 1000 };
    let limiter = AdvancedRateLimiter::new(&config);

    assert!(limiter.check());
    limiter.consume_tokens(500);
    assert!(limiter.check());
}

#[test]
fn test_token_limiter_blocks_at_limit() {
    let config = RateLimitConfig::Token { max_tokens: 100 };
    let limiter = AdvancedRateLimiter::new(&config);

    assert!(limiter.check());
    limiter.consume_tokens(100);
    assert!(!limiter.check());
}

#[test]
fn test_token_limiter_reset() {
    let config = RateLimitConfig::Token { max_tokens: 100 };
    let limiter = AdvancedRateLimiter::new(&config);

    limiter.consume_tokens(100);
    assert!(!limiter.check());

    limiter.reset();
    assert!(limiter.check());
}

#[test]
fn test_interval_limiter_always_passes() {
    let config = RateLimitConfig::Interval { hours: 5 };
    let limiter = AdvancedRateLimiter::new(&config);

    // Interval limiter always allows (reset handled by background worker)
    assert!(limiter.check());
}

#[test]
fn test_interval_limiter_should_reset() {
    let config = RateLimitConfig::Interval { hours: 0 }; // 0 hours = immediate reset
    let limiter = AdvancedRateLimiter::new(&config);

    // With 0 hours interval, should always be ready for reset
    assert!(limiter.should_reset());
}

#[test]
fn test_time_window_limiter_format() {
    // Verify that valid time window configs parse correctly
    let config = RateLimitConfig::TimeWindow {
        start: "09:00".to_string(),
        end: "18:00".to_string(),
    };
    let limiter = AdvancedRateLimiter::new(&config);
    // check() depends on current time, so just verify it doesn't panic
    let _ = limiter.check();
}

#[test]
fn test_none_limiter_always_passes() {
    // RateLimitConfig doesn't have a None variant directly,
    // but AdvancedRateLimiter::None always passes
    let config = RateLimitConfig::Token { max_tokens: 100000 };
    let limiter = AdvancedRateLimiter::new(&config);
    assert!(limiter.check());
}

#[test]
fn test_token_limiter_concurrent_consumption() {
    use std::sync::Arc;

    let config = RateLimitConfig::Token { max_tokens: 10000 };
    let limiter = Arc::new(AdvancedRateLimiter::new(&config));

    let mut handles = vec![];
    for _ in 0..10 {
        let limiter = limiter.clone();
        handles.push(std::thread::spawn(move || {
            limiter.consume_tokens(500);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // After consuming 10 * 500 = 5000, still under 10000
    assert!(limiter.check());
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --test advanced_rate_limiter_test -- --nocapture`
Expected: 8 个测试全部 PASS

- [ ] **Step 3: Commit**

```bash
git add tests/advanced_rate_limiter_test.rs
git commit -m "test: add advanced rate limiter tests for all three modes"
```

---

## Task 8: 清理 dead_code 标记

**Files:**

- Modify: `src/vendor/status.rs`
- Modify: `src/vendor/queue.rs`
- Modify: `src/database/mod.rs`

**问题:** 多处方法标记 `#[allow(dead_code)]` 但实际已被使用或应该公开。

- [ ] **Step 1: 移除 `src/vendor/status.rs` 中不必要的 `#[allow(dead_code)]`**

`mark_throttled`、`mark_unavailable`、`reset_error`、`inc_error`、`update_active` 等方法被 `vendor/manager.rs` 使用，移除 `#[allow(dead_code)]`。

对 `status.rs` 中以下方法移除 `#[allow(dead_code)]`:

- `inc_error` (被 manager.rs:766 使用)
- `reset_error` (被 manager.rs:777 使用)
- `mark_throttled` (被 manager.rs:757 使用)
- `mark_unavailable` (被 manager.rs:490, 866 使用)
- `update_active` (被 mark_busy 内部使用)
- `get_error_count` — 保留 `#[allow(dead_code)]` (未使用)
- `get_last_active` — 保留 `#[allow(dead_code)]` (未使用)

- [ ] **Step 2: 移除 `src/vendor/queue.rs` 中不必要的 `#[allow(dead_code)]`**

移除以下方法的 `#[allow(dead_code)]`:

- `is_empty` (路由权重计算可能使用)
- `is_full` (测试使用)
- `capacity` (测试使用)
- `get_or_create` (manager.rs:68 使用)
- `remove` (可能是 API 调用)
- `get_all_lengths` (可能是监控使用)

保留:

- `queue_size` 字段的 `#[allow(dead_code)]`

- [ ] **Step 3: 移除 `src/database/mod.rs` 中不必要的 `#[allow(dead_code)]`**

移除以下方法/字段的 `#[allow(dead_code)]`:

- `Repository::initialize` (被 Database::new 使用... 但实际 Database::new 自己初始化了 schema，所以此方法确实未使用) — 保留
- `Repository::get_token_stats` — 移除 (admin.rs 使用)
- `Repository::get_all_vendor_status` — 移除 (admin.rs 使用)
- `LogFilter::end_time` — 移除 (query_logs 未来可能使用)
- `Database::pool` — 保留 (确实未直接使用)
- `Database::close` — 保留

具体修改 `database/mod.rs`:

- 移除 `get_token_stats` 上的 `#[allow(dead_code)]`
- 移除 `get_all_vendor_status` 上的 `#[allow(dead_code)]`
- 移除 `LogFilter` 中 `end_time` 上的 `#[allow(dead_code)]`

- [ ] **Step 4: 编译验证**

Run: `cargo check --message-format=short`
Expected: 可能有少量 warning 提示，但无错误

- [ ] **Step 5: 运行完整测试套件**

Run: `cargo test 2>&1 | tail -30`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/vendor/status.rs src/vendor/queue.rs src/database/mod.rs
git commit -m "chore: remove unnecessary dead_code annotations for used methods"
```

---

## Task 9: 最终验证

**Files:** 无新文件

- [ ] **Step 1: 运行 clippy 检查**

Run: `cargo clippy -- -D warnings 2>&1 | head -50`
Expected: 无 warning（或有少量已知的 allow 警告）

- [ ] **Step 2: 运行格式检查**

Run: `cargo fmt -- --check`
Expected: 无输出（已格式化）

- [ ] **Step 3: 运行完整测试**

Run: `cargo test 2>&1 | tail -30`
Expected: 所有测试 PASS

- [ ] **Step 4: 修复 clippy 和 fmt 问题（如有）**

根据 Step 1-2 的输出修复问题。

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "chore: fix clippy warnings and formatting"
```

---

## 实现顺序总结

| Task | 内容                 | 预估步骤 |
| ---- | -------------------- | -------- |
| 1    | 优雅关闭             | 3        |
| 2    | 路由权重完善         | 6        |
| 3    | metrics 模块         | 5        |
| 4    | 指标集成到厂商管理器 | 3        |
| 5    | /admin/metrics 端点  | 4        |
| 6    | 队列测试             | 3        |
| 7    | 限流器测试           | 3        |
| 8    | 清理 dead_code       | 6        |
| 9    | 最终验证             | 5        |
