# 同类项目调研笔记：RouteLLM / vLLM Production Stack Router / Semantic Router

> 调研日期：2026-09-30
> 目的：为 NexusFlow（Rust + Axum 智能路由网关）寻找值得吸收的设计与特性。
> **验证状态**：WebFetch 对 github.com 页面全部连接超时（3 次并行均 443 timeout），改用 `curl` 直连 `api.github.com`（仓库元信息 + contents 接口）与 `raw.githubusercontent.com`（README 与源码文件）。下文所有特性均来自本次真实拉取的文件，`file:line` 指向下载到 `/tmp` 的原始文件（行号与仓库 main 分支一致）。仓库元信息（stars/分支）来自 `GET /repos/<owner>/<repo>` 的真实输出。
>
> 对比基准：NexusFlow 自身代码（`src/router/router.rs` 3D 权重 = model.weight×0.4 + queue_factor×0.3 + window_factor×0.3 加权随机，见 `out/research/module-router.md` §3.1；`src/utils/metrics.rs:15` GlobalMetrics 仅请求数/延迟/错误率）。本次新验证：**oxcache 已在 DI 容器注册（`src/infrastructure/di.rs:131,161`）但 api 请求路径未调用缓存后端**——`src/api/openai.rs` 中 "cache" 仅出现为 `Cache-Control: no-cache` 响应头（openai.rs:82,157），即网关层当前没有任何响应缓存。

---

## 0. 三项目一览（真实拉取的元信息）

| 项目 | Stars | 语言 | 定位（官方 description） | 与 NexusFlow 的本质区别 |
|------|-------|------|--------------------------|--------------------------|
| [lm-sys/RouteLLM](https://github.com/lm-sys/RouteLLM) | 5,556 | Python | "A framework for serving and evaluating LLM routers - save LLM costs without compromising quality" | **按请求难度在强弱模型间二选一**（质量-成本权衡），不做多供应商网关、不做协议转换 |
| [vllm-project/production-stack](https://github.com/vllm-project/production-stack) | 2,648 | Python (FastAPI) + Helm/K8s | "vLLM's reference system for K8S-native cluster-wide deployment with community-driven performance optimization" | **同集群内多实例负载均衡**（KV cache 感知），后端全是同协议 vLLM，不做多供应商/协议转换/计费限流 |
| [aurelio-labs/semantic-router](https://github.com/aurelio-labs/semantic-router) | 3,931 | Python | "Superfast AI decision making and intelligent processing of multi-modal data" | **向量空间决策层库**（不代理 HTTP 流量），为 LLM/agent 提供毫秒级意图路由 |

三者均不含 NexusFlow 的核心能力：OpenAI↔Anthropic 双向协议转换、按 API key 的 GCRA 限流+熔断、多供应商密钥管理、SQLite 计量日志。反过来，三者各有一些 NexusFlow 没有的能力，见各节。

---

## 1. RouteLLM (lm-sys/RouteLLM)

### 1.1 定位与技术栈

Python 框架（paper: [arXiv:2406.18665](https://arxiv.org/abs/2406.18665)，README.md:5）。核心思想：把"简单问题"路由给便宜模型、"难问题"路由给强模型。官方数据（README.md:13-15）：**成本降低最高 85%，同时保持 95% 的 GPT-4 性能（MT Bench）**；比商业方案**便宜 >40%**。

### 1.2 核心特性

1. **强弱模型对 + 胜率预测路由**：路由器对每个 prompt 输出一个"强模型胜率"浮点数，与用户设定的 cost threshold 比较：≥ threshold → 强模型，否则弱模型（`routellm/routers/routers.py:41-45` 的 `Router.route()`）。抽象极简——新路由器只需实现一个方法 `calculate_strong_win_rate(prompt)`（README.md:198-202、routers.py:37-39）。
2. **开箱即用 4 种训练好的路由器**（README.md:171-180，实现见 routers.py）：
   - `mf`：矩阵分解模型（推荐，routers.py:210-242）；
   - `sw_ranking`：相似度加权 Elo——把 Chatbot Arena 对战记录按"与当前 prompt 的嵌入余弦相似度"加权后重算 Elo，再推出强模型胜率（routers.py:133-206）；
   - `bert`：BERT 序列分类器（routers.py:106-130）；
   - `causal_llm`：LLM 分类器（routers.py:52-102）；
   - `random`：基线（routers.py:247-253）。
   训练对是 gpt-4-1106-preview × mixtral-8x7b，但官方声称可泛化到其他模型对而无需重训（README.md:182）。
3. **每请求 cost threshold，编码在 model 字段里**：客户端用 `model="router-mf-0.11593"` 同时指定路由器与阈值，服务端解析后即时决策（README.md:136；解析实现在 `routellm/controller.py:93-103` `_parse_model_name`，阈值校验 0.0–1.0 在 controller.py:88-91）。**这是"路由策略作为请求参数"的优雅设计**。
4. **阈值校准 CLI**（README.md:138-151）：`python -m routellm.calibrate_threshold --routers mf --strong-model-pct 0.5` → 用 Chatbot Arena 公开数据集算出"让恰好 50% 请求走强模型"的阈值。把抽象的质量权衡变成可操作的百分比。
5. **评测框架**（README.md:153-169）：`routellm.evals.evaluate` 在 mmlu/gsm8k/mt-bench 上对比多个路由器，结果预计算缓存，可画帕累托曲线；新增 benchmark 只需实现 `Benchmark` 抽象类（README.md:204-206）。
6. **OpenAI 兼容服务器**：FastAPI，`POST /v1/chat/completions` + `GET /health`（`routellm/openai_server.py:117,143`）；底层用 LiteLLM 统一对接各供应商（controller.py:7,153）。
7. **可观测细节**：Controller 按 `router × 被路由到的模型` 计数（`controller.py:56,113` `model_counts`）；路由只看 messages 最后一轮（controller.py:108-110，注释明确承认"路由器只用第一轮数据训练"是已知局限）。

### 1.3 与 NexusFlow 的差异

| 维度 | RouteLLM | NexusFlow |
|------|----------|-----------|
| 路由粒度 | 强弱**两**模型间二选一（win-rate vs threshold） | 多供应商多模型的加权随机（3D 权重） |
| 路由依据 | **prompt 内容语义**（训练的分类器） | **负载侧信号**（queue/window），完全不看 prompt 内容 |
| 路由策略来源 | 每请求由客户端在 model 字段指定 | 服务端全局策略 + X-Vendor/X-Model 头提示 |
| 成本控制 | 显式的 cost-quality threshold + 校准工具 | 仅隐含在 model.weight 静态配置里 |
| 供应商接入 | LiteLLM 全托管 | 自研 vendor trait（含并发/rate limit/健康） |
| 评测 | 内置 benchmark 框架 | 无路由质量评测 |

**核心洞察：NexusFlow 的路由只用了"负载维度"，完全没有"请求难度维度"。** RouteLLM 证明后者才是省钱的大头（85% vs NexusFlow 加权随机的收益主要是均衡而非省钱）。

---

## 2. vLLM Production Stack 的 Router (vllm-project/production-stack)

### 2.1 定位与技术栈

K8s 原生的 vLLM 生产部署参考栈：Helm 安装，三大件 = serving 引擎 + **请求路由器**（FastAPI，`src/vllm_router/`）+ Prometheus/Grafana 可观测栈（README.md:38-48）。路由器对客户端暴露与 vLLM 相同的 OpenAI 接口（README.md:77）。

### 2.2 核心特性

1. **8 种路由逻辑**（`src/vllm_router/routers/routing_logic.py:54-62` `RoutingLogic` 枚举：ROUND_ROBIN / SESSION_BASED / KVAWARE / LOADAWARE / PREFIXAWARE / DISAGGREGATED_PREFILL / DISAGGREGATED_PREFILL_ORCHESTRATED / PRIORITY）：
   - **Session 亲和**：从请求头（或 body 字段）提取 session key，经 `uhashring.HashRing` 一致性哈希定位后端，端点增减时增量维护哈希环（routing_logic.py:118-137 `_update_hash_ring`，230 起 `SessionRouter`）。多轮对话落到同一实例以最大化 KV cache 复用（README.md:43-44）。
   - **KV-aware**：向 LMCache controller 发 `LookupMsg` 查询"哪个实例持有本 prompt 最长的 KV cache 前缀"，命中超过阈值（默认 2000 token，routing_logic.py:294）就走该实例；未命中回退 session 哈希 → 再回退最低 QPS 实例（routing_logic.py:284-478）。tokenizer 本地失败时回退远端 `/tokenize` API（routing_logic.py:395-411）。
   - **Load-aware**：`score(i) = matched_tokens(i)/prompt_tokens − β × relative_load(i)`，`relative_load = (load−mean)/max(1,mean)`，β 可经环境变量 `LOADAWARE_BETA` 运行时调节（routing_logic.py:65-90, 481-498）。**"缓存亲和"与"负载均衡"用单一无量纲分数线性融合，一个旋钮可调**——注释明确解释了为什么这是可辩护的默认值（routing_logic.py:65-69）。
   - **Prefix-aware**：本地 `HashTrie`（`src/vllm_router/prefix/hashtrie.py`）对 prompt 做最长前缀匹配选实例，可设 `prefix_min_match_length`，未达阈值回退最低 QPS 但**仍把 prompt 插入 trie**（避免冷启动死锁，routing_logic.py:835-844）；chat 消息数组会拼接全部消息文本（含多模态 text part）再匹配（routing_logic.py:804-828）。
   - **Priority**：优先级解析链 = 头 `x-request-priority` > body 字段 `priority` > 默认值，畸形值降级为缺失（routing_logic.py:883-908）；高优先级请求走"最少在途请求"实例（`in_prefill + in_decoding`，routing_logic.py:910-915, 956-960），其余全部轮转（含最少负载实例，**保证没有实例被保留闲置**，routing_logic.py:861-862）；且把解析出的优先级**回写进转发 body** 以便引擎侧抢占（routing_logic.py:952, 注释 934-937）。
   - **QPS 兜底**：`_qps_routing` 选滑动窗口内最低 QPS 的实例，是多个路由器的公共回退（routing_logic.py:94-116）。
2. **主动健康检查**：`--static-backend-health-checks` 每 60s 向后端发真实 dummy 请求，失败节点打警告并**从路由候选中剔除**；不同模型类型（chat/completion）探测不同端点（router README.md:99-109，`--static-model-types`）。
3. **服务发现**：`static`（逗号分隔 URL+模型清单）或 `k8s`（经 K8s API 自动发现 pod、label selector 过滤、自动容错）（router README.md:25-34, 9）。
4. **动态配置热更新**：`--dynamic-config-yaml/-json` 指定文件，路由器**每 10 秒 watch 文件变更**热加载；当前生效配置**反射在 `/health` 响应里**（router README.md:111-186），运维一眼可见。
5. **每实例滑动窗口统计**（`src/vllm_router/stats/request_stats.py`）：`RequestStats` 含 qps / ttft / in_prefill / in_decoding / finished / uptime / avg_decoding_length / avg_latency / **avg_itl** / num_swapped（request_stats.py:21-42）；`MovingAverageMonitor` 用双 deque 实现可配置窗口的滑动平均（request_stats.py:47-93）。抓取间隔 `--engine-stats-interval`（默认 30s）、窗口 `--request-stats-window`（默认 60s）可调（router README.md:41-44）。
6. **Prometheus + Grafana**：`/metrics` 暴露每模型 Gauge：TTFT、ITL、QPS、prefill/decoding/running/waiting/swapped、GPU prefix cache 命中率/查询数、健康 pod 数，以及**路由器自身 CPU/内存**（`src/vllm_router/routers/metrics_router.py:22-58`）；Grafana 面板含 KV 使用率、缓存命中率等 7 类图（README.md:89-105）。语义缓存还有专属 Gauge：hits/misses/hit_ratio/size/latency（`experimental/semantic_cache_integration.py:29-49`）。
7. **实验性语义缓存**（`src/vllm_router/experimental/semantic_cache.py`）：请求 messages 拼接（带 role 前缀）→ sentence-transformers `all-MiniLM-L6-v2` 嵌入 → 本地向量库按 **model 过滤** + 相似度阈值（默认 0.95）检索 → 命中直接返回缓存响应（含 usage）（semantic_cache.py:40-155）。两个工程亮点：
   - **initiate/complete 异步两段式**（semantic_cache.py:212-345）：请求一到就预计算嵌入，把嵌入耗时与上游转发重叠；
   - **集成层纪律**（semantic_cache_integration.py）：`skip_cache` 请求体标志可跳过、流式响应不缓存（无法可靠解析 chunk）、经 feature gate 开关（semantic_cache.py:13-32）。
8. **路由器级 OpenAI Batch/Files API**：`/v1/batches`、`/v1/files` 直接由路由器处理（`src/vllm_router/routers/batches_router.py:21`）。
9. **可插拔回调**：`callbacks` 动态配置项挂 `CustomCallbackHandler`（router README.md:127）；Sentry 错误上报带采样率（traces 0.1、profile 1.0，router README.md:58-62）；日志 text/json 双格式（router README.md:51）。
10. **Roadmap 自述短板**（README.md:50-56）：正在做"非 Python 语言的更高性能路由器、KV-cache 感知路由、更好容错"——官方自己承认 Python 路由器是性能瓶颈，侧面印证 NexusFlow 选 Rust 是对的。

### 2.3 与 NexusFlow 的差异

production-stack 是**单集群实例级 LB**，所有后端同协议（OpenAI/vLLM），所以它能做 KV/前缀亲和这类"缓存感知"路由；NexusFlow 面向多供应商，但供应商侧同样存在**前缀缓存经济性**（Anthropic prompt caching、OpenAI 自动前缀缓存、DeepSeek context caching），"亲和路由省钱"的逻辑完全可迁移。NexusFlow 目前只有 vendor 级 queue/window 信号（`src/router/router.rs:279-283`），没有：session 亲和、前缀/缓存感知、优先级、主动健康探测、Prometheus 暴露、滑动窗口分相统计、响应缓存。

---

## 3. Semantic Router (aurelio-labs/semantic-router)

### 3.1 定位与技术栈

Python 库（非网关）：用**嵌入向量空间**做毫秒级决策，替代"等 LLM 生成再决策"（README.md:13）。双版本线：0.x 稳定（pip `semantic-router<1`），main 为 1.x 重写中（README.md:21-27）。本次调研同时看了 main 的 1.x 源码结构（encoders/index/routers 三层抽象）。

### 3.2 核心特性

1. **Route = 意图 + 示例语料**：`Route(name="politics", utterances=[...])`，嵌入后入库；查询向量最近邻决定命中的 route（README.md:41-128），无命中返回 `None`（README.md:118-128）——**显式的"拒答/兜底"语义**。
2. **三层解耦抽象**（源码树）：
   - encoders：20+ 个 encoder 适配（OpenAI/Cohere/HF/FastEmbed/Ollama/本地 LlamaCpp…，另有 **BM25/TF-IDF 稀疏 encoder**、CLIP/ViT 多模态）；
   - index：local（numpy）/ hybrid_local / Pinecone / Qdrant / Postgres；
   - routers：`SemanticRouter`（纯稠密，`semantic_router/routers/semantic.py:15-40`）与 `HybridRouteLayer`（稠密+稀疏凸组合，`routers/hybrid.py:28-32` alpha 默认 0.3）。
3. **动态路由**：`Route.function_schemas` + 绑定 LLM → 不止选路还能**抽取参数/生成函数调用**；`Route.from_dynamic_route()` 可从 Python 函数/Pydantic 模型自动生成 route（`semantic_router/route.py:72,88-102,174-187`）。
4. **每 route 独立 score_threshold**（route.py:63-64,74），且有阈值优化 notebook（README.md:149）。
5. **配置一致性同步**：`auto_sync` 本地/远端 route 配置以 **hash 对齐**，不一致时 `get_utterance_diff()` 展示差异并拒绝写远端 hash（`routers/semantic.py:98-135`）——多实例部署下路由配置漂移的防御。
6. **与 production-stack 的组合范式**：vLLM 生态的 Semantic Router（Envoy ExtProc + BERT 分类，vllm-semantic-router.com）作为 production-stack 前置层做"按领域（math/code/creative/general）选模型 + 注入领域 system prompt + PII/越狱检测 + 语义缓存"，两篇文档明确分工："语义路由选最优模型，production-stack 路由最大化基础设施效率"（`tutorials/24-semantic-router-integration.md` 头部）。

### 3.3 与 NexusFlow 的差异

semantic-router 是**决策组件**不是代理：不做协议转换、不做转发、无弹性机制。对 NexusFlow 的意义不在"替代路由器"，而在**给路由器增加一个内容语义信号源**：嵌入分类（本地 BERT/轻量 encoder 即可）可以成为 NexusFlow 3D 权重之外的"第 4 维"——按意图把请求导向领域最强的供应商，或在速率压力下识别出"可降级到便宜模型"的简单请求。

---

## 4. 汇总：NexusFlow 值得吸收的设计（按建议优先级）

| # | 吸收点 | 来源（真实打开的 URL） | 对 NexusFlow 的适配判断 |
|---|--------|------------------------|--------------------------|
| 1 | **每请求路由策略放请求里**：RouteLLM 的 `router-<名>-<阈值>` model 字段语法 + 阈值 0–1 校验 | https://raw.githubusercontent.com/lm-sys/RouteLLM/main/routellm/controller.py （93-103, 88-91 行） | 高契合。NexusFlow 已有 model 字段解析与 X-Vendor/X-Model 头；可扩展出 `model="auto:cost=0.3"` 之类的每请求质量/成本偏好，兼容 OpenAI 客户端零改动 |
| 2 | **滑动窗口分相统计 + Prometheus /metrics**：TTFT/ITL/prefill/decoding/在途数按实例统计，Gauge 暴露 | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/stats/request_stats.py （21-42, 47-93 行）；https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/routers/metrics_router.py | 高契合。NexusFlow 的 GlobalMetrics（src/utils/metrics.rs:15）只有 vendor 级请求/延迟/错误率；3D 权重里的 window_factor 已需要窗口数据，应有 per-vendor 滑动窗口实体；加 `/metrics` 即接入 Grafana 生态，Rust 侧可用 prometheus crate |
| 3 | **Session 亲和（一致性哈希）**：session key → hash ring，端点增减只重排 1/n | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/routers/routing_logic.py （118-137, 230 起） | 高契合。Anthropic/OpenAI/DeepSeek 的前缀缓存都按"同一实例+相同前缀"计费打折；NexusFlow 按请求加权随机会打散前缀缓存，亲和路由是直接的省钱改造，且 Rust 侧实现一致性哈希很轻 |
| 4 | **主动健康检查 + 剔除**：定期 dummy 请求，失败节点移出候选 | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/README.md （Backend health checks 节） | 中高契合。NexusFlow 的 get_status()==Available 只反映本地限流/错误状态（见 out/research/module-router.md §1 表格），没有对上游的主动探测；补一个每分钟 /models 或极小 completion 探测即可把"上游挂了"从请求时发现提前到后台发现 |
| 5 | **语义缓存（含异步两段式与缓存纪律）**：嵌入检索相似请求直接回缓存响应；嵌入与上游转发重叠；skip_cache 标志；流式不缓存；命中率进 metrics | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/experimental/semantic_cache.py （40-155, 212-345 行）；https://api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/experimental/semantic_cache_integration.py | 中高契合。已验证 NexusFlow 的 oxcache 仅注册未使用（src/infrastructure/di.rs:131）；语义缓存可直接填这个空位，Rust 侧有 fastembed-rs/ort 可选；相似度阈值+模型过滤+跳过标志三件套照抄 |
| 6 | **前缀感知路由（HashTrie）**：prompt 最长前缀匹配选实例，未达阈值回退 QPS 但仍插入 trie | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/routers/routing_logic.py （759-850 行） | 中契合。与 #3 互补（无需 session key，自动利用重复系统提示词场景）；实现要点是"未命中也记录"防冷启动 |
| 7 | **成本阈值校准方法**：用真实流量样本反推"X% 请求走强模型"的阈值 | https://raw.githubusercontent.com/lm-sys/RouteLLM/main/README.md （Threshold Calibration 节） | 中契合。NexusFlow 的 SQLite 日志已有 token 用量预聚合，可离线校准 model.weight / 每请求 cost 阈值（#1），把拍脑袋权重变成数据驱动 |
| 8 | **优先级路由**：头 > body > 默认值的优先级解析链；高优先级→最闲实例，其余轮转不保留容量 | https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/routers/routing_logic.py （853-964 行） | 中契合。limiteron 已有限流，但无"贵宾插队"语义；实现小（一个头 + 最少在途选择），适合作为 X-Priority 头扩展 |
| 9 | **路由评测框架**：路由器在 benchmark 上的成本-质量帕累托对比，结果可缓存 | https://raw.githubusercontent.com/lm-sys/RouteLLM/main/README.md （Evaluation 节） | 中契合。NexusFlow 改路由策略（权重公式）目前无任何量化验证手段；借用 RouteLLM 的 benchmark 思路对 router 模块做离线评测（用 SQLite 日志回放即可起步） |
| 10 | **配置 hash 对齐防御**：本地/远端路由配置 hash 不一致时拒绝同步并给 diff | https://raw.githubusercontent.com/aurelio-labs/semantic-router/main/semantic_router/routers/semantic.py （98-135 行） | 低中契合。NexusFlow 的 /admin/config 热更新目前无多实例一致性保障；若未来多副本部署，配置版本 hash + diff 是现成方案 |
| 11 | **语义意图分层路由（第 4 维）**：嵌入分类把请求按领域导到专门供应商；可叠加领域 system prompt 注入与 PII/越狱门禁 | https://raw.githubusercontent.com/aurelio-labs/semantic-router/main/semantic_router/routers/semantic.py ；https://raw.githubusercontent.com/vllm-project/production-stack/main/tutorials/24-semantic-router-integration.md | 远期。NexusFlow 3D 权重全是负载信号；嵌入分类是内容信号的第一块拼图，可作为可选 filter（在 weighted_select 之前缩小候选集）而非替代 |

### 不建议照搬的部分

- **RouteLLM 的 LiteLLM 依赖**：把供应商接入整个外包给 Python 库，与 NexusFlow 自研 vendor trait（含并发/rate limit/健康管理）路线冲突；NexusFlow 的 Rust 供应商层是差异化优势。
- **production-stack 的 K8s 服务发现/Helm/operator**：面向同集群 GPU 实例编排，与"聚合多家 SaaS API"的定位不符；static 健康检查思路（#4）可取，机制不必照搬。
- **KV-aware 路由的 LMCache controller 依赖**：NexusFlow 的上游是外部 SaaS，拿不到 KV cache 布局；只能取其"亲和思想"（#3/#6），机制不可复制。
- **semantic-router 的向量库外置（Pinecone/Qdrant）**：对网关内嵌的轻量分类是过度设计；本地内存索引（其 local index）才是对等物。

---

## 5. 证据 URL 清单（本次真实拉取）

GitHub API 元信息（stars/语言/分支）：
- https://api.github.com/repos/lm-sys/RouteLLM
- https://api.github.com/repos/vllm-project/production-stack
- https://api.github.com/repos/aurelio-labs/semantic-router

README：
- https://raw.githubusercontent.com/lm-sys/RouteLLM/main/README.md
- https://raw.githubusercontent.com/vllm-project/production-stack/main/README.md
- https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/README.md
- https://raw.githubusercontent.com/aurelio-labs/semantic-router/main/README.md

RouteLLM 源码：
- https://raw.githubusercontent.com/lm-sys/RouteLLM/main/routellm/routers/routers.py
- https://api.github.com/repos/lm-sys/RouteLLM/contents/routellm/controller.py （raw 两次超时，经 contents API 取得，内容 170 行）
- https://api.github.com/repos/lm-sys/RouteLLM/contents/routellm/openai_server.py

production-stack 源码/文档：
- https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/routers/routing_logic.py
- https://raw.githubusercontent.com/vllm-project/production-stack/main/src/vllm_router/experimental/semantic_cache.py
- https://api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/experimental/semantic_cache_integration.py
- https://api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/stats/request_stats.py
- https://api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/routers/metrics_router.py
- https://api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/routers/batches_router.py
- https://raw.githubusercontent.com/vllm-project/production-stack/main/tutorials/24-semantic-router-integration.md
- 仓库文件树：https://api.github.com/repos/vllm-project/production-stack/git/trees/main?recursive=1 （610 文件，确认 kvaware/loadaware/prefixaware/priority 测试与 experimental/ 语义缓存存在）

semantic-router 源码：
- https://raw.githubusercontent.com/aurelio-labs/semantic-router/main/semantic_router/routers/semantic.py
- https://raw.githubusercontent.com/aurelio-labs/semantic-router/main/semantic_router/routers/hybrid.py
- https://api.github.com/repos/aurelio-labs/semantic-router/contents/semantic_router/route.py
- 仓库文件树：https://api.github.com/repos/aurelio-labs/semantic-router/git/trees/main?recursive=1 （202 文件，确认 20+ encoders / 5 种 index / hybrid 路由存在）

NexusFlow 侧验证（本仓库）：
- `src/infrastructure/di.rs:21,131,161`（oxcache 注册）；`src/api/openai.rs:82,157`（仅 no-cache 头，无缓存调用）；`src/utils/metrics.rs:15`（GlobalMetrics 字段）；`out/research/module-router.md`（3D 权重与路由策略既有结论）
