[中文](README.md) | [English](README_EN.md)

# NexusFlow

智能 LLM 路由代理系统 - 多厂商统一管理、格式转换、智能调度

基于 Rust 2021 edition + Axum 0.8 + sdforge 构建，提供 OpenAI / Anthropic API 的统一网关。通过统一的协议转换层和智能路由调度，将多个上游 LLM 供应商抽象为单一接入点，支持加权路由、限流熔断、请求日志、Token 预聚合统计等生产级能力。

## 特性

- 多厂商统一接入：OpenAI / Anthropic 协议互转，客户端可用任一格式访问任一供应商
- 智能路由调度：基于 3D 权重公式 `model.weight × 0.4 + queue_factor × 0.3 + window_factor × 0.3` 加权随机选择
- 零拷贝透传：入站协议与出站协议相同时直接转发，跳过转换开销
- 智能重试：按状态码分类（429 指数回退、500 固定间隔）
- 限流熔断：基于 limiteron 的 GCRA 令牌桶 + 熔断器，支持全局限流和按端点限流
- 协议转换：OpenAI Chat / Embeddings ↔ Anthropic Messages 双向转换
- 请求日志：异步批量写入 SQLite，支持时间/厂商/模型过滤查询
- Token 预聚合：请求级 Token 使用统计聚合，避免全表扫描
- 可观测性：全局指标（请求数 / 延迟 / 错误率）、厂商状态、模型列表管理
- 配置热更新：通过 `/admin/config` 端点支持运行时配置变更
- 安全：SHA256 API Key 认证、CORS、rate limit 三层防护
- 生产就绪：Docker 多阶段构建、docker-compose 一键部署、GitHub Actions CI/CD

## 架构

```
                          ┌─────────────────────────────────────────────┐
                          │              NexusFlow Gateway              │
                          │            (Axum 0.8 + sdforge)             │
                          │                                             │
   Client ──HTTP────────► │  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
   (OpenAI /              │  │ Auth MW  │─►│ Rate MW  │─►│ Router   │  │
    Anthropic             │  │ (SHA256) │  │(limiteron)│  │(3D权重)  │  │
    格式)                 │  └──────────┘  └──────────┘  └────┬─────┘  │
                          │                                   │       │
                          │                ┌──────────────────┼─────┐ │
                          │                ▼                  ▼     │ │
                          │  ┌──────────────┐      ┌──────────────┐  │ │
                          │  │ Protocol Conv│      │VendorManager │  │ │
                          │  │  (零拷贝透传) │      │ (健康/重试)   │  │ │
                          │  └──────┬───────┘      └──────┬───────┘  │ │
                          └─────────┼─────────────────────┼──────────┘ │
                                    │                     │            │
                          ┌─────────▼─────────────────────▼──────────┐ │
                          │           Database (dbnexus)              │ │
                          │     ┌──────────────────────────────┐      │ │
                          │     │ SQLite (sqlx/sea-orm)        │      │ │
                          │     │ - request_logs (批量写入)     │      │ │
                          │     │ - token_usage (预聚合)        │      │ │
                          │     │ - vendor_stats               │      │ │
                          │     └──────────────────────────────┘      │ │
                          └────────────────────────────────────────────┘ │
                                    │                     │            │
                      ┌─────────────▼──────┐    ┌──────────▼─────────┐  │
                      │  OpenAI Vendor     │    │  Anthropic Vendor   │  │
                      │  (gpt-4, etc.)     │    │  (claude-3, etc.)   │  │
                      └────────────────────┘    └─────────────────────┘  │
```

## 快速开始

### 环境要求

- Rust 1.75+（edition 2021）
- SQLite（系统库或通过 rustls 编译）
- 兄弟库源码需与本项目同级目录（path 依赖）

### 编译运行

```bash
# 默认配置文件 config.toml
cargo run --release

# 指定配置文件
cargo run --release -- /path/to/config.toml

# 服务器监听 0.0.0.0:8080
```

### 首次请求

```bash
# 健康检查（无需认证）
curl http://127.0.0.1:8080/health

# OpenAI 兼容聊天补全
curl -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# Anthropic 兼容消息
curl -X POST http://127.0.0.1:8080/v1/messages \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-3-opus",
    "messages": [{"role": "user", "content": "Hello"}],
    "max_tokens": 1024
  }'
```

## 配置说明

运行时配置文件 `config.toml`，关键配置项：

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 5

# 认证：API Key 以 SHA256 哈希存储
# 生成方式：echo -n "your-api-key" | sha256sum
[auth]
enabled = true
api_keys = []  # 填入 SHA256 哈希

[auth.header]
name = "Authorization"
prefix = "Bearer"

[cors]
enabled = true
allowed_origins = ["*"]
allowed_methods = ["GET", "POST", "OPTIONS", "PUT", "DELETE"]

[rate_limit]
enabled = true
requests_per_second = 100
burst_size = 200
window_seconds = 60

# 按端点限流
[[rate_limit.endpoints]]
path = "/v1/chat/completions"
requests_per_minute = 1000

[[rate_limit.endpoints]]
path = "/v1/embeddings"
requests_per_minute = 500

[[rate_limit.endpoints]]
path = "/admin/*"
requests_per_minute = 100

# 厂商配置（支持多个）
[[vendors]]
id = "mock-openai"
name = "Mock OpenAI"
type_ = "openai"
api_key_env = "MOCK_OPENAI_KEY"   # 从环境变量读取实际 key
api_base = "http://127.0.0.1:8001/v1"
enabled = true
concurrency = { requests_per_second = 100 }
rate_limit = { type = "token", max_tokens = 10000000 }

[[vendors.models]]
model_id = "gpt-4"
enabled = true
weight = 1.0    # 路由权重，参与 3D 权重计算

[global_model]
enabled = true
strategy = "weighted"   # 全局加权路由策略
```

环境变量：通过 `api_key_env` 字段指定的名称从环境变量读取上游厂商的 API Key，避免硬编码到配置文件。

## API 端点

共 14 个路由（`/admin/config` 单路由支持 GET + POST 两个方法）：

### 业务接口

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/v1/chat/completions` | OpenAI 兼容聊天补全 |
| POST | `/v1/embeddings` | OpenAI embeddings |
| POST | `/v1/messages` | Anthropic 兼容消息 |

### 管理接口

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/admin/logs` | 请求日志查询（支持时间/厂商/模型过滤） |
| GET | `/admin/logs/search` | 日志关键词搜索 |
| GET | `/admin/stats/tokens` | Token 使用统计（预聚合） |
| GET | `/admin/metrics` | 全局指标（请求数/延迟/错误率） |
| GET | `/admin/vendors` | 厂商列表 |
| GET | `/admin/vendors/:id/status` | 厂商状态 |
| POST | `/admin/vendors/:id/reset` | 重置厂商（熔断器/限流器） |
| GET | `/admin/config` | 获取当前配置 |
| POST | `/admin/config` | 更新配置 |
| GET | `/admin/models` | 模型列表 |
| POST | `/admin/models/:id` | 更新模型配置 |

### 健康检查

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/health` | 服务健康检查（无需认证） |

## Docker 部署

```bash
# 构建镜像
docker build -t nexusflow:latest .

# 运行容器
docker run -d -p 8080:8080 -v $(pwd)/data:/app/data nexusflow:latest

# 或使用 docker-compose 一键部署
docker-compose up -d
```

数据持久化：SQLite 数据库文件挂载于 `/app/data` 目录，通过 `-v $(pwd)/data:/app/data` 映射到宿主机。

## 开发指南

### 项目结构

```
src/
├── api/               # REST 端点（axum handlers）
│   ├── admin.rs       # 管理接口
│   ├── anthropic.rs   # Anthropic 消息端点
│   ├── openai.rs      # OpenAI 聊天/embeddings 端点
│   ├── health.rs      # 健康检查（sdforge #[service_api]）
│   ├── router_builder.rs  # 路由构建 + 限流中间件
│   └── state.rs       # 共享状态
├── config/            # TOML 配置加载
├── database/          # SQLite 持久化（dbnexus）
├── middleware/         # 认证/CORS 中间件
├── protocol/          # OpenAI ↔ Anthropic 协议转换
├── router/            # 智能路由 + 3D 权重计算
├── vendor/            # 供应商管理（rate limit, retry, health）
├── infrastructure/    # DI 容器（trait-kit AsyncKit）
└── utils/             # 工具函数
```

### Cargo Features

默认启用 `full`，包含全部 8 个特性：

| Feature | 依赖 | 说明 |
|---------|------|------|
| `web-server` | axum, tower, tower-http, async-stream | HTTP 服务器 |
| `database` | dbnexus, sea-orm, async-channel | 数据库层 |
| `vendor-management` | reqwest, limiteron | 供应商管理 |
| `router` | - | 路由调度（依赖 vendor-management） |
| `protocol` | - | 协议转换 |
| `infrastructure` | oxcache, trait-kit | 缓存 + DI 容器 |
| `sdforge-integration` | sdforge | HTTP 框架集成 |
| `logging` | inklog | 结构化日志 |

### 编译检查

```bash
cargo clippy -- -D warnings   # 零告警检查（当前 0 warnings）
cargo build --release         # 生产构建（LTO + opt-level 3）
```

### 代码规范

- 错误处理：`anyhow` + `thiserror`，使用 `?` 操作符，错误必须显式传播
- 异步：Tokio runtime，所有 I/O 必须 async
- 命名：snake_case（Rust 惯例）
- 测试：`tests/` 集成测试 + `#[cfg(test)]` 内联单元测试

## 测试

```bash
# 运行所有测试
cargo test

# 基线：307 passed + 0 ignored + 0 failed
# clippy：0 warnings
```

测试覆盖：

| 模块 | 测试数 | 覆盖范围 |
|------|--------|----------|
| config | 8 | 配置加载/验证 |
| protocol | 7 | OpenAI ↔ Anthropic 转换 |
| vendor | 13 | rate limit / retry / health |
| router | 11 | 3D 权重 / 加权选择 / 边界条件 |
| database | 15 | 日志写入/查询/Token 聚合 |
| api | 22 | 端点集成测试 |
| middleware | - | 认证/CORS |

### 安全审查状态

| 审查项 | 结果 |
|--------|------|
| diting 代码审查 | ✅ 0 Critical / 0 High |
| tiangang SAST 扫描 | ✅ 0 CRITICAL 漏洞 |
| cargo-audit | ✅ 0 漏洞 |

## 兄弟库生态

本项目通过 Cargo path 依赖集成以下本地源码库（需与 NexusFlow 同级目录）：

| 库 | 作用 | 替代 |
|----|------|------|
| [dbnexus](../dbnexus) | SQLite 数据库层（含 kit 集成） | sqlx 直接使用 |
| [inklog](../inklog) | 结构化日志（HTTP/CLI/SQLite） | tracing-subscriber |
| [limiteron](../limiteron) | 限流 / 熔断（GCRA + circuit breaker） | 手写限流 |
| [oxcache](../oxcache) | 统一缓存 API（memory backend） | 手写缓存 |
| [sdforge](../sdforge) | HTTP 服务器框架 + API 网关 | axum 裸用 |
| [trait-kit](../trait-kit) | DI 容器（AsyncKit，异步依赖注入） | 手动 wiring |

所有兄弟库通过 `trait-kit` 0.3 的 AsyncKit DI 容器统一组装，`main.rs` 启动时会构建并验证 `4/4 modules resolved`。

## License

MIT License — 见源码文件头 `Licensed under the MIT License`。
