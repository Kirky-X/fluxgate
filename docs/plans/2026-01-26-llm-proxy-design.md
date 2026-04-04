# LLM 智能代理系统 - 设计文档

**版本**: v1.0  
**日期**: 2026-01-26  
**状态**: 设计阶段完成

---

## 一、项目概述

基于 Rust 的智能 LLM 路由代理系统，提供多厂商统一管理、格式转换、智能调度和监控能力。

---

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

---

## 三、模块结构

```
src/
├── main.rs                     # 应用入口，初始化所有组件
├── config/                     # 配置管理
│   ├── mod.rs                  # 模块导出 + ConfigManager
│   └── loader.rs               # TOML 配置解析
├── api/                        # API 层
│   ├── mod.rs                  # 路由聚合
│   ├── openai.rs               # OpenAI 兼容接口
│   ├── anthropic.rs            # Anthropic 兼容接口
│   └── admin.rs                # 监控管理接口
├── protocol/                   # 协议转换
│   ├── mod.rs                  # 统一数据模型导出 + ConverterRegistry
│   ├── unified.rs              # Internal Protocol 定义
│   ├── openai_converter.rs     # OpenAI 转换器
│   └── anthropic_converter.rs  # Anthropic 转换器
├── router/                     # 路由调度
│   ├── mod.rs                  # 路由决策入口 + Router + RouterCache
│   └── strategy.rs             # 路由策略（加权/轮询/优先级）
├── vendor/                     # 厂商管理
│   ├── mod.rs                  # 厂商管理器入口
│   ├── manager.rs              # VendorManager
│   ├── status.rs               # VendorState + VendorStatus
│   ├── rate_limiter.rs         # TokenBucket + ConcurrencyLimiter
│   └── queue.rs                # VendorQueue + QueueManager
├── database/                   # 数据持久化
│   ├── mod.rs                  # Database + Repository
│   └── schema.sql              # SQLite schema
└── utils/                      # 工具
    ├── mod.rs
    ├── retry.rs                # 重试逻辑
    └── metrics.rs              # 指标收集
```

---

## 四、关键设计决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 配置管理 | 静态加载 + 热重载 API | 核心功能优先，可扩展热重载 |
| 数据库 | dbnexus | PRD 指定，现代 async Rust SQLite |
| 流式响应 | 完整 SSE 支持 | 支持 OpenAI 和 Anthropic 双格式 |
| API Key | 环境变量 + 配置引用 | 灵活，容器化友好 |
| 日志级别 | DEBUG + 可调 | 调试友好 |
| 模型配置 | 模型内嵌厂商内 | 简化配置，模型与厂商不分离 |

---

## 五、数据流

### 5.1 请求处理流程

```
1. 客户端请求
   ├─ POST /v1/chat/completions (OpenAI 格式)
   └─ POST /v1/messages (Anthropic 格式)

2. API 层解析请求头
   ├─ Authorization: Bearer <api_key>
   ├─ X-Vendor: <vendor_id> (可选)
   └─ X-Model: <model_id> (可选)

3. 协议转换
   └─ 外部格式 → UnifiedRequest

4. 路由决策
   ├─ 直接路由: vendor.model 格式
   ├─ 抽象路由: 查找映射的厂商
   └─ 全局路由: 从全局模型池选择

5. 厂商执行
   ├─ 限流检查 (TokenBucket)
   ├─ 队列管理 (VendorQueue)
   └─ HTTP 请求 (reqwest)

6. 响应处理
   ├─ 协议转换: UnifiedResponse → 外部格式
   ├─ 流式处理: SSE 格式转换
   └─ 数据持久化: 写入 SQLite

7. 返回响应
```

### 5.2 路由决策流程

```
输入: abstract_model

├─ 格式 vendor.model?
│  └─ 是 → 直接路由到指定厂商
│
├─ 抽象模型存在?
│  └─ 是 → 过滤可用厂商 → 加权选择 → 抽象路由
│
└─ 全局模型启用?
    └─ 是 → 从所有模型中选择 → 全局路由
    └─ 否 → 返回错误
```

---

## 六、配置设计

### 6.1 配置文件结构

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 10

[[vendors]]
id = "anthropic_main"
name = "anthropic"
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
api_base = "https://api.anthropic.com"
enabled = true

[[vendors.models]]
model_id = "claude-3-5-sonnet-20241022"
abstract_id = "claude-3.5-sonnet"
weight = 1.0

[[vendors.models]]
model_id = "claude-3-haiku-20240307"
abstract_id = "claude-3-haiku"
weight = 0.8

[vendors.rate_limit]
type = "time_window"
start = "09:00"
end = "18:00"

[vendors.concurrency]
requests_per_second = 1

[global_model]
enabled = true
strategy = "weighted"
```

### 6.2 配置数据结构

```rust
struct Config {
    server: ServerConfig,
    database: DatabaseConfig,
    vendors: Vec<VendorConfig>,
    global_model: Option<GlobalModelConfig>,
}

struct VendorConfig {
    id: String,
    name: String,
    type_: String,
    api_key_env: String,
    api_base: String,
    enabled: bool,
    models: Vec<ModelConfig>,
    rate_limit: RateLimitConfig,
    concurrency: ConcurrencyConfig,
}

struct ModelConfig {
    model_id: String,
    abstract_id: Option<String>,
    weight: f64,
    enabled: bool,
}
```

---

## 七、协议转换

### 7.1 转换器架构

```
                    UnifiedRequest
                          ↑
         ┌────────────────┼────────────────┐
         ↓                ↓                ↓
    OpenAIConverter  AnthropicConverter  ...
         ↓                ↓
   OpenAI Request    Anthropic Request
         ↓                ↓
         └────────────────┴────────────────┐
                          ↓
                    HTTP 厂商请求
                          ↓
         ┌────────────────┼────────────────┐
         ↓                ↓                ↓
   OpenAI Response  Anthropic Response  ...
         ↓                ↓
         └────────────────┴────────────────┐
                          ↑
                    UnifiedResponse
```

### 7.2 支持的格式

| 方向 | OpenAI | Anthropic |
|------|--------|-----------|
| 请求格式 | `POST /v1/chat/completions` | `POST /v1/messages` |
| 响应格式 | `chat.completion` | `message` |
| 流式 | `data: {...}` | `event: content_block_delta` |
| 字段映射 | `max_tokens` | `max_tokens_to_sample` |

---

## 八、厂商状态机

```
Available ←───────→ Busy ←───────→ Available
   ↑                   ↓
   │                   ↓
   │              (请求处理中)
   │                   ↓
   ├──────────(429/5xx)───→ Throttled
   │                   ↓
   │              (指数回退)
   │                   ↓
   └───────(时间窗口重置)←─── Unavailable
   ↑
(400/401/403/404)
```

---

## 九、数据库 Schema

### 9.1 表结构

| 表名 | 说明 | 索引 |
|------|------|------|
| request_logs | 请求日志 | timestamp, vendor, model |
| token_usage | Token 预聚合统计 | vendor, model, date, hour |
| vendor_status | 厂商状态 | vendor (PK) |
| model_stats | 模型请求统计 | model, vendor, date |

### 9.2 Token 统计策略

- **实时**: 每次请求后更新
- **预聚合**: 按 (vendor, model, date, hour) 聚合
- **查询**: 支持按小时/天粒度查询

---

## 十、性能优化

| 优化项 | 策略 |
|--------|------|
| 零拷贝转换 | 同格式请求直接透传 |
| 连接池 | HTTP 客户端 + SQLite 连接池 |
| 异步日志 | 专用 channel，批量写入 |
| 懒加载队列 | 只为活跃厂商创建队列 |
| 路由缓存 | 相同请求缓存路由决策 |
| 状态机 | 避免重复状态查询 |

---

## 十一、错误处理

### 11.1 错误分类

| 错误类型 | 处理策略 |
|----------|----------|
| 400, 401, 403, 404 | 标记厂商 Unavailable，不重试 |
| 429 | 指数回退重试 (1s, 2s, 4s, 8s, 16s) |
| 500, 502, 503 | 重试 3 次，间隔 1s |
| 网络超时 | 重试 2 次 |
| 所有厂商不可用 | 返回 503 |

### 11.2 重试策略

```rust
retry_strategy = ExponentialBackoff {
    base_delay: 1s,
    max_delay: 16s,
    max_retries: 5,
}
```

---

## 十二、监控接口

### 12.1 日志查询

```
GET /admin/logs
  ?start_time=<timestamp>
  &end_time=<timestamp>
  &vendor=<vendor_id>
  &model=<model_id>
  &limit=100

GET /admin/logs/search
  ?q=<keyword>
  &limit=100
```

### 12.2 统计

```
GET /admin/stats/tokens
  ?vendor=<vendor_id>
  &model=<model_id>
  &date=<YYYY-MM-DD>
  &granularity=hour|day
```

### 12.3 厂商管理

```
GET  /admin/vendors
GET  /admin/vendors/{id}/status
POST /admin/vendors/{id}/reset
```

---

## 十三、依赖版本

```toml
[dependencies]
axum = "0.7"
tokio = { version = "1.42", features = ["full"] }
confers = "0.2"
dbnexus = "0.1.3"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
reqwest = { version = "0.12", features = ["json", "stream"] }
async-trait = "0.1"
futures = "0.3"
async-channel = "2.3"
inklog = "0.1"
chrono = { version = "0.4", features = ["serde"] }
anyhow = "1.0"
thiserror = "1.0"
rand = "0.8"
```

---

## 十四、实施计划

### 阶段 1: 项目骨架
- [ ] 创建项目结构
- [ ] 配置 Cargo.toml
- [ ] 实现配置加载 (config/)
- [ ] 实现统一数据模型 (protocol/unified.rs)

### 阶段 2: 协议转换
- [ ] 实现 OpenAI 转换器
- [ ] 实现 Anthropic 转换器
- [ ] 实现 ConverterRegistry
- [ ] 测试协议转换

### 阶段 3: 厂商管理
- [ ] 实现 VendorState 状态机
- [ ] 实现 TokenBucket 限流
- [ ] 实现 VendorQueue 队列
- [ ] 实现 VendorManager

### 阶段 4: 路由调度
- [ ] 实现 Router 路由决策
- [ ] 实现路由策略
- [ ] 实现 RouterCache 缓存

### 阶段 5: 数据持久化
- [ ] 创建 SQLite schema
- [ ] 实现 Repository 数据访问
- [ ] 集成数据库到请求流程

### 阶段 6: API 层
- [ ] 实现 OpenAI API 接口
- [ ] 实现 Anthropic API 接口
- [ ] 实现 Admin API 接口
- [ ] 端到端测试

---

## 十五、风险与缓解

| 风险 | 缓解措施 |
|------|----------|
| 配置错误导致启动失败 | 启动时验证配置 |
| 厂商 API 不稳定 | 重试 + 降级策略 |
| 数据库性能瓶颈 | 连接池 + 预聚合 |
| 内存泄漏 | 定期健康检查 |
| 安全问题 | API Key 环境变量 |

---

## 十六、总结

本设计提供：

- ✅ **统一抽象**: 内部协议屏蔽厂商差异
- ✅ **智能路由**: 多维度加权选择最优厂商
- ✅ **弹性管理**: 时间窗口 + 队列 + 重试机制
- ✅ **全面监控**: 请求日志 + Token 统计 + 状态追踪
- ✅ **性能优化**: 零拷贝 + 异步 + 连接池

---

*文档生成时间: 2026-01-26*
