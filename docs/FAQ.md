# NexusFlow FAQ

本页汇总 NexusFlow（Rust LLM 路由网关）使用中的常见问题与真实怪癖，每条答案末尾附代码位置。系统性操作说明见 [用户指南](./USER_GUIDE.md)，项目总览见 [README](../README.md)。

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

裸 `/health` 出现在认证与限流中间件的放行白名单里（`/health`、`/healthz`、`/ready`、`/api/v1/health` 四条，见 `src/api/router_builder.rs:163-167`），但**没有任何路由注册在它上面**——白名单只保证「不会被中间件拦」，不等于「有 handler」。请统一使用：

```bash
curl http://127.0.0.1:8080/api/v1/health
```

> Docker 的 `HEALTHCHECK` 与 `docker-compose` 探针现已对齐该路径（此前误配为裸 `/health` 已修正），详见 [用户指南 · Docker 部署](./USER_GUIDE.md#-docker-部署)。

**代码位置**：`src/api/health.rs:30-37`、`src/api/router_builder.rs:163-167`

### ❓ 如何指定某个厂商或模型？

用请求头（两个推理端点 `/v1/chat/completions` 与 `/v1/messages` 均支持）：

| 请求头 | 行为 |
|:-------|:-----|
| `X-Vendor: <vendor_id>` | 直达指定厂商；**找不到该厂商直接报 Validation 错误**，不静默回退默认路由 |
| `X-Model: <abstract_id>` | 优先按抽象模型路由（大小写不敏感）；本请求为显式指定，不接会话亲和 |
| `X-Session-Id: <key>` | 会话亲和键；缺失时兜底 `sha256(messages[0] 前 2KB)` 截断 hex |
| `X-Weight: <正数>` | 请求级权重，需 `[routing].allow_request_weight = true` 才生效（默认忽略） |

**代码位置**：`src/api/routing_hints.rs`（头提取）、`src/router/router.rs:279-341`（`route_with_hints`，X-Vendor 未命中即报错）

---

## ⚙️ 配置与热更新

### ❓ `POST /admin/config` 后为什么有的配置没生效？

因为各段生效方式不同。`server` / `cors` 在建站期被 sdforge 固化进 axum Router，`rate_limit` 在 Router 构造期消费，vendor `concurrency` 在 vendor 实例构造期消费，`database` 启动期建池——这些段**必须重启进程**才生效；其余段（`auth` / `retry` / `routing` / `cache` / `resilience` / `admission` / `pricing` / `global_model`）每请求热读，下一请求即生效。

响应里的 `restart_required` 数组会列出需重启的段，但**当前只检测 `server` 与 `cors` 两段**——改了 `rate_limit` / `concurrency` / `database` 后不会出现在数组里，请自行安排重启。

**代码位置**：`src/api/admin.rs:134-163`（update_config 的 restart_required 组装）

### ❓ config.toml 被写回后自定义段消失了？

`persist` 采用**全量写回语义**：每次 `POST /admin/config` 落盘时以 `toml::to_string_pretty` 把整个 `Config` 结构体写回 config.toml。所有段必须定义在 `Config` 结构体里（`[resilience]` 等段的落地正是为此），**未进结构体的未知段在 TOML 反序列化时被静默丢弃**，首次热更写回后即从磁盘消失。

不要在 config.toml 里放 NexusFlow 不认识的自定义段；如需扩展，须先在 `src/config/mod.rs` 的 `Config` 结构体中加字段。

**代码位置**：`src/config/mod.rs:830-839`（`[resilience]` 段文档注释中的硬约束说明）

### ❓ `/admin/logs` 传了 `end_time` 为什么没过滤？

已知限制。`LogFilter` 结构体有 `end_time` 字段，`/admin/logs` 也能接收该查询参数，但 `query_logs` 的 SQL 动态拼接**只实现了 `start_time` / `vendor` / `model` / `limit` 四个分支**，没有 `end_time` 对应的 `AND timestamp <= ?` 分支——参数被接收后未参与过滤。

**代码位置**：`src/database/repository.rs:153-185`（`query_logs`，可见无 end_time 分支）

### ❓ `/admin/logs/search` 的 `vendor` 参数为什么是搜索关键词？

历史设计。该端点复用了 `LogQuery` 查询结构（其中 `vendor` 字段名沿用自 `/admin/logs`），但语义完全不同：`search_logs` 把 `params.vendor` 取出后直接当作**全文关键词**，对 `messages` 与 `error_message` 两列做 `LIKE %keyword%` 匹配，与厂商过滤无关。

```bash
# 搜索消息或错误信息里含 "timeout" 的日志
curl "http://127.0.0.1:8080/admin/logs/search?vendor=timeout"
```

**代码位置**：`src/api/admin.rs:47`（`let keyword = params.vendor.unwrap_or_default()`）

### ❓ 如何切换消息语言？

设置环境变量 **`NEXUSFLOW_LANG=en|zh`**（项目级覆盖，优先级最高）。未设置时按检测链回退：`LC_ALL` → `LC_MESSAGES` → `LANG` → 系统区域设置（sys-locale），全部无法识别回退 `en`。`zh_CN` / `zh_TW` 等 zh 系均映射到 `zh`。

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

**代码位置**：`src/middleware/auth.rs:126-144`（`extract_api_key`）

### ❓ `POST /admin/vendors/:id/reset` 为什么是 no-op？

限流状态已由 limiteron `Governor` 自动管理——令牌按速率自动 refill，没有需要手动「清零」的状态。该端点仅为维持 API 兼容性保留，调用固定返回 `success: true` 与「限流由系统自动管理」提示，不做任何实际操作。

**代码位置**：`src/api/admin.rs:112-123`（`reset_vendor`，注释明确 no-op 语义）

### ❓ 如何强制跳过响应缓存？

请求携带 **`X-No-Cache: 1`**（兼容别名 `X-Skip-Cache`），该请求不读也不写缓存，强制回源。注意前提：`[cache].enabled` 默认 `false`，缓存本就关闭；只有显式开启后才需要此头做按请求豁免。

**代码位置**：`src/api/response_cache.rs`（`X-No-Cache` 检测，111-114 行附近）

---

## 💰 成本与统计

### ❓ 成本统计为什么一直是 0？

两个独立原因，逐项排查：

1. **定价表未命中**：成本按 `(vendor_id, model_id)` 精确匹配 `[[pricing]]` 定价表，未配置该段、或表中的 vendor/model 与实际路由结果不完全一致时，`calculate` 直接返回 `0.0`（不阻塞响应）。
2. **已知边界**：即使算出非零成本，当前也只累加进**内存 `CostTracker`**——请求日志表 `request_logs.cost_usd` 恒为 `None`（日志条目构造时固定填 `None`），且没有 `/admin/cost` 查询端点，跨重启无成本持久化查询。

**代码位置**：`src/vendor/cost.rs:55-71`（未命中返回 0.0）

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

**代码位置**：`src/vendor/manager.rs:150-155`（`nexusflow_circuit_config`：`CircuitBreakerConfig::new(5, 3, 30s)` + 慢调用双参数）

---

## 🎯 还有其他问题？

| 文档 | 说明 |
|:-----|:-----|
| [用户指南](./USER_GUIDE.md) | 从编译到运维的完整操作路径 |
| [API 参考](./API_REFERENCE.md) | 端点与请求 / 响应格式 |
| [架构文档](./ARCHITECTURE.md) | 内部机制与模块设计 |
| [README](../README.md) | 项目总览 |
