# LLM 配置管理和接口转换项目 - 技术架构设计

## 一、项目概述

这是一个基于 Rust 的智能 LLM 路由代理系统，提供多厂商统一管理、格式转换、智能调度和监控能力。

------

## 二、核心架构设计

### 2.1 系统分层架构



```
┌─────────────────────────────────────────────────────────┐
│                    API Gateway Layer                     │
│  (OpenAI API + Anthropic API + Monitor API)             │
└─────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────┐
│                 Request Processing Layer                 │
│  (Format Validation + Protocol Conversion)              │
└─────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────┐
│                  Routing Decision Layer                  │
│  (Vendor Selection + Model Mapping + Load Balancing)    │
└─────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────┐
│                  Vendor Management Layer                 │
│  (Rate Limiting + Queue + Retry + Health Check)         │
└─────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────┐
│                    Persistence Layer                     │
│  (Request Logging + Token Tracking + Analytics)         │
└─────────────────────────────────────────────────────────┘
```

------

## 三、技术栈选型

### 3.1 核心依赖

```toml
[dependencies]
# Web 框架 - 选择 axum (tokio 生态，性能优秀)
axum = "0.7"
tokio = { version = "1.42", features = ["full"] }
tower = "0.5"
tower-http = "0.6"

# 配置管理
confers = "0.2"

# 数据库
dbnexus = "0.1.3"

# 序列化
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# HTTP 客户端
reqwest = { version = "0.12", features = ["json", "stream"] }

# 异步工具
async-trait = "0.1"
futures = "0.3"

# 并发控制
tokio-util = "0.7"
async-channel = "2.3"

# 日志
inklog = "0.1"

# 时间处理
chrono = "0.4"

# 错误处理
anyhow = "1.0"
thiserror = "1.0"
```

------

## 四、核心模块设计

### 4.1 统一数据模型 (Internal Protocol)

```rust
// 内部统一请求结构
struct UnifiedRequest {
    model: String,              // 抽象模型 ID
    messages: Vec<Message>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: bool,
    // ... 其他通用参数
}

// 内部统一响应结构
struct UnifiedResponse {
    id: String,
    content: String,
    usage: TokenUsage,
    model: String,
    // ... 其他通用字段
}

// 消息结构
struct Message {
    role: Role,
    content: MessageContent,
}

enum MessageContent {
    Text(String),
    MultiModal(Vec<ContentBlock>),
}
```

### 4.2 协议转换器 (Protocol Converter)

```rust
trait ProtocolConverter {
    fn response_to_external(&self, req: UnifiedRequest) -> Result<HttpRequest>;
    fn to_unified(&self, resp: HttpResponse) -> Result<UnifiedResponse>;
}

// 实现转换器
struct OpenAIConverter;
struct AnthropicConverter;
```

**转换优化策略**:

- 入站协议 == 出站协议 → 直接透传（零拷贝）
- 入站协议 != 出站协议 → 两次转换（入站→统一→出站）

------

### 4.3 厂商配置管理

```toml
# config.toml 示例
[vendors.anthropic_1]
name = "anthropic"
type = "anthropic"
api_key = "sk-ant-xxx"
api_base = "https://api.anthropic.com"
rate_limit = { type = "time_window", start = "10:00", end = "15:00" }
# 或者: rate_limit = { type = "interval", hours = 5 }
# 或者: rate_limit = { type = "token", max_tokens = 1000000 }
concurrency = { requests_per_second = 1 }

[vendors.openai_1]
name = "openai"
type = "openai"
api_key = "sk-xxx"
rate_limit = { type = "interval", hours = 24 }
concurrency = { requests_per_second = 2 }

# 模型映射
[[models]]
abstract_id = "claude-3.5-sonnet"  # 自动转小写
vendors = [
    { vendor = "anthropic_1", model_id = "claude-3-5-sonnet-20241022" },
    { vendor = "anthropic_2", model_id = "claude-3-5-sonnet-20241022" }
]

[[models]]
abstract_id = "gpt-4"
vendors = [
    { vendor = "openai_1", model_id = "gpt-4-turbo-preview" }
]

# 全局抽象模型
[global_model]
abstract_id = "auto"  # 特殊标识
strategy = "weighted"  # 加权选择策略
```

---

### 4.4 智能路由调度器 (Smart Router)

#### 路由决策流程:

```
请求 → 解析模型 ID
  ├─ 具体厂商 + 模型 → 直接路由
  ├─ 抽象厂商 + 模型 → 多厂商选择
  └─ 全局抽象模型 → 全模型池选择

多厂商选择逻辑:
  1. 过滤可用厂商（状态 == Available）
  2. 计算每个厂商权重:
     - 时间窗口即将结束但有余额 → 高权重
     - 队列为空 → 高权重
     - Token 余额充足 → 高权重
  3. 加权随机选择
  4. 若选中厂商队列满 → 降级到下一个
```

#### 厂商状态机:

```
Available → (请求) → Busy → (完成) → Available
          ↓                     ↓
     (资源耗尽)            (429/5xx)
          ↓                     ↓
     Unavailable ← (时间窗口重置) ← Throttled
          ↑                           ↓
          └───── (下一时间窗口) ←──(指数回退)
```

------

### 4.5 并发控制 (Rate Limiter & Queue)

#### 设计方案:

1. **每厂商独立队列**: 使用 `async-channel` 创建有界队列
2. **懒加载**: 首次请求时初始化队列
3. **令牌桶算法**: 控制每秒请求数

```rust
struct VendorQueue {
    sender: Sender<PendingRequest>,
    receiver: Receiver<PendingRequest>,
    rate_limiter: TokenBucket,
    max_concurrency: usize,
}

struct TokenBucket {
    capacity: usize,
    tokens: AtomicUsize,
    refill_rate: Duration,
}
```

#### 队列处理器:

```rust
// 每个厂商一个后台任务
async fn process_vendor_queue(vendor: VendorConfig, queue: VendorQueue) {
    loop {
        // 等待令牌桶补充
        queue.rate_limiter.wait_for_token().await;
        
        // 从队列取任务
        let request = queue.receiver.recv().await;
        
        // 执行请求
        tokio::spawn(async move {
            execute_request(vendor, request).await;
        });
    }
}
```

------

### 4.6 智能重试与降级

#### 错误处理策略:

| 状态码             | 处理策略                            |
| ------------------ | ----------------------------------- |
| 400, 401, 403, 404 | 标记厂商 Unavailable，不重试        |
| 429                | 指数回退重试（1s, 2s, 4s, 8s, 16s） |
| 500, 502, 503      | 重试 3 次，间隔 1s                  |
| 网络超时           | 重试 2 次                           |

#### 抽象厂商降级:

```rust
async fn request_with_fallback(req: UnifiedRequest) -> Result<UnifiedResponse> {
    let vendors = select_vendors(&req.model);
    
    for vendor in vendors {
        match try_vendor(vendor, &req).await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_retryable() => continue,
            Err(e) => {
                mark_vendor_unavailable(vendor);
                continue;
            }
        }
    }
    
    Err(Error::AllVendorsUnavailable)
}
```

------

### 4.7 时间窗口管理

#### 三种窗口类型:

1. **时间段窗口** (`time_window`):

```rust
   struct TimeWindow {
       start: NaiveTime,  // 10:00
       end: NaiveTime,    // 15:00
   }
   
   fn is_in_window(&self) -> bool {
       let now = Local::now().time();
       now >= self.start && now < self.end
   }
```

1. **间隔窗口** (`interval`):

```rust
   struct IntervalWindow {
       interval: Duration,  // 5 hours
       last_reset: Instant,
   }
   
   fn should_reset(&self) -> bool {
       self.last_reset.elapsed() >= self.interval
   }
```

1. **Token 窗口** (`token`):

```rust
   struct TokenWindow {
       max_tokens: u64,
       used_tokens: AtomicU64,
   }
   
   fn can_request(&self, tokens: u64) -> bool {
       self.used_tokens.load(Ordering::Relaxed) + tokens <= self.max_tokens
   }
```

#### 窗口重置任务:

```rust
// 后台定时任务检查窗口重置
async fn window_reset_worker() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    
    loop {
        interval.tick().await;
        
        for vendor in get_all_vendors() {
            if vendor.should_reset_window() {
                vendor.reset();
                vendor.set_status(VendorStatus::Available);
            }
        }
    }
}
```

------

### 4.8 数据持久化 (SQLite)

#### 数据库 Schema:

```sql
-- 请求日志表
CREATE TABLE request_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT UNIQUE NOT NULL,
    timestamp INTEGER NOT NULL,
    
    -- 请求信息
    endpoint TEXT NOT NULL,           -- /v1/chat/completions 或 /v1/messages
    input_format TEXT NOT NULL,       -- openai 或 anthropic
    abstract_model TEXT,              -- 用户请求的抽象模型
    messages TEXT NOT NULL,           -- JSON 格式存储
    
    -- 路由信息
    actual_vendor TEXT NOT NULL,      -- 实际使用的厂商
    actual_model TEXT NOT NULL,       -- 实际使用的模型 ID
    routing_strategy TEXT NOT NULL,   -- direct/abstract/global
    
    -- 响应信息
    status_code INTEGER,
    response TEXT,                    -- 完整响应 JSON
    error_message TEXT,
    
    -- Token 统计
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    
    -- 性能指标
    duration_ms INTEGER,
    
    INDEX idx_timestamp (timestamp),
    INDEX idx_vendor (actual_vendor),
    INDEX idx_model (abstract_model)
);

-- Token 消耗统计表（预聚合）
CREATE TABLE token_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    date TEXT NOT NULL,              -- YYYY-MM-DD
    hour INTEGER NOT NULL,           -- 0-23
    
    request_count INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    prompt_tokens INTEGER DEFAULT 0,
    completion_tokens INTEGER DEFAULT 0,
    
    UNIQUE(vendor, model, date, hour)
);

-- 厂商状态表
CREATE TABLE vendor_status (
    vendor TEXT PRIMARY KEY,
    status TEXT NOT NULL,            -- available/unavailable/throttled
    last_updated INTEGER NOT NULL,
    window_used_tokens INTEGER DEFAULT 0,
    window_reset_at INTEGER,
    error_count INTEGER DEFAULT 0
);
```

#### 数据访问层:

```rust
struct Database {
    pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
}

impl Database {
    async fn log_request(&self, log: RequestLog) -> Result<()>;
    
    async fn query_logs(&self, filter: LogFilter) -> Result<Vec<RequestLog>>;
    
    async fn search_logs(&self, keyword: &str) -> Result<Vec<RequestLog>>;
    
    async fn get_token_stats(&self, filter: TokenStatsFilter) -> Result<TokenStats>;
    
    async fn update_vendor_status(&self, vendor: &str, status: VendorStatus) -> Result<()>;
}

struct LogFilter {
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    vendor: Option<String>,
    model: Option<String>,
    status_code: Option<u16>,
}
```

---

## 五、API 接口设计

### 5.1 LLM 接口

```
POST /v1/chat/completions          (OpenAI 格式)
POST /v1/embeddings                (OpenAI Embedding)
POST /v1/messages                  (Anthropic 格式)
```

**请求头扩展**:

```
X-Vendor: anthropic_1              (可选，指定具体厂商)
X-Model: claude-3.5-sonnet         (可选，使用抽象模型)
Authorization: Bearer <api_key>
```

### 5.2 监控接口

```
GET  /admin/logs                   查询请求日志
  ?start_time=<timestamp>
  &end_time=<timestamp>
  &vendor=<vendor_id>
  &model=<model_id>
  &limit=100

GET  /admin/logs/search            关键词搜索
  ?q=<keyword>
  &limit=100

GET  /admin/stats/tokens           Token 统计
  ?vendor=<vendor_id>
  &model=<model_id>
  &date=<YYYY-MM-DD>
  &granularity=hour|day

GET  /admin/vendors                厂商状态列表

GET  /admin/vendors/{id}/status    特定厂商状态

POST /admin/vendors/{id}/reset     手动重置厂商状态
```

------

## 六、关键技术挑战与解决方案

### 6.1 流式响应处理

**挑战**: OpenAI 和 Anthropic 的 SSE 格式不同

**解决方案**:

```rust
async fn stream_response(
    unified_stream: UnifiedResponseStream,
    output_format: Protocol,
) -> impl Stream<Item = Bytes> {
    unified_stream.map(move |chunk| {
        match output_format {
            Protocol::OpenAI => convert_to_openai_sse(chunk),
            Protocol::Anthropic => convert_to_anthropic_sse(chunk),
        }
    })
}
```

### 6.2 加权选择算法

```rust
fn weighted_select(vendors: Vec<VendorScore>) -> Option<String> {
    let total_weight: f64 = vendors.iter().map(|v| v.weight).sum();
    let mut rng = rand::thread_rng();
    let target = rng.gen::<f64>() * total_weight;
    
    let mut cumulative = 0.0;
    for vendor in vendors {
        cumulative += vendor.weight;
        if cumulative >= target {
            return Some(vendor.id);
        }
    }
    None
}

fn calculate_weight(vendor: &Vendor) -> f64 {
    let mut weight = 1.0;
    
    // 时间窗口即将结束但有余额 → 高权重
    if vendor.window_ending_soon() && vendor.has_budget() {
        weight *= 2.0;
    }
    
    // 队列空闲 → 高权重
    if vendor.queue_is_empty() {
        weight *= 1.5;
    }
    
    // Token 余额百分比
    weight *= vendor.budget_percentage();
    
    weight
}
```

### 6.3 并发安全

- 使用 `Arc<RwLock<>>` 管理共享状态
- 使用 `AtomicUsize` 跟踪 Token 消耗
- 数据库连接池避免竞争

------

## 七、性能优化策略

1. **零拷贝转换**: 同格式请求直接透传
2. **连接池复用**: HTTP 客户端使用连接池
3. **异步日志**: 日志写入使用专用 channel，批量写入
4. **懒加载队列**: 只为活跃厂商创建队列
5. **预聚合统计**: Token 统计按小时预聚合

------

## 八、配置示例

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 10

[[vendors]]
id = "anthropic_main"
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
api_base = "https://api.anthropic.com"
concurrency = 2
rate_limit = { type = "time_window", start = "09:00", end = "18:00" }

[[vendors]]
id = "openai_backup"
type = "openai"
api_key_env = "OPENAI_API_KEY"
concurrency = 1
rate_limit = { type = "token", max_tokens = 1000000 }

[[models]]
abstract_id = "sonnet"
vendors = [
    { vendor = "anthropic_main", model_id = "claude-3-5-sonnet-20241022", weight = 1.0 }
]

[global_model]
enabled = true
models = ["sonnet", "gpt-4"]
strategy = "weighted"
```

---

## 九、项目结构

```
src/
├── main.rs                 # 入口
├── config/                 # 配置管理
│   ├── mod.rs
│   └── loader.rs
├── api/                    # API 层
│   ├── openai.rs
│   ├── anthropic.rs
│   └── admin.rs
├── protocol/               # 协议转换
│   ├── unified.rs
│   ├── openai_converter.rs
│   └── anthropic_converter.rs
├── router/                 # 路由调度
│   ├── selector.rs
│   └── strategy.rs
├── vendor/                 # 厂商管理
│   ├── manager.rs
│   ├── queue.rs
│   ├── rate_limiter.rs
│   └── status.rs
├── database/               # 数据库
│   ├── mod.rs
│   ├── schema.sql
│   └── repository.rs
└── utils/                  # 工具
    ├── retry.rs
    └── metrics.rs
```

------

## 十、总结

这个架构设计提供了:

✅ **统一抽象**: 内部协议屏蔽厂商差异
 ✅ **智能路由**: 多维度加权选择最优厂商
 ✅ **弹性管理**: 时间窗口 + 队列 + 重试机制
 ✅ **全面监控**: 请求日志 + Token 统计 + 状态追踪
 ✅ **性能优化**: 零拷贝 + 异步 + 连接池

**核心创新点**:

1. 三层模型抽象（具体厂商 → 抽象厂商 → 全局模型）
2. 滑动窗口优先级调度
3. 协议透传优化
