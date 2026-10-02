# NexusFlow 安全文档

[🤝 贡献指南](./CONTRIBUTING.md) · [📜 更新日志](./CHANGELOG.md) · [📖 用户指南](./USER_GUIDE.md) · [🏗️ 架构文档](./ARCHITECTURE.md) · [📖 README](../README.md)

NexusFlow 将安全性作为核心设计目标。本文档介绍 NexusFlow 的安全策略、漏洞报告流程、内置安全机制与已知安全边界。

## 📋 目录

- [安全模型总览](#️-安全模型总览)
- [API Key 认证设计](#-api-key-认证设计)
- [上游密钥隔离](#️-上游密钥隔离)
- [入站限流与封禁](#-入站限流与封禁)
- [拥塞准入](#-拥塞准入)
- [出站弹性边界](#-出站弹性边界)
- [信息泄露控制](#️-信息泄露控制)
- [CORS](#-cors)
- [传输与部署](#-传输与部署)
- [供应链与 CI 门禁](#-供应链与-ci-门禁)
- [安全审查状态](#-安全审查状态)
- [已知安全边界](#️-已知安全边界)
- [漏洞报告流程](#-漏洞报告流程)

---

## 🛡️ 安全模型总览

一个请求进入 NexusFlow 后依次穿过五道防线，任何一道都可以独立拒绝请求：

```mermaid
flowchart LR
    C[客户端请求] --> A["认证层<br/>SHA256 摘要 + 常量时间比较<br/>src/middleware/auth.rs"]
    A --> RL["入站限流<br/>limiteron 全局规则 + 端点表<br/>src/api/router_builder.rs"]
    RL --> AD["拥塞准入<br/>档位优先级 shed（默认关）<br/>src/middleware/admission.rs"]
    AD --> R["路由与协议转换<br/>5 因子加权选择<br/>src/router/"]
    R --> O["出站弹性边界<br/>熔断 / 重试 / FallbackChain<br/>src/vendor/"]
    O --> V[(上游 LLM 厂商)]
```

各防线的职责与失效语义：

| 防线 | 拒绝方式 | 失效语义 |
|:-----|:---------|:---------|
| 认证 | 401 `authentication_error` | 认证关闭时全放行（须显式配置 `[auth]` 段） |
| 入站限流 | 429 + `Retry-After`；封禁 403 | 限流器内部故障时 fail-open 放行 |
| 拥塞准入 | 429 + `Retry-After: 1` | 默认关闭；关闭时判定旁路 |
| 路由 | 400 / 503 等（固定错误码） | 无可用候选时错误显性冒泡 |
| 出站弹性 | 熔断 Open 直接拒绝并换路 | 候选耗尽后原错误冒泡 |

健康检查路径在认证与限流两处均有独立白名单，永不卸载。

---

## 🔑 API Key 认证设计

认证实现位于 `src/middleware/auth.rs`，核心原则：**明文密钥不落盘、不驻内存、比较无时序侧信道**。

### 摘要存储

- 配置中的 `[auth].api_keys` 只存 **SHA256 hex 摘要**（64 个小写 hex 字符），不存明文；生成方式 `echo -n "your-api-key" | sha256sum`
- `ApiAuthState` 内部以 `HashSet<String>` 存摘要；编程接口 `add_api_key` 现场摘要后存储，明文即刻丢弃
- 单元测试以已知明文/摘要对锁定算法与编码格式

### 常量时间比较

校验时对请求 key 现场计算 SHA256 摘要，再与存储摘要逐字节 XOR 累积比较（`constant_time_eq`），消除时序侧信道。摘要长度恒为 64 hex，前置长度检查不泄露有效信息。key 档位查询（`tier_for_key`）复用同一常量时间比较。

### 请求头提取

| 顺序 | 来源 | 说明 |
|:-----|:-----|:-----|
| 1 | `Authorization: Bearer <key>` | 主通道（OpenAI / Anthropic 兼容） |
| 2 | `x-api-key` | 备用通道 |

### 热读与即时生效

认证中间件的 State 是 `Arc<ConfigManager>`：**每请求从热配置现读 `[auth]` 段** 构造认证状态。key 吊销、轮换与 `enabled` 开关经 `POST /admin/config` 更新后立即生效，不存在「更新成功但旧 key 集仍生效到重启」的静默失效。

### Key 档位

可选 `[[auth.keys]]` 子表声明 key 档位（SHA256 摘要 → 档位），作为拥塞准入的优先级基准：

| 档位 | 语义 | 基准权重 |
|:-----|:-----|:--------:|
| `dedicated` | 专属容量 | 1000 |
| `guaranteed` | 保证档 | 100 |
| `elastic`（缺省） | 弹性档 | 1 |
| `spot` | 低价可抢占档 | 0.1 |

未在 `keys` 表声明的 key（含 `api_keys` 全部）缺省 `elastic`。

### 部署要求

**`[auth]` 段缺省等于认证禁用**（配置中无该段时 `enabled=false`，所有请求放行）。生产部署必须显式配置 `[auth]` 段。认证侧白名单仅 `/health`、`/api/v1/health`、`/`；其余路径（含全部 `/admin/*`）均需认证。

---

## 🗝️ 上游密钥隔离

上游厂商的 API Key 与网关自身的认证 key 完全隔离：

- 厂商密钥在配置中只存**环境变量名**（`[[vendors]].api_key_env`，如 `"OPENAI_API_KEY"`），真实密钥经环境变量在运行时注入，**不落配置文件**（`src/config/mod.rs` 的 `VendorConfig`）
- 配置快照、`GET /admin/config` 返回值中只出现变量名，不出现密钥本体

---

## 🚦 入站限流与封禁

入站限流基于 limiteron 实现（`src/api/router_builder.rs`），两层判定：

| 层 | 消费配置 | 实现 | 分桶维度 |
|:---|:---------|:-----|:---------|
| 全局规则 | `[[rate_limit.rules]]` 中 `path` 缺省的规则 | 构造期转 limiteron `Rule` 注入 Governor | 规则级单例桶 |
| 端点规则 | `[[rate_limit.rules]]` 中带 `path` 前缀的规则 | 中间件侧端点表（`ShardedSlidingWindowLimiter` 惰性创建） | 按租户（调用方 API key 的 SHA256 摘要）分桶，可经 `per_tenant` 关闭 |

响应语义：

| 判定 | 响应 |
|:-----|:-----|
| 端点规则超限 | 429 + `Retry-After`（取窗口重置剩余秒数） |
| 全局规则 `Decision::Rejected` | 429 |
| 全局规则 `Decision::Banned` | **403 "Access denied"** |

内存防护：端点表条目有硬上限（1024）——`[auth]` 关闭时攻击者伪造任意 Bearer key 也无法无限建桶，超限后新租户退化到规则级 `__overflow__` 共享单桶。

健康检查白名单（限流侧，独立于认证侧）：`/health`、`/healthz`、`/ready`、`/api/v1/health`。

> 注意：`[rate_limit]` 段在 Router 构造期消费，**restart-required**——`POST /admin/config` 更新该段不热生效。

---

## 🚪 拥塞准入

拥塞感知优先级准入位于 `src/middleware/admission.rs`，**默认关闭**（`[admission].enabled` 默认 `false`，热读，可经 `POST /admin/config` 立即开启）。

工作方式：

- 拥塞信号 = 网关侧并发占用率 = Σ in_flight / Σ 并发上限，达到 `[admission].congestion_threshold`（默认 0.9）视为拥塞
- 拥塞时新请求的档位优先级与「当前在途最小档」比较：**低于者直接 429 + `Retry-After: 1`**（不排队）；同档/高档照常放行（同档内保持先到先得）
- 优先级公式：`w_tier·(1+α_burst·b)⁻¹·(1+α_debt·d)⁻¹`（SLO 缺口维度 α_slo 为配置预留，当前恒不生效）
- b / d 为 per-key 的 burst（到达速率）/ debt（被 shed 频度）EWMA，`AtomicU64` 定点（×2¹⁶）记账；per-key 表条目上限 1024，超限共享溢出桶
- per-tier 在途计数经 guard 的 `Drop` 释放，panic 展开亦不泄漏
- 健康检查在限流白名单头部放行，**永不卸载**

语义定位：网关侧近似、粗分类的 shed 顺序控制，不承诺精确的 TTFT 服务水平。

---

## 🔄 出站弹性边界

对上游厂商的调用由三层弹性机制包裹（`src/vendor/`），防止单厂商故障演变为网关级不可用或重试风暴。

### 熔断器

`limiteron::circuit` 三态机（生产配置单一构造点 `nexusflow_circuit_config()`，见 `src/vendor/manager.rs`）：

| 参数 | 值 | 语义 |
|:-----|:---|:-----|
| failure_threshold | 5 | 窗口内 5 次失败 → Open |
| open 时长 | 30s | 到期转 Half-Open |
| half-open 探针 | 3 | 探针成功恢复 Closed |
| 慢调用判定 | 60s / 100% | 慢调用路径仅在窗口内 100% 调用皆 >60s 时可能触发，熔断由真实失败驱动 |

### 重试

`[retry]` 段（worker 每请求热读）：

- 可重试状态码集合默认 `[429, 500, 502, 503]`；429 走指数退避（尊重 `Retry-After`），5xx 走固定间隔
- 退避带抖动（`jitter` 默认 0.2）与同步惊群展宽（`spread_jitter` 默认 1.0，多实例同刻重试错峰）
- 单 vendor 内最大重试次数默认 3

### 换路 FallbackChain

- 链由 `[[vendors]].priority` 构建（数值越小优先级越高），失败 vendor 进排除集后重路由
- 跨 vendor 总尝试预算 `max_total_attempts` **默认 8**，防「vendor 数 × 重试数」爆炸
- 可选单请求重试时间预算 `retry_budget_secs`（默认 0 = 不限时）
- 三层出站超时兜底：per-attempt `timeout_ms`（默认 60s）、流式 TTFT `stream_ttft_ms`（默认 60s）、流式 chunk 间 idle `stream_idle_ms`（默认 30s）——上游挂起不能无限占用并发 permit

---

## 🕵️ 信息泄露控制

### 错误响应

错误响应只暴露**固定的 `error.type` 代码**，不暴露内部枚举结构与堆栈（`src/error.rs`）：

| error.type | HTTP |
|:-----------|:-----|
| `config_error` | 500 |
| `protocol_error` | 502 |
| `vendor_unavailable` | 503 |
| `timeout` | 504 |
| `rate_limited` | 429 |
| `database_error` | 500 |
| `validation_error` | 400 |
| `routing_error` | 500 |
| `all_vendors_throttled` | 429（附 `Retry-After` 头） |

`error.message` 走 i18n 本地化轨道（Fluent，随 locale）；内部 `Display` 规范串仅进日志/错误链，不出网关。

### 配置快照打码

`POST /admin/config` 更新前自动落盘旧配置快照至 `config-backups/`（时间戳命名，保留 10 份，超量自动清理）；其中 `auth.api_keys` 经 confers Redacted 序列化**自动打码**（key 本身已是摘要，打码为双保险）——见 `src/config/manager.rs:18-63`。

### 管理端点

`/admin/config`（GET/POST）与其他全部 `/admin/*` 路由都在认证中间件之下，**需认证访问**；白名单仅健康检查路径。

---

## 🌐 CORS

`[cors]` 段在服务**构造期一次性消费并固化**（`src/main.rs` 组装 sdforge 配置时挂载），`POST /admin/config` 修改 `[cors]` 仅提示 restart-required，**变更需重启**。

安全相关行为：

- sdforge 构造校验强制 origin 带 `http(s)://` 前缀且拒绝空列表
- 通配 `"*"` 会被过滤；过滤后为空则**不挂 CORS 层**（网关场景由调用方处理跨域）
- 默认 `allow_credentials = false`、`max_age = 86400`

---

## 📦 传输与部署

| 项 | 事实 |
|:---|:-----|
| 数据库 TLS | dbnexus 以 `runtime-tokio-rustls` 特性编译（rustls 栈） |
| 网关自身 TLS | 不经 sdforge 终结 TLS——自建 axum 明文监听，**TLS 由外部反向代理承担**（部署责任） |
| 容器运行时 | `alpine:3.21` 最小化镜像，多阶段构建 |
| 非 root | 专用用户 `appuser`（UID/GID 10001），无 home、无登录 shell |
| 数据卷 | `/app/data`（SQLite 数据库），属主 appuser |
| 健康检查 | Dockerfile `HEALTHCHECK` 探测 `/health` |

---

## 🔗 供应链与 CI 门禁

`.github/workflows/ci.yml` 自动化门禁：

| 门禁 | 命令/动作 | 执行时机 |
|:-----|:----------|:---------|
| 格式检查 | `cargo fmt --check --all` | 每次 push / PR |
| 静态检查 | `cargo clippy --all-features -- -D warnings` | 每次 push / PR |
| 依赖漏洞扫描 | `cargo audit`（actions-rs/audit-check） | 每次 push / PR + **每日定时**（cron `0 0 * * *`） |
| 全量测试 | `cargo test --all-features --all` | 每次 push / PR |
| 镜像构建 | Docker 多阶段构建（main 分支 push） | main 分支 |
| 镜像漏洞扫描 | Trivy（SARIF 结果上传 GitHub code scanning） | 每次 Docker 构建后 |

工具链锁定 `RUST_VERSION=1.92`（`dtolnay/rust-toolchain@stable`），构建产物可复现。

---

## ✅ 安全审查状态

以下为项目自审记录（见 [README · 安全审查状态](../README.md#安全审查状态)）：

| 审查项 | 结果 |
|:-------|:-----|
| diting 代码审查 | ✅ 0 Critical / 0 High |
| tiangang SAST 扫描 | ✅ 0 CRITICAL 漏洞 |
| cargo-audit 依赖审计 | ✅ 0 漏洞 |

自审记录不等同于第三方审计；如发现安全问题请走下方漏洞报告流程。

---

## ⚠️ 已知安全边界

如实声明当前版本的能力边界，部署者应据此评估风险：

| 边界 | 说明 | 部署建议 |
|:-----|:-----|:---------|
| 成本聚合仅存内存 | `CostTracker`（by_vendor / by_model 聚合，`src/utils/cost_tracker.rs`）仅在内存累积，**重启清零**；逐请求 `cost_usd` 已随 `request_logs` 落库 | 需要精确长期成本账时以 `request_logs` / `token_usage` 表为准 |
| 请求消息明文落库 | `request_logs.messages` 列**明文存储请求消息**（含用户对话内容，见 `src/database/schema.sql`） | 部署者需自行加密磁盘（如 LUKS / 云盘加密）并严格控制数据库文件访问权限 |
| 日志搜索为 LIKE 查询 | `GET /admin/logs/search` 对 `messages` / `error_message` 做关键词 LIKE 匹配；查询经参数化绑定（`Statement::from_sql_and_values`），**无 SQL 注入**，但关键词匹配会放大敏感内容读取面 | 限制 `/admin/*` 的网络暴露面，认证 key 仅发给可信运维 |

---

## 🐛 漏洞报告流程

我们非常重视安全漏洞。如果您发现安全问题，请负责任地进行报告。

### 如何报告

**请勿针对安全漏洞创建公开的 GitHub Issue。**

| 方式 | 渠道 | 响应时间 |
|:-----|:-----|:---------|
| **GitHub 安全通告（推荐）** | [通过 GH Advisory 私密报告](https://github.com/Kirky-X/NexusFlow/security/advisories/new) | 48 小时内确认 |

### 报告内容

1. **描述**：清晰描述漏洞本身
2. **复现步骤**：详细的问题复现步骤
3. **影响评估**：该漏洞可能如何被利用
4. **受影响版本**：哪些版本受到影响
5. **修复建议**（可选）：如果您已想到潜在修复方案

### 我们的承诺

| 阶段 | 时间线 | 动作 |
|:-----|:-------|:-----|
| **确认收到** | 48 小时内 | 确认收到报告 |
| **初步评估** | 7 天内 | 完成严重程度定级 |
| **修复开发** | 视严重程度而定 | 按优先级实施修复 |
| **协同披露** | 修复可用之后 | 发布公开公告 |

---

## 📚 相关文档

| 文档 | 说明 |
|:-----|:-----|
| [🤝 贡献指南](./CONTRIBUTING.md) | 开发环境与提交流程 |
| [📜 更新日志](./CHANGELOG.md) | 安全修复随版本发布记录 |
| [📖 用户指南](./USER_GUIDE.md) | 安全配置实践 |
| [🏗️ 架构文档](./ARCHITECTURE.md) | 防线在整体架构中的位置 |
