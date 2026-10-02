# 同类项目调研：云厂商/开源 AI 网关（Portkey Gateway · Cloudflare AI Gateway · Higress）

> 调研日期：2026-09-30
> 调研者：workflow 子代理「同类项目-cloudgw」
> 对照对象：NexusFlow（Rust + Axum 智能路由代理，3D 权重路由、零拷贝透传、按状态码智能重试、GCRA 限流、双协议转换、SQLite 批量日志）
>
> **证据说明**：本文所有特性均来自本会话中真实抓取的页面/源码，每节标注 URL。抓取方式为 `curl`（api.github.com 元信息、raw.githubusercontent.com 源码、developers.cloudflare.com 的 `index.md` markdown 版本）。Portkey 官网文档（portkey.ai/docs）为客户端渲染，curl 抓不到正文、WebFetch 超时，因此 Portkey 部分以 GitHub README + 仓库源码为据；未能抓到的文件（如 `src/handlers/retryHandler.ts`，curl 反复超时 exit 28）在文中如实标注。

---

## 1. Portkey AI Gateway（Portkey-AI/gateway）

### 1.1 元信息与定位
- 仓库：https://github.com/Portkey-AI/gateway
- 一句话（README 原文）：*"A blazing fast AI Gateway with integrated guardrails. Route to 1,600+ LLMs, 50+ AI Guardrails with 1 fast & friendly API."*
- 元信息（来自 `curl https://api.github.com/repos/Portkey-AI/gateway`）：
  - 语言 **TypeScript**，License **MIT**，**13,111 stars / 1,321 forks**
  - 描述："Route to 1,600+ LLMs, 50+ AI Guardrails with 1 fast & friendly API"
  - README 强调：<1ms 延迟、122kb 体积、每天处理 10B+ tokens
  - 注意：仓库已有 2.0 pre-release 分支（企业网关核心正在合入开源版）
- 部署形态：`npx @portkey-ai/gateway` 一行启动（8787 端口）、Docker、Node.js Server、**Cloudflare Workers**、Replit、AWS EC2 CloudFormation 一键部署（README「Quickstart」节）。

### 1.2 架构（来自 `GET /repos/.../contents/src`，本会话已列目录）
```
src/
├── handlers/     # 每种 API 一个 handler：chatCompletions / messages / embeddings /
│                 #   imageGenerations / createSpeech / createTranscription / realtime /
│                 #   proxyHandler / retryHandler / streamHandler / modelResponses ...
├── middlewares/  # adminAuth / cache / hooks / log / requestValidator
├── services/     # conditionalRouter.ts / transformToProviderRequest.ts / realtimeLlmEventParser
├── providers/    # 每个上游 provider 一个适配目录
├── apm/ data/ errors/ shared/ types/
```
- 与 NexusFlow 的同构点：provider 适配层 ≈ `src/vendor/`；协议转换 ≈ `src/protocol/`；handlers ≈ `src/api/`。
- 与 NexusFlow 的最大差异：Portkey 把**路由编排做成请求级 JSON Config**（随请求传入，也可集中管理），而不是 NexusFlow 的静态 `config.toml`；支持本地 **Gateway Console**（`http://localhost:8787/public/`）直接看本地日志（README「Quickstart」节）。

### 1.3 核心特性（README「Core Features」节，URL 同上）
**路由/可靠性**
- **Fallbacks**：可指定触发 fallback 的错误（状态码）；源码证据 `src/types/requestBody.ts` 中 `Strategy { mode: StrategyModes; onStatusCodes?: Array<number>; ... }`
- **Automatic Retries**：最多 5 次、指数回退；`RetrySettings { attempts; onStatusCodes: number[]; useRetryAfterHeader?: boolean }` —— **支持遵循上游 Retry-After 头**（NexusFlow 的 500 固定间隔重试没有这一项）
- **Load Balancing**：带权重地在多个 API key / provider 间分配；`Targets { weight?: number; ... }`
- **Request Timeouts**：`Targets { requestTimeout?: number }`，按 target 粒度
- **多模态 + Realtime API**（websocket）、MCP Gateway（统一认证、访问控制、每次工具调用日志、身份转发）

**策略编排（最有价值的部分）**
- `StrategyModes = LOADBALANCE | FALLBACK | SINGLE | CONDITIONAL`（`src/types/requestBody.ts:22-27`，本会话已抓取）
- **Targets 可递归嵌套**：`Targets { targets?: Targets[] }` —— 意味着可以表达「先按权重负载均衡，全部失败后 fallback 到另一组负载均衡」这样的策略树；且每个 target 可各自带 retry / weight / cache / overrideParams / guardrails
- **条件路由引擎** `src/services/conditionalRouter.ts`（本会话已抓取全文，156 行）：MongoDB 查询风格操作符 `$eq/$ne/$gt/$gte/$lt/$lte/$in/$nin/$regex` + 逻辑组合 `$and/$or`，对请求 metadata/params/url 求值，按 `conditions[].query → then` 顺序匹配，支持 `strategy.default` 兜底
- Config 既支持请求头传入也支持托管保存（README：「Configs in the LLM gateway allow you to create routing rules, add reliability and setup guardrails」）

**安全/护栏**
- **Guardrails**：40+ 预置检查 + 自带护栏（BYO），输入/输出均可配置；README 示例里 `"output_guardrails": [{"default.contains": {...}, "deny": true}]` 直接拒绝含敏感词的回答
- **Virtual Keys / RBAC / 合规**（SOC2、HIPAA、GDPR、CCPA）—— 多为托管/企业版能力

**成本/可观测**
- **Smart caching**：简单缓存 + **语义缓存**（标注为 hosted/enterprise 版）
- **Usage analytics**：请求数、延迟、成本、错误率；配套开源项目 **Portkey-AI/models**（40+ provider、2300+ 模型的定价数据库，供成本追踪用）—— 这是个独立的、可直接复用的开源资产（https://github.com/Portkey-AI/models）
- **Prompt Template 管理**、agent 框架集成（LangChain/CrewAI/Autogen/LlamaIndex…）

### 1.4 未抓到的部分（如实标注）
- `src/handlers/retryHandler.ts`：curl 至 raw.githubusercontent.com 反复超时（exit 28），未读到实现细节；重试行为的描述以 README 为准。
- portkey.ai/docs 各文档页（fallback/loadbalancer/cache）：客户端渲染 + WebFetch 超时，未读到正文；所有结论以 README 与 `src/types/requestBody.ts` 源码为据。

---

## 2. Cloudflare AI Gateway（闭源托管服务，文档公开）

### 2.1 定位与形态
- 文档入口：https://developers.cloudflare.com/ai-gateway/ （本会话抓取，2026-04-20 版）
- 定位（原文）：*"Cloudflare's AI Gateway allows you to gain visibility and control over your AI apps... with analytics and logging and then control how your application scales with features such as caching, rate limiting, as well as request retries, model fallback, and more."*
- **不是开源项目**：部署在 Cloudflare 边缘网络的托管服务；技术栈为 Cloudflare Workers。与 NexusFlow 的对照意义在于**产品能力面**而非代码。
- 文档全目录索引：https://developers.cloudflare.com/ai-gateway/llms.txt （本会话抓取；任何页面可用 `curl -H "Accept: text/markdown"` 取 markdown 版）

### 2.2 可观测（https://developers.cloudflare.com/ai-gateway/observability/analytics/ ，本会话抓取）
- 五类指标：Requests / Token Usage / **Costs** / Errors / **Cached Responses（缓存命中率）**，可按时间过滤
- **GraphQL API** 查询用量数据（按 model/provider/gateway/分钟 聚合），页面给出完整查询示例 —— NexusFlow 只有进程内 GlobalMetrics，没有可查询的持久化指标 API

### 2.3 缓存（https://developers.cloudflare.com/ai-gateway/features/caching/ ，本会话抓取）
- 默认缓存键 = SHA-256(provider + endpoint + model + provider 认证头 + 完整请求体) → **全等匹配**；任何 body 差异都是新条目
- **每请求级控制全走 HTTP 头**：
  - `cf-aig-skip-cache`：跳过缓存
  - `cf-aig-cache-ttl`：单请求 TTL（最小 60s，最长 1 个月）
  - `cf-aig-cache-key`：**自定义缓存键**（业务可用业务语义归并等价请求；首次未命中回源，之后共享缓存）
  - 响应头 `cf-aig-cache-status: HIT|MISS` 让客户端知道是否命中
- 语义缓存官方声明"计划中"（原文：*"We plan on adding semantic search for caching in the future"*）—— 即云厂商也尚未默认提供
- NexusFlow 现状：**没有任何响应缓存**（兄弟库 oxcache 只用于自身缓存场景），这是能力空白

### 2.4 限流与消费限额
**Rate limiting**（https://developers.cloudflare.com/ai-gateway/features/rate-limiting/ ，本会话抓取）
- 支持请求次数限流，可选 **fixed window / sliding window** 两种算法，超限返回 429
- NexusFlow 用 limiteron GCRA（令牌桶），算法本身更精细；但 Cloudflare 的「按网关默认配置 + 简单可选算法」对用户更易理解

**Spend limits（消费限额，NexusFlow 完全没有的能力）**（https://developers.cloudflare.com/ai-gateway/features/spend-limits/ ，本会话抓取）
- 按**美元预算**限流：实时按 token 用量 × 模型定价累计花费，超预算直接 429，窗口滚动/固定可选
- 维度系统：可按 provider / model / 自定义 metadata（如 user_id、agent_id）分桶；每个维度有两种模式：
  - **Split by value**：每个取值一个独立预算桶（→ 天然实现"每个用户/每个 agent 自己的预算"）
  - **Filter by value**：规则只对该取值生效
- 最多 20 条规则/网关；官方诚实标注"最终一致"（并发突发可能短暂超限）

### 2.5 重试/超时/回退
**Request handling**（https://developers.cloudflare.com/ai-gateway/configuration/request-handling/ ，本会话抓取）
- 重试三参数全部可调且**每请求头可覆盖**：`cf-aig-max-attempts`（最多 5）、`cf-aig-retry-delay`（最长 60s）、`cf-aig-backoff`（**constant | linear | exponential** 三种回退曲线）
- 超时语义：**以响应首字节到达为准**（流式场景下只要首包及时，后续可慢慢流）—— 比整请求超时对流式友好得多
- 最后一次重试会等待请求完成、不再受超时约束
- NexusFlow 现状：重试策略硬编码为「429 指数回退、500 固定间隔」两类，不可配置、无 Retry-After 头遵循、无整体超时配置

**Universal Endpoint + Fallbacks**（https://developers.cloudflare.com/ai-gateway/usage/universal/ ，本会话抓取；该端点已标注 Deprecated，由 Dynamic Routing 接替）
- 请求体为**步骤数组**：`[{provider, endpoint, authorization, query}, ...]`，前者失败自动落到下一个 —— 回退序列由客户端在请求里声明
- 响应头 **`cf-aig-step`** 告知实际用了第几个 fallback（0=主模型）—— 方便客户端与日志归因
- 每个 provider 步骤可带独立 `requestTimeout`，超时同样触发回退

### 2.6 动态路由（最值得吸收的单个特性）
（https://developers.cloudflare.com/ai-gateway/features/dynamic-routing/ 与 .../dynamic-routing/json-configuration/ ，本会话均抓取）
- **路由 = 版本化的节点图（flow）**，可存为命名路由（如 `dynamic/support`），客户端把路由名填在 model 字段调用；每次修改产生新 draft，**部署后可即时回滚版本**
- 节点类型：Start / **Conditional**（对 metadata、请求头、请求体做条件判断，支持 MongoDB 风格 `$eq/$ne/$in/$and/$or`）/ **Percentage**（按概率分流，最多 5 输出，天然支持 A/B 与灰度）/ Model / **Rate Limit**（超阈值切 fallback）/ **Budget Limit**（超预算切 fallback）/ End
- 用途示例（官方）：付费/免费用户走不同模型；按用户/项目/团队限制预算；A/B 测试与渐进放量
- 关键洞察：**"配额/预算超限时不是拒绝而是自动切换到 fallback 模型"** —— 把成本控制变成了降级路由

### 2.7 其他能力（文档目录 llms.txt + 已抓页面）
- **BYOK**：provider 密钥集中存储，请求只引用（https://developers.cloudflare.com/ai-gateway/configuration/bring-your-own-keys/）
- **Custom costs**：按请求覆盖模型成本（自建/微调模型计价）（.../configuration/custom-costs/）
- **DLP**（数据防泄漏，扫描 prompt/响应中的敏感信息）、**Guardrails**（对 prompt 与响应做有害内容检测，可 flag 或 block，https://developers.cloudflare.com/ai-gateway/features/guardrails/ ，本会话抓取）
- **Evaluations**：对日志做数据集评估 + 人工 thumbs-up/down 标注
- **OpenTelemetry 导出**（trace spans 到任意 OTLP 后端，.../observability/otel-integration/）、Logpush、自定义 metadata
- **Unified Billing**、认证网关（要求 token）、Cloudflare Access 身份门禁、审计日志

---

## 3. Higress（higress-group/higress，原 alibaba/higress）

### 3.1 元信息与定位
- 仓库：https://github.com/higress-group/higress （`curl https://api.github.com/repos/alibaba/higress` 返回 301 Moved Permanently，项目已迁至 higress-group 组织）
- 元信息（`curl -L https://api.github.com/repositories/558188628`，本会话）：
  - 描述："🤖 AI Gateway | AI Native API Gateway"，语言 **Go**，**9,474 stars / 1,319 forks**，Apache-2.0，**CNCF Sandbox**
- 架构（README）：**Istio + Envoy** 之上的云原生 API 网关，业务逻辑以 **Wasm 插件**（Go/Rust/JS）热插拔；主打「配置变更毫秒生效、不中断长连接」（对 AI 的 SSE 长流式场景关键）、完整的请求/响应体流式处理、插件沙箱隔离 + 无损热更新
- 部署：单条 docker run 起 all-in-one（含 UI 控制台 :8001，网关 :8080/:8443），也支持 K8s Helm

### 3.2 AI 能力 = 一组 Wasm 插件（`plugins/wasm-go/extensions/`，本会话列出目录，54 个插件中 19 个 `ai-*`）
`ai-agent, ai-cache, ai-context-limit, ai-endpoint-picker, ai-history, ai-image-reader, ai-intent, ai-json-resp, ai-load-balancer, ai-prompt-decorator, ai-prompt-template, ai-proxy, ai-quota, ai-rag, ai-search, ai-security-guard, ai-statistics, ai-token-ratelimit, ai-transformer`

**ai-proxy**（协议转换，https://github.com/higress-group/higress/tree/main/plugins/wasm-go/extensions/ai-proxy/provider ，本会话列出目录）
- 67 个 Go 文件、约 60 个 provider（去 test）：openai、claude、gemini、qwen、moonshot、doubao、deepseek、zhipuai、hunyuan、baichuan、minimax、spark、yi、stepfun、coze、dify、ollama、vllm、triton、vertex、bedrock、azure、openrouter、groq、cohere、mistral、deepl、kling(视频)…（国内外全兼顾）
- 有 `claude_to_openai.go` —— 与 NexusFlow 的 OpenAI↔Anthropic 双向转换同题；另有 `failover.go`、`retry.go` 独立封装

**ai-token-ratelimit**（https://github.com/higress-group/higress/blob/main/plugins/wasm-go/extensions/ai-token-ratelimit/README.md ，本会话抓取全文）
- **基于 Redis 的 Token 限流**（不是请求数限流！）：`token_per_second/minute/hour/day` 四档
- 三种模式：规则级全局阈值；**Key 级动态限流**（key 可取 URL 参数 / 请求头 / consumer / Cookie / 客户端 IP，支持 `limit_by_per_*` 按 value 分桶 + 正则）；混合模式（全局兜底 + 细分叠加，任一触发即拒绝）
- 拒绝时返回标准 `X-RateLimit-Reset` 等响应头，rejection 状态码/文案可配
- **对 NexusFlow 的意义**：LLM 的成本由 token 决定，请求数限流挡不住"单请求 100 万 token"；NexusFlow 的 limiteron GCRA 是按请求计的，token 维度限流是明确的能力空白

**ai-cache**（https://github.com/higress-group/higress/blob/main/plugins/wasm-go/extensions/ai-cache/README.md ，本会话抓取全文）
- **双模缓存**：精确字符串匹配缓存 + **语义缓存**（vector + embedding），可叠加（字符串命中优先，未命中走向量检索）
- 向量库支持 7 种：dashvector / chroma / elasticsearch / weaviate / pinecone / qdrant / milvus；embedding 支持 dashscope / openai / azure / cohere / ollama / huggingface 等
- 缓存键策略可配：`lastQuestion`（默认）/`allQuestions`/`disabled`；相似度阈值 `threshold` + 比较方向 `thresholdRelation`（Cosine/DotProduct 用 gt，Euclidean 用 lt）
- 请求头 `x-higress-skip-ai-cache: on` 跳过缓存

**ai-load-balancer**（https://github.com/higress-group/higress/blob/main/plugins/wasm-go/extensions/ai-load-balancer/README.md ，本会话抓取全文）
- 为 LLM 后端提供**热插拔 LB 策略**（关掉插件退回 Envoy 原生轮询/最少请求/随机/一致性哈希）：
  - `global_least_request`：**Redis 全局在途请求数最小**（跨网关实例共享计数，选 host → 计数+1 → 响应后 -1）
  - `prefix_cache`：**按 prompt 前缀匹配选节点** —— 同一长 system prompt 的请求总是路由到已持有该 KV cache 前缀的推理节点（vLLM 等 prefix caching 生效），未命中再退 global_least_request
  - `endpoint_metrics`：按 LLM 服务自身暴露的 metrics 做 LB
  - `cluster_metrics` / `cluster_hash`：网关统计的服务级指标 LB；按指定请求头 FNV-1a 一致性哈希做会话亲和（支持按权重分流）
- **对 NexusFlow 的意义**：NexusFlow 的 3D 权重是「静态权重 × 队列因子 × 时间窗口」的**网关侧自观测**；Higress 提供了两个 NexusFlow 没有的信号维度——全局共享的跨实例计数（NexusFlow 单实例下 GlobalMetrics 等价，但多实例部署就失真）与**面向推理引擎 KV cache 的 prompt 前缀亲和**（agent 场景 system prompt 高度重复，命中可显著省 TTFT 与算力）

**ai-quota**（https://github.com/higress-group/higress/blob/main/plugins/wasm-go/extensions/ai-quota/README.md ，本会话抓取全文）
- 按 consumer 分配固定 **token 配额**，并提供**配额管理 API**（查询/刷新/增减 quota，admin consumer 身份鉴权）→ 商业化"卖 token 包"的基础设施；需与 key-auth/jwt-auth + ai-statistics 配合
- 与 Cloudflare Spend limits 互补：CF 是"超了就 429/降级"，Higress 是"预付费配额 + 可运营充值"

**ai-statistics**（https://github.com/higress-group/higress/blob/main/plugins/wasm-go/extensions/ai-statistics/README.md ，本会话抓取全文）
- AI 可观测基座：metric + log + trace；默认指标含**输入/输出 token、流式首 token RT、请求总 RT**，支持**网关/路由/服务/模型四个维度**观测
- 可配置属性提取（`value_source`: fixed/request_header/request_body/response_header/response_body/response_streaming_body，jsonpath 提取、流式提取规则 first/replace/append）
- `session_id_header` 自动追踪多轮 Agent 会话；`use_default_response_attributes` 提供轻量模式（不缓冲流式体，适配高并发生产）

### 3.3 与 NexusFlow 的架构差异
| 维度 | NexusFlow | Higress |
|---|---|---|
| 数据面 | 自研 Axum 单进程 | Envoy + Istio（C++/Go，生产级长连接处理） |
| 扩展模型 | Rust 模块 + feature 门控 | Wasm 插件（Go/Rust/JS），热插拔、无损热更新 |
| 状态共享 | 进程内（GlobalMetrics、内存权重） | Redis 外置（token 限流/quota/全局 LB 计数共享） |
| 控制面 | config.toml + /admin/config 热更新 | Console UI + K8s CRD/Gateway API |
| 协议转换 | OpenAI↔Anthropic 双向 | 60+ provider 互转（含 ASR/TTS/图像/视频模型） |

---

## 4. 对 NexusFlow 的可吸收设计（汇总排序）

1. **每请求可覆盖的路由/弹性配置 + HTTP 头控制面**（三家共性）：`cf-aig-max-attempts/backoff/cache-key/...` 与 Portkey 的请求级 Config 证明「网关默认值 + 请求级覆盖」是 AI 网关的标配交互。NexusFlow 的重试/限流参数目前只能改 config.toml。
2. **条件路由引擎**（Portkey `conditionalRouter.ts`、Cloudflare Conditional 节点）：MongoDB 风格操作符（$eq/$gt/$in/$regex/$and/$or）对 metadata 求值选 target，实现"付费用户→强模型、免费→便宜模型"。NexusFlow 只有 3D 权重加权随机，没有基于请求属性的路由。
3. **策略树（嵌套 targets）**（Portkey `Targets.targets?: Targets[]`）：loadbalance 内嵌 fallback、fallback 内嵌 loadbalance 可任意组合，替代 NexusFlow 目前 strategy 平铺的表达力。
4. **Token 维度限流与配额**（Higress ai-token-ratelimit/ai-quota、Cloudflare Spend Limits）：按 token 而非请求数限流；按美元预算/预付费配额控制成本，超限 429 或降级。NexusFlow 完全没有。
5. **响应缓存（精确 + 每请求 TTL/自定义键）与语义缓存**（Cloudflare caching、Higress ai-cache、Portkey smart caching）：SHA-256( provider+model+body ) 为键、HIT/MISS 响应头、语义缓存走向量库。NexusFlow 完全没有。
6. **首字节超时 + Retry-After 遵循 + 三种回退曲线**（Cloudflare request-handling、Portkey useRetryAfterHeader）：流式场景超时应按 TTFB 计；重试间隔应尊重上游 Retry-After。
7. **动态路由 = 版本化节点图**（Cloudflare Dynamic Routing）：Conditional/Percentage/RateLimit/BudgetLimit 节点组成 flow，路由即模型名，**预算超限自动 fallback 到便宜模型**（把成本控制变成降级路由），版本可回滚。
8. **Prompt 前缀亲和 LB**（Higress `prefix_cache`）：agent 场景 system prompt 重复度高，按前缀哈希路由可命中推理引擎 KV cache，显著降 TTFT。NexusFlow 的 3D 权重不含该信号。
9. **provider 定价数据库做成本核算**（Portkey-AI/models，开源 2300+ 模型定价）：NexusFlow 已有 Token 预聚合统计，接上定价表即可产出成本指标与预算控制。
10. **结构化 AI 可观测**（Higress ai-statistics 四维指标、Cloudflare GraphQL analytics、OTel 导出）：首 token RT、缓存命中率、按 model/provider/consumer 维度可查询的持久化指标，而非仅进程内快照。

---

## 附录：本会话实际抓取的证据清单

| # | 内容 | 方式 | 结果 |
|---|---|---|---|
| 1 | `api.github.com/repos/Portkey-AI/gateway` | curl | ✅ 元信息（stars/语言/license） |
| 2 | `raw.githubusercontent.com/Portkey-AI/gateway/main/README.md` | curl | ✅ 全文 338 行 |
| 3 | `.../contents/src`、`.../contents/src/handlers|middlewares|services|apm|docs` | curl API | ✅ 目录结构 |
| 4 | `.../src/services/conditionalRouter.ts` | curl | ✅ 全文 156 行 |
| 5 | `.../src/types/requestBody.ts` | curl | ✅ 关键类型（StrategyModes/RetrySettings/Targets） |
| 6 | `.../src/handlers/retryHandler.ts` | curl ×3 | ❌ 超时 exit 28，未获取 |
| 7 | portkey.ai/docs/api-reference/config | curl + WebFetch | ❌ 客户端渲染/WebFetch 超时 |
| 8 | developers.cloudflare.com/ai-gateway/（概览）、llms.txt、caching、rate-limiting、guardrails、dynamic-routing、dynamic-routing/json-configuration、spend-limits、configuration/request-handling、usage/universal、observability/analytics | curl（index.md/markdown） | ✅ 全部抓到并读取 |
| 9 | `api.github.com/repos/alibaba/higress` → 301 → `repositories/558188628` | curl | ✅ 元信息（已迁移至 higress-group） |
| 10 | higress README.md | curl | ✅ 全文 |
| 11 | higress `plugins/wasm-go/extensions` 与 `ai-proxy/provider` 目录 | curl API | ✅ 54 插件 / 67 provider 文件清单 |
| 12 | higress ai-token-ratelimit / ai-cache / ai-load-balancer / ai-quota / ai-statistics 五个插件 README | curl | ✅ 全文（中文） |
