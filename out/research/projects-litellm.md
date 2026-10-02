# 同类项目调研：LiteLLM 与 One API / New API

**调研日期**：2026-09-30
**调研对象**：
| 项目 | 仓库 | Stars | 主语言 | 最近推送 |
|------|------|-------|--------|----------|
| LiteLLM | [BerriAI/litellm](https://github.com/BerriAI/litellm) | 59,875 | Python(SDK) + Rust(核心) + TS(UI) | 2026-09-29 |
| New API | [QuantumNous/new-api](https://github.com/QuantumNous/new-api) | 49,094 | Go + React 19 | 2026-09-28 |
| One API | [github.com/songquanpeng/one-api](https://github.com/songquanpeng/one-api) | 37,045 | Go + JS | 2026-01-09 |

> 元信息来源：GitHub API（`curl -s https://api.github.com/repos/<owner>/<repo>`，2026-09-30 实测）。

**证据规则**：下文每个特性都标注了真实打开过的 URL 或本地代码 `path:line`。GitHub API / raw.githubusercontent.com / docs.litellm.ai 在本次会话中均可访问（WebFetch 超时，全部改用 `curl` 实测可达）。

---

## 一、LiteLLM (BerriAI/litellm)

### 1.1 定位与技术栈

- **定位**："开源 AI Gateway，统一接口调用 100+ LLM 提供商，OpenAI 格式为主"。两种形态：**Python SDK**（库内嵌路由）与 **Proxy Server（AI Gateway）**（中心化网关）。来源：https://raw.githubusercontent.com/BerriAI/litellm/main/README.md
- **技术栈**：GitHub 仓库描述为 "The fastest, litest AI Gateway. **Rust core** with Python SDK"（https://api.github.com/repos/BerriAI/litellm）；语言构成 Python 102MB / TypeScript 15MB / Rust 4.2MB（GitHub languages API 实测）。README 宣称 **1k RPS 下 P95 延迟 8ms**，Docker stable 镜像发布前经过 12 小时负载测试，镜像用 cosign 签名。
- **部署生态**：官方 Terraform 模块（AWS ECS Fargate + Aurora + ElastiCache；GCP Cloud Run + Cloud SQL + Memorystore），README 直接内嵌。

### 1.2 端点覆盖（比 NexusFlow 宽得多）

README 列出支持 `/chat/completions`、`/responses`、`/embeddings`、`/images`、`/audio(speech/transcriptions)`、`/moderations`、`/batches`、`/rerank`、`/messages`(Anthropic 原生)、**A2A Agent 网关**、**MCP 网关**（把 MCP server 挂到 `/mcp/`，chat 请求里可以直接引用工具）。NexusFlow 目前只有 chat 类 + embedding 转换。
来源：https://raw.githubusercontent.com/BerriAI/litellm/main/README.md

### 1.3 路由与调度（docs.litellm.ai/docs/routing，下同）

打开页面：https://docs.litellm.ai/docs/routing

**五种路由策略**，默认 `simple-shuffle`（加权随机，官方推荐生产使用，"usage-based 路由因 Redis 操作有显著延迟，不推荐生产"）：

| 策略 | 机制 | 与 NexusFlow 对比 |
|------|------|------------------|
| simple-shuffle（默认） | 按 `rpm`/`weight` 加权随机；无 rpm/weight 时纯随机 | ≈ NexusFlow `weighted_select`（router/router.rs:222） |
| latency-based-routing | 选响应时间最低的部署；可配 `ttl`（延迟统计时间窗）和 `lowest_latency_buffer`（如 0.5 → 最快档 50% 以内的都算候选，避免打爆最快的那个） | **NexusFlow 没有**延迟维度 |
| usage-based-routing-v2 | 选当分钟 TPM 用量最低的部署；用 Redis `incr`/`mget` 跨实例统计；超 tpm/rpm 限制的部署被过滤 | NexusFlow window_factor 只算供应商侧窗口余额，不算自身累计用量 |
| least-busy | 选在途请求数最少的部署 | ≈ NexusFlow queue_factor（queue_len 降权） |
| cost-based-routing | 选单价最低的部署；支持 `input_cost_per_token`/`output_cost_per_token` 自定义定价；不在价目表的按 $1 兜底 | **NexusFlow 3D 权重没有成本维度** |
| 自定义策略 | `CustomRoutingStrategyBase` 插件接口 | NexusFlow 无策略插件点 |

** Routing Groups（按模型组分策略）**：不同 `model_name` 可以绑不同策略与参数（如 gpt-5.6-terra 走 latency-based、cheap-model 走 simple-shuffle）；组名本身可作为可调用虚拟模型出现在 `/v1/models` 里，客户端模型发现能直接列出。
**Session Affinity（粘性会话）**：`optional_pre_call_checks: ["session_affinity"]` + `deployment_affinity_ttl_seconds`；从 `x-litellm-session-id`（或任意 `x-*-session-id`、body `metadata.session_id`）取会话 ID，首次请求按策略选部署后固定下来，TTL 每次刷新（是空闲 TTL 不是会话长度）；pin 存 Router cache，配了 Redis 就跨实例共享；被 pin 的部署进冷却时放行给其它健康部署但保留 pin。**用途直指供应商侧 prompt cache 命中**（同一会话打到同一部署才能吃到上游 prompt cache 折扣）。
**Traffic Mirroring（流量镜像）**：生产流量旁路复制到"沉默模型"做评估，不影响主请求延迟与结果（A/B 测试）。
**Deployment Ordering**：`order: 1/2/3` 优先级分层；请求先打 order=1 层，失败后自动升级 order=2 层，**每层内各自完成重试次数才升级**；全部层级耗尽才走 fallbacks。
**Weighted Failover**：`enable_weighted_failover` —— 失败部署 ID 加入排除集，在同一 model group 内按剩余权重重归一化重挑；排除集跨跳累积；`max_fallbacks` 默认 5；ContextWindowExceeded/ContentPolicyViolation 不走此路径（有专属 fallback）。
**Max Parallel Requests**：每部署并发上限，超限**立即 429 拒绝而非排队**（旧版排队会导致流量被静默串行化）；该 429 不计入冷却，router 对它的处理与普通 429 一致（可 failover 到同组其它部署）。
**Pre-call Checks**：发请求前过滤——上下文窗口装不下本次 messages 的部署、不满足 EU 区域要求的部署。
**冷却（Cooldowns）**：按**部署**（不是整个模型组）冷却；触发条件：429 立即冷却、当分钟失败率 >50%、401/404/408 不可重试错误；默认 `allowed_fails: 3` 次每分钟、`cooldown_time: 5s`；支持部署级覆盖（`model_info.allowed_fails` / `cooldown_time`，让不稳定的三方端点用更短的熔丝）。
**重试策略按错误类型细分**：`RetryPolicy`（AuthenticationErrorRetries/TimeoutErrorRetries/RateLimitErrorRetries/ContentPolicyViolationErrorRetries/BadRequestErrorRetries/NotFoundErrorRetries/InternalServerErrorRetries/ServiceUnavailableErrorRetries + DefaultRetries，未设置字段向下层让位）；`AllowedFailsPolicy` 按错误类型设置不同的冷却阈值。**重试次数四级优先链**：`x-litellm-num-retries` header > 请求体 > 部署 litellm_params > 全局默认——调用方可单请求覆盖（含设 0 禁用）。还有一个关键工程细节：router 拥有重试时会**把 provider SDK 内部 max_retries 钉为 0**，避免 (1+N)² 次放大。
**Slack 告警**：`AlertingConfig(webhook_url, alerting_threshold)` 对 LLM 异常和慢响应发告警。

### 1.4 跨模型组 Fallback

打开页面：https://docs.litellm.ai/docs/proxy/reliability

- **三类 fallback**：`fallbacks`（一般错误如 RateLimitError）、`context_window_fallbacks`（上下文超限错误，LiteLLM 跨供应商归一化识别该错误）、`content_policy_fallbacks`（内容审查拒绝）。
- fallback 按序执行；支持 `default_fallbacks`（指定组全部坏掉时兜底）；fallback 时可为每个目标模型传**不同的 messages/参数**（"Control Fallback Prompts"）；可 fallback 到具体 model ID 并**跳过其冷却检查**（救急路径）。

### 1.5 缓存

打开页面：https://docs.litellm.ai/docs/proxy/caching

- 后端：Redis/Valkey/ElastiCache/Memorystore、语义缓存（Qdrant / Redis-semantic / Valkey-semantic）、S3/GCS、磁盘、内存。**明确指出内存缓存在多 worker 下命中率按 worker 数下降**——这是"多实例必须共享缓存"的论述。
- 精确匹配缓存按整个请求体 hash 做 key；`/cache/ping` 健康检查端点；响应带 `x-litellm-cache-key` header 可直接调 `/cache/delete`。
- 文档对语义缓存的警告很诚实："适合单轮 prompt，在 agentic 流量上会严重出错"。

### 1.6 虚拟密钥与配额（NexusFlow 完全没有的能力）

打开页面：https://docs.litellm.ai/docs/proxy/virtual_keys

- **master key（管理员）→ /key/generate 签发虚拟密钥**，密钥可绑定：模型白名单（含 alias 重命名）、`max_budget`（美元硬预算）、`budget_duration`/`budget_reset_at`（**周期预算**，如 10d 自动重置）、`tpm_limit`/`rpm_limit`、过期时间、`allowed_routes`。
- **三层归属：key → team → user**，spend 按 key/user/team/end_user 四级追踪（`/key/info`、`/user/info`、`/team/info`、`/end_user/info`）。
- key 从 owner 继承模型权限/预算/限流，但可逐项覆盖；`upperbound_key_generate_params` 让管理员给所有密钥生成请求设**上限**（如 max_budget ≤ $100，超限 400 拒绝且不静默 clamp）；`custom_key_policy` 钩子覆盖 generate/update/regenerate 全部密钥操作。
- 角色体系：proxy_admin / org_admin / internal_user / customer（外部客户）等。

### 1.7 成本控制与可观测

打开页面：https://docs.litellm.ai/docs/proxy/cost_tracking

- 内置全模型价目表（`model_prices_and_context_window.json`），`completion_cost()` 每请求自动算成本；**响应头带 `x-litellm-response-cost`** 让调用方直接看到本次花费。
- SpendLogs 记录：api_key / user / team_id / end_user / request_tags / model_group / api_base / spend / tokens / `attempted_fallbacks` / `original_model_group`（fallback 溯源）。
- **请求 tags 成本归因**：body `metadata.tags` 或 header 打标，按 job/任务聚合成本；默认把 **User-Agent 也记为 tag**（可看 Claude Code / Gemini CLI 各工具的用量）。
- 供应商侧特殊计价自动处理：Vertex PayGo/优先级定价、Bedrock service tiers、Azure base model 映射（Azure 返回 gpt-4 而实际是 gpt-4-1106-preview 时用 `base_model` 校正计价）。
- **零成本请求可观测**：Prometheus 计数器 `litellm_zero_cost_requests_total`（按原因分：missing_pricing_key / pricing_not_applied / cost_calculation_error）——专治"价目表缺了新模型导致成本静默漏记"。

---

## 二、New API (QuantumNous/new-api)

### 2.1 定位与技术栈

- **定位**："面向应用、Agent 与团队的自托管 AI 网关……统一聚合与分发，跨格式转换为 OpenAI/Claude/Gemini 兼容格式"（https://github.com/QuantumNous/new-api，README）。One API 的活跃超集（AGPLv3，基于 MIT 的 One API 二开）。
- **技术栈**：Go + Gin 后端；React 19 + TS + Rsbuild 前端（嵌入 `web/dist`）；SQLite/MySQL/PostgreSQL 主库；**日志可独立库（`LOG_SQL_DSN`，支持 ClickHouse）**；Redis 用于跨节点共享限流与会话；docker-compose 默认带 PostgreSQL + Redis，另附 ClickHouse 日志库示例。
- **RelayKit**：独立可构建的 Go module（`relaykit/`），专职四种文本协议（OpenAI Chat/Responses、Anthropic Messages、Gemini）的 DTO 与双向流式转换——协议转换层被拆成独立库。NexusFlow 的 `src/protocol/` 与之同构，但 NexusFlow 只覆盖 OpenAI↔Anthropic 两种。
- 支持端点：OpenAI Chat/Responses、Anthropic `/v1/messages`、Gemini 原生 `generateContent`、**Realtime/Responses 的 WebSocket 升级**、images/audio、embeddings/rerank、**JS 插件任务 API**（`/v1/tasks/{pluginKey}`，图像/视频等异步任务用 JS 插件扩展，含任务状态与产出检索）。

### 2.2 渠道路由：ability 表 + 优先级分层 + 层内加权随机

打开文件：https://raw.githubusercontent.com/QuantumNous/new-api/main/model/ability.go

- **`Ability` 表**（ability.go:18-26）：`(Group, Model, ChannelId)` 联合主键 + `Enabled` + `Priority` + `Weight` + `Tag`——渠道×模型×分组展开成**路由倒排表**（渠道创建时展开写入，`AddAbilities` ability.go:216；渠道改动全量重建 `UpdateAbilities` ability.go:263；`FixAbility` ability.go:357 一键修复一致性）。选路时按 `(group, model, enabled)` 索引直达候选集，无需扫描渠道。
- **选路算法**（`GetChannel` ability.go:108-164）：候选按 `priority DESC` 去重得到优先级层级列表；**`retry` 参数直接索引到第 N 层优先级**（ability.go:134-141）——第 0 次尝试用最高优先级层，每次重试降到下一层，层数用尽后钉在最低层；同层内按 `weight + 10` 加权随机（ability.go:146-158，**所有渠道有 +10 基础权重，weight=0 的渠道也能分到流量**，不会出现权重饿死）。
- 渠道级约束过滤：`filterAbilitiesByConstraints`（ability.go:169）支持按 ChannelFilter（如任务插件身份）过滤候选；**查 channel 失败时 fail-closed/fail-open 区分**（任务插件身份要求则拒绝，否则放行原候选）。

### 2.3 重试决策：集中式、可配置、带审计

打开文件：https://raw.githubusercontent.com/QuantumNous/new-api/main/service/relay_error.go

- **`DecideRelayRetry`**（relay_error.go:21-58）是**唯一的重试决策入口**，返回结构化 `PolicyDecision{Action, Reason, Source}`，决策理由可分类：`channel_error` / `non_retryable_error` / `system_retry_exclusion` / `unrecognized_status` / `retry_status_matched` / `status_not_retryable`——**哪些状态码可重试是运营可配置的**（`operation_setting.ShouldRetryByStatusCode`），还有"永远不重试"黑名单。对比 NexusFlow：重试分类硬编码在 utils/retry.rs:40-52（429/500/502/503），改策略要改代码。
- 重试主循环（https://raw.githubusercontent.com/QuantumNous/new-api/main/controller/relay.go:158-219）：`for ; retryParam.GetRetry() <= RetryTimes; ...` 每轮**重新选渠道**（`getChannel` → `CacheGetRandomSatisfiedChannel(retryParam)`，retry 索引驱动优先级降层），每次决策打日志"重试：渠道A->渠道B"。

### 2.4 渠道亲和（Rule-based Channel Affinity）

打开文件：https://raw.githubusercontent.com/QuantumNous/new-api/main/service/channel_affinity.go

- 比 LiteLLM 更可配置的粘性路由：**规则引擎**按请求特征生成 cache key（`KeySourceType/KeySourcePath/ParamTemplate`，用 gjson 从 body 按路径抽取 + 正则提取，channel_affinity.go:44-60），命中则固定渠道，`TTLSeconds` 控制，存 `cachex.HybridCache[int]`（内存+Redis 混合）；可配 `SkipRetry`（亲和渠道失败时不重试其它渠道——用于强会话绑定场景）；还带亲和缓存**命中率统计**（`ChannelAffinityUsageCacheCounters`）供控制台观测。

### 2.5 计费与配额

来源 README capabilities 表（https://github.com/QuantumNous/new-api）：

- **额度（quota）体系**：用户/分组/令牌三级额度；`model/quota_reserve.go`（请求前预扣）/ 结算退还模式；`service/tiered_settle.go` 分级结算。
- **表达式定价**：按用量档位配置价格表达式；**cache accounting**：上游 prompt-cache 命中 token 单独计价（Claude/OpenAI 的 cache read/write 折扣能精确入账）。
- 订阅（subscriptions）、兑换码、充值（epay）、违规罚金（`violation_fee.go`）——完整"API 分销"商业闭环，NexusFlow 定位是内部网关可只取配额部分。

### 2.6 渠道健康

- 渠道测活：手动"渠道测试" + 定时（README/控制台）；错误日志全量入库并关联渠道（relay_error.go `ProcessChannelError` → `RecordErrorLog`）；错误敏感信息脱敏后入库（`MaskSensitiveErrorWithStatusCode`）。
- 自动禁用：继承 One API 的"错误触发禁用 + 成功率统计"双轨（见下节）。

---

## 三、One API (songquanpeng/one-api)

### 3.1 定位与技术栈

- **定位**："通过标准 OpenAI API 格式访问所有大模型，开箱即用；单可执行文件"。功能清单（README，https://github.com/songquanpeng/one-api）：多渠道负载均衡、流式、多机部署、令牌管理（过期/额度/IP 白名单/模型白名单）、兑换码、渠道批量管理、用户/渠道分组倍率、模型映射、失败自动重试。
- **额度公式**（README FAQ）：`额度 = 分组倍率 × 模型倍率 × (提示 token + 补全 token × 补全倍率)`——倍率体系简单清晰，可作为 NexusFlow 计费的起点模型。
- **令牌直连渠道调试**：`Authorization: Bearer KEY-CHANNEL_ID` 可强制本次请求走指定渠道（README"使用方法"节）——排障神器，实现成本极低。

### 3.2 成功率滑动窗口自动禁用渠道

打开文件：https://raw.githubusercontent.com/songquanpeng/one-api/main/monitor/metric.go

- 每渠道维护一个 success/fail 布尔**滑动窗口**（默认队列 10，`config.MetricQueueSize`），fail 事件到达时计算窗口成功率，**窗口满且成功率 < 0.8（`MetricSuccessRateThreshold`）→ 自动禁用渠道并清空窗口**（metric.go:18-38）。
- 实现完全异步非阻塞：`Emit()` 用带缓冲 channel + 消费 goroutine（metric.go:61-79），统计不拖慢请求路径。阈值与开关都是环境变量（README 环境变量节 `ENABLE_METRIC` 等）。
- 与错误码联动：致命错误（如 401）直接 `ShouldDisableChannel` 禁用并通知，普通错误只进成功率统计（https://raw.githubusercontent.com/songquanpeng/one-api/main/controller/relay.go，`processChannelRelayError`）。

### 3.3 重试判定

打开文件：https://raw.githubusercontent.com/songquanpeng/one-api/main/controller/relay.go:105-121

- `shouldRetry`：429 → 重试；5xx → 重试；400 → 不重试；2xx → 不重试；**其余 → 默认重试**；指定渠道（KEY-CHANNEL_ID）→ 禁止重试。分类比 NexusFlow 粗（无指数回退），但"默认重试未知错误"的取向值得注意。

### 3.4 多机部署与批量聚合

- README"多机部署"节：master/slave + 共享 MySQL + `SYNC_FREQUENCY` 定期同步配置 + 从节点各自 Redis 做本地缓存（"缓存未过期时数据库零访问"）。
- **`BATCH_UPDATE_ENABLED`**：用户额度更新批量聚合写入（默认间隔 5s），专门解决高并发下数据库连接数耗尽——与 NexusFlow 已有的"异步日志批量写入 + Token 预聚合"同一思想，但 One API 把**额度扣减**也纳入了批量管道。

---

## 四、与 NexusFlow 的架构差异

NexusFlow 现状核对（本地代码，2026-09-30 实读）：

| 维度 | NexusFlow | LiteLLM / New API / One API |
|------|-----------|------------------------------|
| 路由候选组织 | `config.toml` 静态 vendor→model 树；三级路由 direct→abstract→global（router/router.rs:94-220） | New API ability 倒排表（group×model×channel）；LiteLLM model_list 平铺 + 组策略 |
| 选路算法 | 3D 权重加权随机（weight×0.4+queue×0.3+window×0.3，router/router.rs:279-290） | 同类加权随机（New API weight+10）之外还有：最低延迟、最低成本、最低 TPM 用量、最闲四种可插拔策略 |
| 重试 | 状态码硬编码分类：429 指数回退(≤5)/500,502,503 固定 1s(≤3)/短重试(≤2)（utils/retry.rs:30-109） | 优先级分层升级（New API retry→层；LiteLLM order）；同层排除已失败再加权重挑（LiteLLM weighted failover）；可重试状态码配置化（New API `ShouldRetryByStatusCode`）；分类 fallback（LiteLLM 三类）；重试次数调用方可覆盖（LiteLLM header） |
| 弹性状态 | 供应商级熔断 + mark_throttled 退避（vendor/status.rs:14-75, manager.rs:173-194） | 部署/渠道级冷却（分钟失败率+错误类型阈值），自动恢复，按部署粒度隔离故障（LiteLLM cooldowns；One API 成功率窗口禁用） |
| 会话亲和 | 无 | LiteLLM session_affinity（prompt cache 命中）；New API 规则化亲和（key 路径+正则+TTL+命中率统计） |
| 客户端密钥 | 静态 key 列表校验（middleware/auth.rs:15-43） | 虚拟密钥：模型白名单/周期预算/TPM·RPM 限制/过期/角色/密钥策略钩子（LiteLLM virtual keys；New API 令牌+分组+订阅） |
| 成本 | CostTracker 按 vendor/model 累计（utils/cost_tracker.rs:28-118），窗口余额参与路由 | 每请求实价计算+响应头返回成本；价目表+cache token 单独计价；预算硬限制+周期重置；零成本请求告警计数器 |
| 响应缓存 | 无（oxcache 仅用于 DI 内部） | Redis/语义/磁盘多层响应缓存（LiteLLM caching） |
| 可观测 | GlobalMetrics + SQLite 批量日志 + /admin metrics | Prometheus 全套 + 请求级 spend log + fallback 溯源字段 + Slack 异常/慢响应告警 + User-Agent 自动归因 |
| 协议转换 | OpenAI↔Anthropic 双向 + 零拷贝透传 | 四协议（+Gemini/Responses）+ WebSocket + 独立转换库（New API RelayKit） |

**NexusFlow 已有且同类也不弱于的地方**：零拷贝透传（LiteLLM/One API 都要重构请求体，One API README 明确说模型映射"会导致请求体被重新构造而非直接透传"）；GCRA 令牌桶 + 熔断器；SQLite 批量日志 + Token 预聚合（≈One API BATCH_UPDATE）；3D 权重把队列与窗口余额纳入权重（LiteLLM 默认 simple-shuffle 反而不看实时负载）。

---

## 五、值得吸收的设计（按优先级）

### P0（高价值 × 实现代价小）

1. **优先级分层重试**：模型/渠道配 `priority`，第 N 次重试降到第 N 优先级层（New API ability.go:133-141 只用了十几行）。NexusFlow 路由已有候选排序概念，补一个"retry 次数→层索引"即可，立刻获得"便宜优先、失败下沉"的调度语义。
2. **重试时排除已失败供应商并重归一化权重**（LiteLLM weighted failover，docs.litellm.ai/docs/routing）：NexusFlow 当前重试若重新 `weighted_select` 可能再次选中刚失败的 vendor；排除集 + 权重重归一化是通用正确做法。
3. **重试状态码集合配置化**（New API `DecideRelayRetry` + `ShouldRetryByStatusCode`，service/relay_error.go）：把 utils/retry.rs 的硬编码 429/5xx 分类搬进 config.toml 热更新面，与现有 /admin/config 对齐；顺带引入结构化 RetryDecision（action/reason）便于日志。
4. **渠道/供应商级冷却而非仅熔断**（LiteLLM cooldowns）：429 立即冷却 5s、分钟失败率>50% 冷却、`allowed_fails` 阈值化，冷却到期自动回池——比"熔断开→半开探测"更细粒度，且两者可叠加（NexusFlow 已有 circuit state，缺的是短冷却自动恢复语义）。
5. **成功率滑动窗口自动禁用**（One API monitor/metric.go:18-38）：每渠道 10 个样本的环形缓冲 + 0.8 阈值，异步统计不进请求热路径。NexusFlow 有全局限时器但无按渠道成功率的自动摘除。
6. **令牌直连渠道**（One API `Bearer KEY-CHANNEL_ID`）：网关调试刚需，一行解析即可实现。

### P1（需要新子系统，价值高）

7. **虚拟密钥 + 周期预算**（LiteLLM virtual keys，docs.litellm.ai/docs/proxy/virtual_keys）：把 middleware/auth.rs 的静态列表升级为数据库签发的 key：模型白名单、`max_budget`+`budget_duration` 周期重置、`tpm_limit`/`rpm_limit`（limiteron 现成可按 key 维度再挂一层）。这是 NexusFlow 从"自用代理"走向"团队网关"的分水岭。
8. **每请求成本计算 + 响应头返回**（LiteLLM cost_tracking `x-litellm-response-cost`）：NexusFlow 有 CostTracker 与 vendor/cost.rs，补一张模型价目表（含 cache token 计价）即可让每次响应自带成本，客户端与日志都能对账。
9. **成本参与路由**（LiteLLM cost-based-routing）：作为 3D 权重之外的第四维或独立策略（`weight×0.3 + queue×0.25 + window×0.25 + cost×0.2` 之类的可配置组合）。
10. **上下文窗口 pre-call check**（LiteLLM routing 文档）：请求 token 估算（NexusFlow 已有 token 统计基础）后过滤装不下的模型，避免注定失败的 400；同类还应该吸收 **context_window_fallbacks**（超长自动换大窗口模型）。
11. **会话亲和（sticky session）**（LiteLLM session_affinity）：读 `x-session-id` 类 header，TTL 内固定 vendor——直接动机是吃上游 prompt cache 折扣；New API 的规则化配置（key 路径 + TTL + 命中率统计）是更完整的形态。
12. **多路由策略可插拔**（LiteLLM 五策略 + 自定义插件）：latency-based（带 buffer 防打爆最快者）与 least-busy 是 NexusFlow queue_factor 的自然延伸；把策略做成 trait 即可。

### P2（远期/按需）

13. **响应缓存**（LiteLLM caching）：仅建议对 temperature=0/确定性请求与 embedding 开启；LiteLLM 文档自己警告语义缓存对 agentic 流量有害。
14. **流量镜像 A/B**（LiteLLM traffic mirroring）：供应商灰度评估用，网关成熟期特性。
15. **独立日志库**（New API `LOG_SQL_DSN` + ClickHouse；One API 同）：日志量大后把 logs 表迁出主库，SQLite 阶段可先做"日志库文件分离"。
16. **`litellm_zero_cost_requests_total` 式审计**：价目表缺新模型时成本静默漏记——加一个"未知模型价格"计数器/告警，防账单黑洞。

### 不建议照搬

- **多机部署/主从同步**（One API）：NexusFlow 单机 SQLite 定位下复杂度不划算；要横向扩展时先解决配置分发与限流共享（New API 用 Redis 的方案可回头参考）。
- **完整商业闭环**（兑换码/充值/订阅/违规罚金）：超出内部网关定位。
- **请求体重构式模型映射**（One API 模型映射）：与 NexusFlow 零拷贝透传优势冲突；若做模型名映射，应在选路层改写目标而非重写 body。
