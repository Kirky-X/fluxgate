# Fluxgate FAQ

本页汇总 Fluxgate（Rust LLM 路由网关）使用中的常见问题与真实怪癖，每条答案末尾附代码位置。系统性操作说明见 [用户指南](./USER_GUIDE.md)，项目总览见 [README](../README.md)。

## 📋 目录

- [路由与端点](#-路由与端点)
- [配置与热更新](#-配置与热更新)
- [认证与限流](#-认证与限流)
- [成本与统计](#-成本与统计)
- [韧性与路由行为](#-韧性与路由行为)

---

## 🌐 路由与端点

### ❓ `GET /health` 为什么 404？

因为健康检查的**注册路径是 `/api/v1/health`**。`src/api/health.rs:30-37` 的 `#[forge(version = "v1", path = "/health")]` 宏会自动给路径加 `/api/v1` 版本前缀后注册。

裸 `/health` 出现在**限流**白名单里（`is_quota_exempt_path` 十条：`/health`、`/healthz`、`/ready`、`/api/v1/health`、`/readyz`、`/metrics`、`/api-docs` 与 `/api-docs/*`、`/swagger-ui` 与 `/swagger-ui/*`，见 `src/api/router_builder.rs:55-64`）。认证侧是**另一份独立白名单**（`/health`、`/api/v1/health`、`/`、`/readyz`、`/metrics`、文档面两组，见 `src/middleware/auth.rs:159-168`）——两处各自独立、语义不共享；`/healthz` 与 `/readyz` 例外——由 sdforge 在认证层之后挂载，**免认证免限流**（实测无 key 返回 200），`/ready` 未注册（404）只在限流豁免表里留存。白名单只保证「不会被中间件拦」，不等于「有 handler」，而裸 `/health` **没有任何路由注册在它上面**。请统一使用：

```bash
curl http://127.0.0.1:8080/api/v1/health
```

> Docker 健康检查现只有**镜像层 `HEALTHCHECK` 一处定义**（compose 不再重复声明），已对齐该路径（此前误配为裸 `/health` 与两处 `start-period` 不一致已修正），详见 [用户指南 · Docker 部署](./USER_GUIDE.md#-docker-部署)。

**代码位置**：`src/api/health.rs:30-37`、`src/api/router_builder.rs:55-64`

### ❓ 如何指定某个厂商或模型？

用请求头（`X-Vendor` / `X-Model` 仅 OpenAI 兼容端点 `/v1/chat/completions` 消费——`/v1/messages` 保持无 hints 路由语义，忽略这两个头；`X-Session-Id` / `X-Weight` 两端点均支持）：

| 请求头 | 行为 |
|:-------|:-----|
| `X-Vendor: <vendor_id>` | 仅 `/v1/chat/completions`：直达指定厂商；**找不到该厂商直接报 Validation 错误**，不静默回退默认路由 |
| `X-Model: <abstract_id>` | 仅 `/v1/chat/completions`：优先按抽象模型路由（大小写不敏感）；本请求为显式指定，不接会话亲和 |
| `X-Session-Id: <key>` | 会话亲和键（两端点均支持）；缺失时兜底 `sha256(messages[0] 前 2KB)` 截断 hex |
| `X-Weight: <正数>` | 请求级权重（两端点均支持），需 `[routing].allow_request_weight = true` 才生效（默认忽略） |

**代码位置**：`src/api/routing_hints.rs`（头提取）、`src/api/openai.rs:692-698`（`hints.has_hints()` 时走 `route_with_hints`，X-Vendor 未命中即报错）、`src/api/anthropic.rs:64`、`:702`（保持无 hints 语义，走 `route_weighted`）

---

## ⚙️ 配置与热更新

### ❓ `POST /admin/config` 后为什么有的配置没生效？

因为各段生效方式不同。`server` / `cors` 在建站期被 sdforge 固化进 axum Router，`rate_limit` 在 Router 构造期消费，vendor `concurrency` 在 vendor 实例构造期消费，`database` 启动期建池，`cache.semantic` 的语义层在 bootstrap 装配一次——这些段**必须重启进程**才生效；其余段（`auth` / `retry` / `routing` / `cache`（精确缓存）/ `resilience` / `admission` / `quota` / `health` / `pricing` / `global_model` / `lexcompress`）每请求热读，下一请求即生效。

响应里的 `restart_required` 数组会列出需重启的段：`server` / `cors` / `rate_limit` / `database` / `grpc` / `global_model` / `cache.semantic` / `docs.enabled` / `routing.difficulty.enabled`，外加 `routing.difficulty.classifier` 的 `model_path` / `engine` / `cache_ttl_secs` 三个键（分类器 bootstrap 装配一次）。数组之外仍有两类需重启面请自行安排重启：vendor `concurrency`（vendor 实例构造期消费）与 `vendors` 拓扑。

**代码位置**：`src/api/admin.rs:333-395`（update_config 的 restart_required 组装）

### ❓ config.toml 被写回后自定义段消失了？

`persist` 采用**全量写回语义**：每次 `POST /admin/config` 落盘时以 `toml::to_string_pretty` 把整个 `Config` 结构体写回 config.toml。所有段必须定义在 `Config` 结构体里（`[resilience]` 等段的落地正是为此），**未进结构体的未知段在 TOML 反序列化时被静默丢弃**，首次热更写回后即从磁盘消失。

不要在 config.toml 里放 Fluxgate 不认识的自定义段；如需扩展，须先在 `src/config/mod.rs` 的 `Config` 结构体中加字段。

**代码位置**：`src/config/mod.rs`（`Config` 结构体字段文档注释中的「persist 全量写回」硬约束说明）

### ❓ `/admin/logs` 的 `end_time` 过滤是什么语义？

已修复。`/admin/logs?end_time=...` 的参数链路四个环节全部生效：API 层接收并透传（`src/api/admin.rs`）→ `LogFilter.end_time`（`src/storage.rs`）→ `Repository::query_logs` 的 SQL 分支（`AND timestamp <= ?`，经绑定参数传入）→ 数据库层过滤。时间单位为秒级 i64（与 `request_logs.timestamp` 存储一致）。

过滤语义为**闭区间** `[start_time, end_time]`，两端含边界——`timestamp = end_time` 的行会返回，与既有的 `start_time >=` 分支对称。

**代码位置**：`src/database/repository.rs`（`query_logs`，`end_time` 分支紧随 `start_time` 分支）

### ❓ `/admin/logs/search` 的 `vendor` 参数为什么是搜索关键词？

历史设计。该端点复用了 `LogQuery` 查询结构（其中 `vendor` 字段名沿用自 `/admin/logs`），但语义完全不同：`search_logs` 把 `params.vendor` 取出后直接当作**全文关键词**，对 `messages` 与 `error_message` 两列做 `LIKE %keyword%` 匹配，与厂商过滤无关。

```bash
# 搜索消息或错误信息里含 "timeout" 的日志
curl "http://127.0.0.1:8080/admin/logs/search?vendor=timeout"
```

**代码位置**：`src/api/admin.rs:54`（`let keyword = params.vendor.unwrap_or_default()`）

### ❓ 如何切换消息语言？

设置环境变量 **`FLUXGATE_LANG=en|zh`**（项目级覆盖，优先级最高）。未设置时按检测链回退：`LC_ALL` → `LC_MESSAGES` → `LANG` → 系统区域设置（sys-locale），全部无法识别回退 `en`。`zh_CN` / `zh_TW` 等 zh 系均映射到 `zh`。

**代码位置**：`src/i18n/locale.rs`（`detect_from` 检测链）

---

## 🔑 认证与限流

### ❓ 请求支持哪些认证头？

两种，按顺序取用：

| 优先级 | 请求头 | 形态 |
|:------:|:-------|:-----|
| 1 | `Authorization` | `Bearer <key>`（带前缀剥离） |
| 2 | `x-api-key` | `<key>` 原值 |

先尝试 `Authorization: Bearer`，取不到再读 `x-api-key`。key 经 SHA256 摘要后与 `[auth].api_keys` 做常量时间比较；`[[auth.keys]]` 未声明的 key 档位缺省 `elastic`。整个 `[auth]` 段缺省时认证禁用（放行）。

**代码位置**：`src/utils/tenant.rs`（`extract_api_key`，`src/middleware/auth.rs` re-export 同一实现）

### ❓ `POST /admin/vendors/:id/reset` 会做什么？

清空该厂商 key 池的运行时状态——密钥池黑名单、轮换游标与冷却状态（WP6 密钥池三态）。限流令牌本就会按速率自动 refill，无需手动清零；此端点针对的是**错误分类驱动的 key 黑名单与冷却**（401/402/403 及账务形态错误把 key 拉黑后，用它恢复）。出站重试/换路不受影响。未知 vendor 显性返回 `validation_error`(400)，成功后返回 `success: true` 与「密钥池黑名单与冷却状态已清空」提示。

**代码位置**：`src/api/admin.rs:241-256`（`reset_vendor` → `VendorManager::reset_key_pool_state`，`src/vendor/manager.rs`）

### ❓ 如何强制跳过响应缓存？

请求携带 **`X-No-Cache: 1`**（兼容别名 `X-Skip-Cache`），该请求不读也不写缓存，强制回源。注意前提：`[cache].enabled` 默认 `false`，缓存本就关闭；只有显式开启后才需要此头做按请求豁免。

**代码位置**：`src/api/response_cache.rs`（`X-No-Cache` 检测，111-114 行附近）

---

## 💰 成本与统计

### ❓ 成本统计为什么一直是 0？

两个独立原因，逐项排查：

1. **定价未命中**：成本按 `(vendor_id, model_id)` 精确匹配 `[[model_catalog]]` 的四档单价——catalog 是计价正源（`estimate_cost_from_catalog`），目录里没有该模型时直接返回 0（不阻塞响应）。`[[pricing]]` 只用于路由 SLA 因子与未登记模型的配额回退档，不是计价来源。
2. **观测位置**：算出非零成本后会按 `(vendor, model, api_key_hash, date, hour)` 写入 `token_usage` 表的四档成本列（`input_cost`/`output_cost`/`cache_read_cost`/`cache_creation_cost`），并可由 `GET /admin/cost` 按 vendor/model 分组聚合查询，跨重启保留；进程内 `CostTracker` 亦在内存累积但重启清零。注意逐请求日志 `request_logs.cost_usd` 恒为 `None` 是历史兼容列，**不代表成本没算**。若 `GET /admin/cost` 仍为 0，请回到第 1 项排查 `[[pricing]]` / `[[model_catalog]]` 定价命中。

**代码位置**：`src/vendor/cost.rs:152-183`（`estimate_cost_from_catalog` 目录未命中返回 `None`）+ 调用方映射（`src/api/mod.rs:437-469` 分账、`src/middleware/quota.rs:129-167` 配额）；`[[pricing]]` 路径（`CostCalculator::calculate`，`src/vendor/cost.rs:50-66`）在生产代码零调用，仅测试使用

---

## 🛡️ 韧性与路由行为

### ❓ 熔断器什么时候打开 / 恢复？

per-vendor 熔断器（limiteron CircuitBreaker）状态机：

| 状态 | 进入条件 / 行为 |
|:-----|:----------------|
| Closed（正常） | **连续 5 次失败** → 打开 |
| Open（打开） | 持续 **30 秒**，期间请求被快速拒绝并触发换路 |
| Half-open（试探） | 30 秒后放行试探请求，**连续 3 次成功** → 恢复 Closed |

慢调用防误判：慢调用判定线为 **60 秒 / 100% 比例**——LLM 成功调用常态超过通用默认（500ms），若不抬高，混合流量下慢调用率轻易过阈导致与真实失败无关的熔断；取 60s / 1.0 后慢调用路径在混合流量下失活，熔断由真实失败驱动。

**代码位置**：`src/vendor/manager.rs:135-145`（`gate_circuit_config`：`CircuitBreakerConfig::new(5, 3, 30s)` + 慢调用双参数）

---

## 🎯 还有其他问题？

| 文档 | 说明 |
|:-----|:-----|
| [用户指南](./USER_GUIDE.md) | 从编译到运维的完整操作路径 |
| [API 参考](./API_REFERENCE.md) | 端点与请求 / 响应格式 |
| [架构文档](./ARCHITECTURE.md) | 内部机制与模块设计 |
| [README](../README.md) | 项目总览 |
