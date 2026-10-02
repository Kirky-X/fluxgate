# 同类项目调研：Bifrost / Kong AI Gateway / OpenRouter

> 调研日期：2026-09-30。调研方式：WebFetch 实际打开官方 README 与文档页面（每条结论均标注来源 URL）。
> 调研目的：为 NexusFlow（Rust + Axum 智能路由网关，~7.6k 行）找出值得吸收的设计与特性。

---

## 0. 结论速览（按价值排序的 Top 吸收点）

| # | 吸收点 | 来源 | 对 NexusFlow 的价值 |
|---|--------|------|---------------------|
| 1 | 按状态码细分重试语义（401/403/402 立即轮换 key、无退避；语义化错误 `502 upstream_credentials_exhausted`） | Bifrost | NexusFlow 只有 429/500 两类，可细化为 5 类 |
| 2 | 每请求可传的 fallback 链 + 按权重降序自动生成 fallback 链 | Bifrost / OpenRouter | 给 API 增加 `fallbacks` 参数，客户端零代码获得高可用 |
| 3 | 价格感知路由：1/price² 加权随机 + `max_price` 硬上限 | OpenRouter | 3D 权重加入第 4 维（价格），或直接替换 window_factor |
| 4 | 虚拟 Key + 分层预算（USD 上限 + 日历对齐重置 + token 级限流） | Bifrost | 多租户治理完全缺失，是最大的能力空洞 |
| 5 | 语义缓存（哈希精确 + 嵌入相似度两级） | Bifrost / Kong | 成本优化利器，oxcache 兄弟库可作为精确缓存起点 |
| 6 | 会话亲和（session stickiness）保住上游 prompt cache | Bifrost | 零成本利用供应商侧缓存，Rust 实现极简 |
| 7 | EWMA 延迟追踪（tpot/e2e 两种口径）做 lowest-latency 路由 | Kong | queue_factor 的升级替代：直接以观测延迟驱动权重 |
| 8 | `failover_criteria` 可配置回退触发条件（默认客户端错误不回退） | Kong | 替代硬编码状态码分类 |
| 9 | 复杂度路由（SIMPLE/MEDIUM/COMPLEX 三档 + CEL 表达式） | Bifrost | 内容感知调度，省钱的核心手段 |
| 10 | 路由决策审计 trail + OpenTelemetry 导出 | Bifrost / Kong | GlobalMetrics 只有聚合数，缺决策级追踪 |
| 11 | 流式响应的启动事件缓冲重试 | Bifrost | 解决「200 已发出才发现上游失败」场景 |
| 12 | 在途消费预算预冻结（in-flight budget） | OpenRouter | 防并发请求击穿成本上限 |

---

## 1. Bifrost（maximhq/bifrost）

### 1.1 定位与技术栈

- **定位**：高性能企业级 AI 网关，「50x faster than LiteLLM」，支持 1000+ 模型、23+ 供应商，5k RPS 下每请求仅增加 <100µs（README 自述压测 11µs）。
- **技术栈**：Go 编写（核心 `core/bifrost.go`，Go SDK 可嵌入），HTTP 传输层 `transports/bifrost-http`（FastHTTP 服务器），Web UI 管理台。Apache 2.0。
- **架构**：`core/`（引擎+供应商实现）、`framework/`（configstore/logstore/vectorstore 可插拔存储）、`transports/`（HTTP 网关）、`plugins/`（governance、semanticcache、telemetry、logging 等插件）、`ui/`。
- 来源：https://github.com/maximhq/bifrost （README，经 raw.githubusercontent.com 抓取）；https://api.github.com/repos/maximhq/bifrost （仓库元信息，description 字段）

### 1.2 请求处理管道（9 阶段）

来源：https://docs.getbifrost.ai/architecture/core/request-flow.md

1. **传输层**：解析 headers → JSON 反序列化 + schema 校验 → 转内部 `BifrostRequest`。解析约 2.1µs、校验约 400ns，尽量零拷贝。Go SDK 路径直接函数调用，无序列化开销。
2. **路由与负载均衡**：模型可用则直接用指定 provider，否则走映射；**API Key 池加权随机选择（约 10ns 常数时间）**；健康检查走缓存（约 50ns）；provider 不健康 → 熔断器 → 有 fallback 则切换重新进入 key pool。
3. **Pre-Hook 插件管道**（顺序固定，任一失败中断链）：认证（1-5µs）→ 限流（500ns）→ 请求转换（2-10µs，失败用原始请求继续）→ 监控（200ns，静默）。
4. **MCP 工具发现与注入**（include/exclude 过滤）。
5. **内存池**：`sync.Pool` 管理 channel/message/response/buffer 对象，用完 Reset 归还。
6. **Worker 池**：入队 → 取 worker → 熔断器检查 → 转发 → 更新健康指标。
7. **Provider 通信**：加认证头 → 熔断健康检查 → 带 timeout 执行 → 记录指标。
8. **工具并发执行**。
9. **Post-Hooks**（失败只记 warning 继续）：日志 → 缓存写入 → 指标。

**与 NexusFlow 差异**：Bifrost 把「路由→pre-hooks→执行→post-hooks」做成显式分阶段管道，每个阶段有明确的错误传播语义（pre-hook 短路、post-hook 不短路）。NexusFlow 的中间件链（middleware/）+ router 是隐式组合，可以借鉴其「pre-fail-fast / post-best-effort」的两段式语义。

### 1.3 重试与回退（重点，与 NexusFlow 直接对标）

来源：https://docs.getbifrost.ai/features/retries-and-fallbacks.md

**重试按状态码分 4 类**（NexusFlow 现状：仅 429 指数回退 / 500 固定间隔，见 AGENTS.md）：

| 类别 | Bifrost 行为 | NexusFlow 可借鉴 |
|---|---|---|
| 网络错误 / 5xx | 重试，复用同一 key，指数退避 | 对齐现有 500 分类 |
| 429 | 重试并**轮换 key**，指数退避 | NexusFlow 未提及 429 时换 key/供应商 |
| **401/403/402** | 重试并轮换 key，**无退避**——「坏凭据等待无意义」，key 在本请求内标记 dead | **NexusFlow 缺失：认证/配额类错误应立即换路，且零退避** |
| 400/404/422、插件拦截、请求取消 | 不重试 | 对齐 |

- 退避公式：`min(initial × 2^attempt, max) × jitter(0.8–1.2)`，默认 initial=500ms、max=5000ms、max_retries=0。**jitter 抖动**防止惊群——NexusFlow 的 429 指数回退建议加上。
- 所有 key 失效时返回语义化错误 `502 upstream_credentials_exhausted` 而非裸 4xx。NexusFlow 应定义类似的「上游凭据/产能耗尽」语义化错误码。
- dead-key 是 per-request 的（不跨请求持久化），持久化熔断由熔断器负责。

**Fallback 语义**：
- 请求体传 `fallbacks: ["provider/model", ...]` 数组，按序尝试；**每个 fallback provider 获得独立完整的重试预算**（主+2 备 × max_retries=3 = 最多 12 次尝试）。
- 全部失败返回**主 provider 的原始错误**（保留最相关的错误信息）。
- fallback 请求视为全新请求，语义缓存、治理、日志等所有插件重新执行。
- 插件可设置 `AllowFallbacks = false` 立即中断回退链（如请求级幂等约束）。
- 虚拟 Key 场景下：多 provider 配权重时，**自动按 weight 降序生成 fallback 链**（仅在请求未手动指定时）。来源：https://docs.getbifrost.ai/features/governance/virtual-keys.md

**流式请求重试（特色）**：Azure 流可在 HTTP 200 内先发启动元数据再报错。Bifrost 缓冲可识别的启动事件（空 choice 注解、空 delta、assistant-role chunk 等），错误出现时仍可进入重试/回退；成功后按序回放缓冲事件。缓冲终止条件：收到正文内容、或 64 chunks / 256 KiB 上限。

**与 NexusFlow 差异**：NexusFlow 的重试在 vendor/ 层按状态码分类，但 (a) 没有 401/403/402 的「立即换 key 不退避」；(b) 没有抖动；(c) 没有请求级 fallback 链 API；(d) 没有流式启动缓冲。这四点都是明确可落地的改进。

### 1.4 治理（Governance）：虚拟 Key / 预算 / 限流 —— NexusFlow 完全缺失的能力

来源：https://docs.getbifrost.ai/deployment-guides/config-json/governance.md 、https://docs.getbifrost.ai/features/governance/virtual-keys.md

- **Virtual Key**：发给客户端的 scoped token，可限定 provider 白名单（`allowed_models`，支持 `regex:` RE2 前缀）、key 池（`key_ids`）、权重（`weight`）、独立限流（`rate_limit_id`），可挂到 team 或 customer。
- **分层预算**：`budgets[]` 挂在 virtual_key / team / customer / provider_config / model_config 之一（最多一个归属），`max_limit` 以 **USD 计**，`reset_duration` 支持 30s→1Y 粒度，`calendar_aligned` 把重置对齐到日历边界（而非滚动窗口）。
- **Token 级限流**：`request_max_limit`（请求数）与 `token_max_limit`（token 数）**两个独立维度、各自时间窗口**。NexusFlow 的 limiteron GCRA 是请求级，无 token 维度。
- **CEL 表达式动态路由规则**：`cel_expression`（如 `request.model == 'gpt-4o'`、`headers["x-tier"] == "premium" && complexity_tier == "COMPLEX"`）→ 加权 `targets`（权重和必须为 1.0）→ `fallbacks` 链；支持 priority（数字越小越先评估）、scope（global/team/customer/virtual_key）、`chain_rule`（匹配后重新评估链）。来源：https://docs.getbifrost.ai/features/governance/routing.md 页面顶注（详见 Routing Rules 页）。

**对 NexusFlow 的适配**：多租户与成本控制是 NexusFlow 的最大能力空洞。落地路径：数据库里已有 SQLite，可加 `virtual_keys` / `budgets` / `rate_limits` 三张表；中间件认证处校验 VK → 查预算 → 记账（响应后按 usage 扣减）。USD 计价需要每模型单价表（Bifrost 有 custom-pricing：https://docs.getbifrost.ai/providers/custom-pricing.md ，索引页列出）。

### 1.5 语义缓存

来源：https://docs.getbifrost.ai/features/semantic-caching.md

- **两级匹配**：先规范化哈希精确匹配（无需嵌入模型），未命中再走嵌入相似度（余弦 ≥ threshold，默认 0.8）。
- **关键配置**：`ttl`（默认 5 分钟）、`threshold`、`conversation_history_threshold`（对话消息数 >3 跳过缓存，防多轮对话污染）、`cache_by_model`/`cache_by_provider`（缓存键含模型/供应商）、`exclude_system_prompt`。
- **必须携带缓存键**（header `x-bf-cache-key` 或 `default_cache_key`），否则完全绕过——把「是否允许缓存」的决定权交给客户端。
- 写入异步，首次请求不阻塞；支持流式响应缓存与逐块重放。
- **诚实的代价分析**（文档原话）：语义未命中总耗时反而比不用缓存更慢（多付一次嵌入调用，几十到几百毫秒）。
- 后端：Redis/Valkey（直接模式唯一推荐）、Weaviate、Qdrant、Pinecone。

**对 NexusFlow 的适配**：NexusFlow 完全没有响应缓存。最小可行起步：oxcache 做精确哈希缓存（规范化后 SHA256，TTL 短），`cache_by_model/provider` 与 `conversation_history_threshold` 两个设计点直接照抄；语义缓存留作可选插件（Rust 侧可接 Qdrant，或用 candle/fastembed 本地嵌入）。

### 1.6 Key 管理

来源：https://docs.getbifrost.ai/features/keys-management.md

- Key 池：每 key 可配 `weight`、`models` 白名单（`[]` 拒绝全部、`["*"]` 全放行——**deny-by-default**）、`blacklisted_models`（黑名单优先于白名单）。
- 加权随机：总权重 → 随机数 → 累计权重区间选中；选中失败自动回退下一 key。权重是**静态配置**，无运行时自动调权（动态调权是企业版 Adaptive LB）。
- 显式指定 key 的 header（`x-bf-api-key-id`）绕过加权但仍应用模型过滤。
- Azure/Bedrock 部署映射缺失的 key 自动排除出选择池。

### 1.7 复杂度路由（Complexity Router）

来源：https://docs.getbifrost.ai/features/governance/complexity-router.md

- **语义三档分类**：取最近 N 条用户消息 → 嵌入 → 与 150 条内置「参考短语」（SIMPLE/MEDIUM/COMPLEX 各 50 条）最近邻匹配 → 发布 `complexity_tier` 作为 CEL 变量。**不是** prompt 长度/关键词启发式。
- 关键工程设计：
  - **懒执行**：只有路由规则实际引用 `complexity_tier` 才运行分类，否则零开销。
  - `min_similarity` 下限：低于下限不发布层级，「宁可跳过，不猜测」。
  - **LLM 兜底分类器**（可选）：语义分类无结果时才调小模型兜底，串行不并行。
  - **会话感知路由**：会话内复杂度只升不降（首个分级确立会话层级），避免破坏供应商 prompt cache。
  - 分类器状态机（warming/ready/failed）与配置热更新后台预热。
- 典型用法：`complexity_tier == "COMPLEX"` → frontier 模型，其余走原路径——一条规则即可显著降本。

**对 NexusFlow 的适配**：NexusFlow 路由只看模型/队列/窗口三维，完全不看内容。第一版可用**廉价启发式**（入站 token 数、是否带 tools、上下文长度）做三档分级映射到不同权重表，不必上嵌入；接口设计成 `TierRouter` trait 即可保留升级到语义分类的空间。

### 1.8 会话亲和（Session Affinity）

来源：https://docs.getbifrost.ai/providers/session-affinity.md

- 同一会话的请求持续落回上次服务的 **provider + key**（两层）。session ID 来源优先级：显式 header `x-bf-session-id` > 编码工具 header（自动识别 Claude Code/Codex CLI）> SDK 上下文。
- 绑定存 KV 存储，**仅「成功服务」才写入**；回退服务成功则绑定迁移；跟随绑定后失败则删除；TTL 默认 1h 活跃刷新。
- 动机：供应商 prompt cache 按供应商乃至按 key/组织隔离，会话跳来跳去就失去「焐热」的缓存；同时限流桶可预测（固定一个 key 一个桶）。
- 亲和**不决定路由**，只在路由引擎给出的合格集合内调整优先顺序。

**对 NexusFlow 的适配**：实现成本极低（一个 `DashMap<SessionId, (Vendor, Key)>` + TTL 清理），收益直接（多轮对话的供应商侧缓存命中 + 限流桶稳定）。强烈建议吸收。

---

## 2. Kong AI Gateway

### 2.1 定位与架构

来源：https://developer.konghq.com/ai-gateway/

- **定位**：AI 原生应用的「连接与治理层」，建在 Kong Gateway 之上（控制平面 + 数据平面），把 LLM、MCP、A2A 三类流量统一到单一端点与控制面治理。
- **架构演进**：v2.x 实体模型（AI Model、AI Model Provider、AI Auth Strategy、AI Consumer、AI Consumer Group 等）取代 v1 的插件中心模式。
- 与传统网关关系的官方表述：仅把 LLM API 挂在 Kong 后面只是 API 层治理；AI Policies 能「understand the prompts」——检查请求体并提供 AI 专属能力。

### 2.2 负载均衡算法（7 种，NexusFlow 仅有加权随机一种）

来源：https://developer.konghq.com/ai-gateway/load-balancing/ 、https://developer.konghq.com/plugins/ai-proxy-advanced/

| 算法 | 机制 | NexusFlow 对标 |
|---|---|---|
| Round-robin | 加权比例分发，不考虑缓存命中/延迟/负载 | ≈ NexusFlow 的 model.weight 维度 |
| Consistent-hashing | `hash_on_header`（默认 `X-Kong-LLM-Request-ID`）粘性会话 | 无（对应 Bifrost session affinity） |
| Least-connections | 追踪在途请求数，发给容量余量最大的后端 | ≈ queue_factor 维度（半对标） |
| **Lowest-latency** | **peak EWMA** 追踪「TCP 连接→响应体完成」的延迟，`latency_strategy` 默认 `tpot`（每输出 token 耗时），可选 `e2e`；最快者拿 90-99% 流量，其余持续拿 0.1-5% 探测流量 | **无。NexusFlow 的 window_factor 是静态窗口，不是观测延迟** |
| Lowest-usage | `tokens_count_strategy`：prompt/completion/total tokens、**cost**、准确度分数 | 无成本维度 |
| Semantic | prompt 嵌入与模型描述向量相似度路由（`distance_metric`、`threshold`，需向量库） | 无 |
| Priority | 分层回退：最高优先级组承载全部流量，组内按 weight 分配；全组不可用才降级下一组 | ≈ NexusFlow 的 fallback 模式（但组化更清晰） |

**EWMA 探测流量设计**值得单独注意：不给最快者 100% 流量，保留 0.1-5% 持续探测其他目标，EWMA 随每次响应更新、无固定学习期——延迟数据永远新鲜，故障后可立即切换。这比 NexusFlow 的「时间窗口因子」更有自适应性。

### 2.3 回退与熔断

来源：https://developer.konghq.com/plugins/ai-proxy-advanced/ 、https://developer.konghq.com/ai-gateway/load-balancing/

- **`failover_criteria` 可配置**：默认 `error` + `timeout`，可加 `http_429`、`http_502`、`non_idempotent` 等。**官方明确「Client errors don't trigger failover」**——与 Bifrost 的 401/403 立即换 key 形成有意思的对比（Kong 换的是 target 不是凭据）。
- **熔断器（v3.13+）**：`max_fails`（**累计**失败计数，非连续）达到阈值 → `fail_timeout` 内停止向该 target 路由 → 超时后重新探测，成功即恢复。所有 target 不健康返回 HTTP 500。这是一个干净、易实现的熔断范式。
- v3.10+ 支持跨格式混合回退（OpenAI 与 Mistral 混用）；`llm_format` 可设原生格式（anthropic/bedrock/cohere/gemini）**透传原生请求/响应**——与 NexusFlow 零拷贝透传同构，互相印证该设计合理。
- `model_alias` 模板：客户端可用别名代替真实模型名，v3.7+ 支持 `$(headers.x)`、`$(uri_captures.x)`、`$(query_params.x)` 占位符。

### 2.4 内容级能力（NexusFlow 完全没有的方向）

来源：https://developer.konghq.com/ai-gateway/

- AI Prompt Guard / AI Semantic Prompt Guard（关键词 + 语义两层拦截越狱与注入）
- AI Sanitizer（上游前 PII 脱敏）
- AI Semantic Response Guard（响应侧不安全内容检查）
- **AI Prompt Compressor**（上游前压缩 prompt 省 token）
- AI Semantic Cache、Model cost management（含缓存/上下文窗口/服务层级定价的精确成本计算）
- 可代理 AI CLI 工具流量（Claude Code、Codex CLI）、REST API 映射为 MCP 工具

**判断**：guardrails/sanitizer 属于安全产品方向，超出 NexusFlow 当前「路由网关」定位，暂不建议吸收；但 **Prompt Compressor**（可选插件：超长上下文先压缩再转发）与成本计算表值得放进路线图。

---

## 3. OpenRouter

### 3.1 定位与技术栈

来源：https://openrouter.ai/docs

- **定位**：托管型 LLM 路由服务（非自建开源软件）——单一 API 聚合数百个模型的多家供应商端点，「handles fallbacks automatically and picks the most cost-effective option for each request」。闭源 SaaS，但路由/回退机制的**产品语义**设计极具参考价值。
- 客户端协议：OpenAI 兼容 + 原生 Anthropic Messages（`/api/v1/messages`）。

### 3.2 供应商选择（Provider Selection）—— 请求级路由偏好 API

来源：https://openrouter.ai/docs/guides/routing/provider-selection.md

请求体 `provider` 对象的完整字段（NexusFlow 完全没有请求级路由控制）：

| 字段 | 语义 |
|---|---|
| `order` | 显式 provider 尝试顺序 |
| `allow_fallbacks`（默认 true） | false 时只由首选 provider 服务，失败即失败 |
| `require_parameters` | 只路由到支持请求全部参数的 provider |
| `only` / `ignore` | 白名单 / 黑名单（黑名单与账户级合并，白名单与账户级取交集） |
| `quantizations` | 按量化级别过滤（int4/fp8/bf16/fp32…）——**权重会变，需要精度过滤** |
| `sort` | `price` / `throughput` / `latency`；设置后**禁用负载均衡**改为按序尝试 |
| `preferred_min_throughput` / `preferred_max_latency` | 软偏好：支持 p50/p75/p90/p99 百分位对象；**不达标只降级排序，永不阻止执行** |
| `max_price` | **硬上限**：`{"prompt": 1, "completion": 2}`，不满足则请求直接失败（与软偏好的关键区别） |
| `data_collection` / `zdr` | 隐私约束路由（不用记录用户数据的供应商 / 零数据保留端点） |

**默认路由逻辑（核心公式）**：
1. 过滤掉**过去 30 秒内出现明显故障**的 provider；
2. 对剩余稳定低价候选者，**按价格平方倒数加权随机**（$1/$2/$3 → A 概率是 C 的 9 倍）；
3. 失败后依次回退。

**对 NexusFlow 的适配**：1/price² 是一个极优雅的成本倾向公式——NexusFlow 3D 权重（weight×0.4 + queue×0.3 + window×0.3）里没有价格，可以直接把 model.weight 解释为单价、套 1/price² 归一化，或加第四维。30 秒故障过滤窗口与「软偏好降级排序、硬上限失败」的区分也应该照抄。`zdr`/`data_collection` 隐私维度是企业客户刚需。

### 3.3 模型回退（Model Fallbacks）

来源：https://openrouter.ai/docs/guides/routing/model-fallbacks.md

- 请求体 `models` 数组（首选放 `model` 字段），**任何错误**都触发回退：上下文长度超限、内容审核标记、速率限制、宕机。
- 按数组序依次尝试；主备都挂则返回**回退模型的错误**。
- **按实际使用的（成功的）模型计价**，响应 `model` 字段返回实际模型——客户端可感知回退发生。NexusFlow 的 `X-Vendor`/`X-Model` 响应头已对齐此设计（AGENTS.md「API 请求头扩展」）。
- Anthropic Messages 端点的 `fallbacks` 参数：每条只允许 `model` 字段（其他覆盖 400 拒绝）、不能与 `models` 并用、最多 3 条——**参数校验从严**的好例子。

### 3.4 可用性追踪

来源：https://openrouter.ai/docs/guides/best-practices/uptime-optimization.md

- 实时追踪每个供应商的**响应时间、错误率、可用性**三类指标；对外展示近 3 天逐小时可用性曲线，区分「经过路由恢复」与「不经过路由」两种口径（展示路由收益）；颜色阈值 ≥95% 绿 / ≥85% 黄 / 更低红。
- 可用性数据可经 **Endpoints API** 编程获取——把路由器的健康观测做成一等公民对外产品。
- 失败检测窗口细节文档未给出（如实标注）。

### 3.5 零补全保险（Zero Completion Insurance）

来源：https://openrouter.ai/docs/guides/features/zero-completion-insurance.md

- 全账户自动启用：补全 token 为零且 finish reason 为空/null，或 finish reason 为 error → **推理成本全免**（即使上游已向 OpenRouter 收费）。
- TTS 判定：交付至少一帧完整 MPEG frame / 一个完整 PCM sample；图像：至少一张 base64 非空可解码。
- 辅助服务（web 搜索、PDF OCR）费用不免。
- 本质：把「上游失败但响应了空内容」从计费事故变成路由器自担的成本——**网关侧可作为 usage 记账的负向修正项**：NexusFlow 的 Token 预聚合统计里，可对 0-token+error 的请求标记 `billed=false`。

### 3.6 限额与 429 语义

来源：https://openrouter.ai/docs/api_reference/limits.md

- Credit limits（402）与 Rate limits（429）两类，错误码区分原因。
- **在途消费预算（in-flight spending budget）**：请求完成才扣费 → 为防并发击穿余额，按「输入 + max_tokens 上限」预冻结每请求的估算成本；预算超限返回 402 + `metadata.reason: "in_flight_budget_exhausted"`（带 `Retry-After`，可重试）或 `"weight_exceeds_budget"`（单请求超全部预算，重试无用）。
- 429 三来源：平台限流（带 `X-RateLimit-*` 头）→ 上游供应商限流（`error.metadata.provider_code`）→ 流式中触发（**HTTP 200 已发出，错误以 SSE 事件到达**，`finish_reason: "error"` 含 code 429）。
- 供应商侧限流时先在同模型其他供应商间自动重试，全部耗尽才返回；错误带 `Retry-After`。
- 处理建议明确「按 `limit_source`/`remedy_hint` 字段分支，不要解析提示文本」。

**对 NexusFlow 的适配**：(a) 错误响应的结构化 metadata（`limit_source`、`provider_code`、`remedy_hint`、`Retry-After`）应照抄进 API 错误设计；(b) 流式中以 SSE 错误事件终止的语义需要确认 NexusFlow 流式路径是否实现；(c) in-flight 预算冻结是「预算」落地后的并发安全版本。

---

## 4. 架构对比总表

| 维度 | NexusFlow | Bifrost | Kong AI Gateway | OpenRouter |
|---|---|---|---|---|
| 语言/形态 | Rust + Axum，自建网关 | Go，网关+SDK+UI | 网关插件体系（控制面/数据面） | 托管 SaaS（闭源） |
| 协议 | OpenAI ↔ Anthropic 双向转换 | OpenAI 兼容统一入口，SDK 原生 | 多格式（llm_format 原生透传） | OpenAI 兼容 + Anthropic Messages |
| 透传 | 零拷贝透传（同协议直转） | 尽量零拷贝解析 | `llm_format` 原生透传 | — |
| 路由 | 3D 权重加权随机（weight/queue/window） | 加权随机 key 池（~10ns）+ CEL 规则 + 复杂度分级 | 7 种算法（RR/哈希/最少连接/EWMA/用量/语义/优先级） | 1/price² 加权随机 + 请求级偏好对象 |
| 重试 | 429 指数回退、500 固定间隔 | 4 类状态码语义 + jitter + key 轮换 + 独立回退预算 | retries + failover_criteria + 熔断 | 自动供应商回退（内部细节未公开） |
| 回退 | vendor 级 fallback 模式 | 请求级 fallbacks 链 + 权重降序自动链 | priority 分组 + 跨格式回退 | `models` 数组 + 计费按实际模型 |
| 缓存 | 无 | 精确哈希 + 语义缓存（4 种向量库） | AI Semantic Cache | 响应缓存（计费侧） |
| 限流 | limiteron GCRA（请求级）+ 熔断 | 请求级 + token 级，每 VK/provider/model | AI Rate Limiting Advanced（成本口径） | 账户级 RPM/RPD + credit + 在途预算 |
| 成本 | 无 | 预算（USD，日历对齐重置）、custom pricing | 成本管理、计量计费 | 1/price² 路由、零补全保险、按实际模型计费 |
| 多租户 | 无 | virtual keys / teams / customers / projects | AI Consumer Group | API key 级额度 |
| 可观测 | 内存 GlobalMetrics | Prometheus/OTel/9 种导出 + 路由决策 trail | OTel、审计日志、仪表盘 | 可用性公开页 + Endpoints API |

---

## 5. 参考 URL 清单（本次实际打开）

**Bifrost**
- https://github.com/maximhq/bifrost （README via raw.githubusercontent.com）
- https://api.github.com/repos/maximhq/bifrost （curl 元信息）
- https://docs.getbifrost.ai/
- https://docs.getbifrost.ai/architecture/core/request-flow.md
- https://docs.getbifrost.ai/deployment-guides/config-json/governance.md
- https://docs.getbifrost.ai/features/retries-and-fallbacks.md
- https://docs.getbifrost.ai/features/semantic-caching.md
- https://docs.getbifrost.ai/features/keys-management.md
- https://docs.getbifrost.ai/features/governance/virtual-keys.md
- https://docs.getbifrost.ai/features/governance/complexity-router.md
- https://docs.getbifrost.ai/providers/session-affinity.md
- https://docs.getbifrost.ai/llms.txt 、https://docs.getbifrost.ai/_llms/oss.md （文档索引）

**Kong AI Gateway**
- https://developer.konghq.com/ai-gateway/ （经 docs.konghq.com 301 重定向）
- https://developer.konghq.com/plugins/ai-proxy-advanced/ （经 docs.konghq.com 301 重定向）
- https://developer.konghq.com/ai-gateway/load-balancing/

**OpenRouter**
- https://openrouter.ai/docs
- https://openrouter.ai/docs/llms.txt （文档索引）
- https://openrouter.ai/docs/guides/routing/provider-selection.md
- https://openrouter.ai/docs/guides/routing/model-fallbacks.md
- https://openrouter.ai/docs/guides/best-practices/uptime-optimization.md
- https://openrouter.ai/docs/guides/features/zero-completion-insurance.md
- https://openrouter.ai/docs/api_reference/limits.md

**未打开/未获得**：Bifrost 企业版 Adaptive Load Balancing 与 Circuit Breaker 详情页（索引已定位 https://docs.getbifrost.ai/enterprise/adaptive-load-balancing.md 、https://docs.getbifrost.ai/enterprise/circuit-breaker.md ，本次未展开）；Kong ai-proxy-advanced 完整 reference schema（`groups` 数组字段）；OpenRouter 内部重试次数/退避参数（文档未公开）。
