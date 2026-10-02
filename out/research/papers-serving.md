# LLM 服务调度与负载均衡 — 论文调研笔记（面向 NexusFlow 优化）

- **日期**: 2026-09-30
- **工具**: liuxiang `search_papers.py`（4 轮检索 + 1 轮按标题补查，3 篇重点论文下载全文）
- **调研对象**: NexusFlow 智能路由网关（Rust + Axum）。当前路由 = 3D 权重加权随机（`model.weight×0.4 + queue_factor×0.3 + window_factor×0.3`，`src/router/router.rs:286-300`），选择算法为轮盘赌单次抽取（`src/router/router.rs:222-277`）；重试按状态码分类（429 指数回退 / 500 固定间隔）；limiteron GCRA 限流 + 熔断；`GlobalMetrics`（`src/utils/metrics.rs:15-20`）已有 per-vendor 请求数/成功失败/累计时长（`src/utils/metrics.rs:129-165`），**但未接入路由权重**（`calculate_weight` 只读 queue_len 与 window_info，`src/router/router.rs:279-283`）。
- **视角声明**: NexusFlow 是网关/代理层，不做 GPU 内 batch 调度（非 vLLM/SGLang），因此引擎内部优化（P/D 分离、量化、batching 策略）类论文只作参考；多实例/多供应商的「一次分发」层论文最相关。

## 实际执行的检索命令

```bash
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "LLM serving load balancing scheduling" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "prefill decode disaggregation inference" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "request scheduling LLM inference server" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "LLM router routing between multiple models serving" --source multi --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "Efficient Training-Free Online Routing for High-Volume Multi-LLM Serving" --mode title --source auto --limit 3   # 未命中目标论文（S2 429 降级，openalex 无此条）
```

## 一、重点论文详析（3 篇，已下载全文：`/tmp/liuxiang_serving/`）

### 1. RouteBalance: Fused Model Routing and Load Balancing for Heterogeneous LLM Serving
- **作者**: Wei Da, Evangelia Kalyvianaki（University of Cambridge）
- **年份/venue**: 2026，arXiv:2606.17949（脚本 venue 输出为 arXiv；正文为 ACM 投稿模板，尚未见正式会议信息）
- **核心思想（机制级）**:
  1. **路由即「对具体模型实例的在线分配」**，而非"选模型名"。把 model router（管质量/成本）和 serving LB（管队列）两层合并为一次联合打分：对每个「请求-实例对」计算 weighted **quality–latency–cost** 分数，用 **3-simplex 可调权重**（Quality/Latency/Cost 三个角 + 平衡点）暴露命名工作点；扫一条权重向量即可遍历质量-成本-吞吐前沿。
  2. **核心因果结论（四臂隔离实验）**：收益主要来自"在模型选择时就给延迟定价"（pricing latency at model-selection time）。质量-only 路由在 30 req/s 下把平均 E2E 延迟从 2.3 s 推到 >60 s（流量聚集在少数高质量副本，便宜档闲置）；load-only LB 则白白损失 +0.04~0.05 的质量。**层级分工结论：跨 tier（不同模型/档位间）必须联合考虑负载；同 tier 内部用 reactive queue depth 就足够**，学习型预测器只贡献校准和 SLO headroom。
  3. **热路径开销控制**：单次 CPU 端 sentence embedding → k=10 KNN 头输出每模型质量 + 期望输出长度（批内摊销 prompt 相关估计）；in-process gradient-boosted 每档位延迟头；**dead-reckoned 实例状态**（本地推算实例负载而非轮询）+ 贪心 LPT（longest-processing-time）分配避免在陈旧负载信号上 herding。决策开销 ≈32 ms @12 req/s。
- **对 NexusFlow 的适用性: 高（最直接对标）**
  - NexusFlow 的 3D 权重（`src/router/router.rs:290-294`）正是 quality–latency–cost 线性融合的静态特例：`model.weight`≈质量项、`queue_factor`≈延迟项、`window_factor`≈预算项。但 (a) 系数 0.4/0.3/0.3 **硬编码**，无法在 quality/cost 优先等工作点间切换；(b) **延迟项只有队列长度代理，实测延迟/错误率（已在 GlobalMetrics 中）没进权重**。
  - 可落地：①把三个系数挪进 config 并挂到 `/admin/config` 热更新，暴露 quality-latency-cost 预设；②`compute_weight` 增加 `avg_duration_ms`（建议改 EWMA 窗口，现实现是全生命期均值 `src/utils/metrics.rs:158-165`）与 per-vendor error_rate 项。RouteBalance 证据表明这一步（"给延迟定价"）是收益主体，而非更复杂的预测器——不必先上学习模型。
  - 参考数据点：单集群 13 实例/28 GPU、442 配置、≈1.5M 请求；对比 BEST-Route 等基线，高载下 2.6~4.1× 优势。

### 2. Astrolabe: Balancing Load in LLM Serving with Randomized Prediction-Guided Scheduling
- **作者**: Wei Da, Evangelia Kalyvianaki
- **年份/venue**: 2025（arXiv:2508.03611，v3 2026-08 更新）；脚本 venue 输出为 arXiv，下载全文显示已被 **SYSTOR '26** 接收（ACM, doi:10.1145/3793230.3837768）
- **核心思想（机制级）**:
  1. **动机**：LLM 负载不可预测（变长响应、PagedAttention 动态内存、decode 延迟高方差），常用 runtime 指标（延迟/吞吐/内存）不能代表单个请求的 E2E 负载；round-robin 等启发式无保证，Llumnix 式迁移重平衡要搬 KV cache，高载下网络/编排开销会反噬（对比 Llumnix 吞吐最高 2.6×）。生产多实例框架实际仍在用 round-robin。
  2. **三件套组合**：① 轻量回归模型从 prompt **预测响应长度**；② per-instance **Predictor sidecar** 把请求在本地运行时状态上做模拟式延迟预测，返回期望延迟；③ **power-of-two-choices (Po2) 分发**：每请求随机采样 2 个实例、并行查询、路由到预测延迟更低者。one-shot 分发、不做迁移。
  3. **Po2 的三重收益**：每请求 fanout 成本从 O(N) 降为常数（预测器 CPU ≈2.8×↓）；**随机化探测集打破 herding**（确定性 fanout 下短时突发请求算出相同"最优"实例、瞬时压垮同一台）；经典 Po2C 理论保证。效果：mean TTFT −8~36%、P99 TTFT −16~77%、过载时 preemption ≈6×↓，SLO 容量持平 full-fanout（31.6 vs 31.7 QPS）。
- **对 NexusFlow 的适用性: 高（低改动、直接替换选择内核）**
  - NexusFlow 的 `weighted_select`（`src/router/router.rs:222-277`）是**单次轮盘赌**：权重悬殊时退化为近似确定性路由，正是 Astrolabe 批判的 herding 形态之一；权重全 0 时（`router.rs:239-246`）更是纯均匀随机。
  - 可落地（零训练版 Po2C）：按权重**采样 2 个候选**（而非抽 1 个），用 GlobalMetrics 的 per-vendor EWMA 延迟 + `QueueManager::len()`（`src/vendor/queue.rs:39`）+ `ConnectionPool::available_permits()`（`src/vendor/pool.rs:56`）合成"预测负载分"，路由到较优者。Astrolabe 证明"预测器"不必复杂——其收益与 full-fanout 持平而成本常数级；网关侧用近期观测 EWMA 作为延迟预测的零训练近似即可起步。
  - 注意：其长度预测/模拟器 sidecar 属于引擎内组件，网关不可复制；可复制的是 **Po2 决策模式**与"先预测后分发、避免事后迁移"的思想（与 NexusFlow 现有"失败后重试"形成互补：减少进入重试路径的请求数本身就是收益）。

### 3. Learning to Route and Schedule LLMs from User Retrials via Contextual Queueing Bandits
- **作者**: Seoungbin Bae, Junyoung Son, Dabeen Lee（KAIST / SNU）
- **年份/venue**: 2026，arXiv:2602.02061（v2 2026-06-27）
- **核心思想（机制级）**:
  1. **CQB-MNL 框架**：路由（query→LLM 匹配）+ 调度（积压查询优先级）联合建模为"contextual queueing bandit with multinomial logit feedback"。关键洞察有二：(a) 用户不满意会**重试（retrial）**，重试回流加重 backlog，因此"探索次优模型"有队列稳定性代价；(b) 显式反馈（评分）实践中稀疏且伤体验，应改用**隐式反馈**——用户重试/放弃行为。
  2. **ACQB 算法**：Thompson sampling（MNL）+ 递减速率的强制均匀探索；anytime（无需预知时间域/流量松弛参数）。理论保证：routing cumulative regret Õ(√t)，queue length regret Õ(t^(−1/4))。
  3. 实验：SPROUT / EmbedLLM / RouterBench 离线路由数据集 + WildChat-1M 真实对话日志重放（从重复/高度相似查询中提取 retrial）。
- **对 NexusFlow 的适用性: 中（可取其"隐式反馈在线学习权重"骨架，不必照搬理论）**
  - 网关的"用户"是程序，retrial 在网关语境 = 客户端重发相同请求 + NexusFlow 自身按状态码触发的重试（429/500）。这些信号 + `VendorMetrics.successes/failures`（`src/utils/metrics.rs:129-165`）+ `record_error`（`src/utils/metrics.rs:61`）足够构成 reward 流：**vendor 权重不必是静态 config 值 + 手动 `/admin/config` 热更新，可以由 bandit 在线自学习**（成功且延迟达标=正奖励；触发重试/熔断=负奖励）。
  - 完整 MNL + embedding + Thompson 的实现偏重，且其队列模型假设查询积压在网关单队列（NexusFlow 是 per-vendor 队列 + 立即转发）。**低配落地**：EWMA reward + ε-greedy 或对数正态 Thompson sampling，arm = vendor，与现有 `model.weight` 相乘作为先验（人工权重变先验、在线信号做修正）。
  - 最有价值的 takeaway：**探索有拥塞代价，探索率应随时间衰减**——网关做在线权重学习时不能匀速乱试，否则 429 会自我放大。

## 二、其余检索结果速览（相关性分组）

### 相关性中高（网关可参考思想）

| 论文 | 年份/venue | 核心思想 | 对 NexusFlow 适用性 |
|---|---|---|---|
| A Predictive and Synergistic Two-Layer Scheduling Framework for LLM Serving (arXiv:2509.23384) | 2025, arXiv | 集群 router + 引擎内调度两层各管一段，指出两层割裂造成系统性低效，做预测式协同 | 与 RouteBalance 同主题互证：网关（上层）应感知下游容量/负载；佐证优化 1、2 |
| Efficient Training-Free Online Routing for High-Volume Multi-LLM Serving (DOI:10.52202/085713-4577) | 2025, Advances in Neural Information Processing Systems 38 | training-free 的在线多 LLM 路由，面向高吞吐场景（**仅拿到 venue+DOI，标题精确搜索未取到摘要**，未展开） | 方向对口（网关零训练路由），待有摘要后再评估具体机制 |
| DualMap: Enabling Both Cache Affinity and Load Balancing for Distributed LLM Serving (arXiv:2602.06502) | 2026, arXiv | cache 亲和（同前缀请求共置以复用 KV cache）与负载均衡天然冲突，给出全局调度同时兼顾两者 | 思想可迁移：上游供应商有 prompt cache，网关对同一会话做 **sticky/session-affinity 路由**可提升上游缓存命中（省钱+降 TTFT）；但 KV cache 管理本身在引擎内，网关只做亲和分发 |
| Preble: Efficient Distributed Prompt Scheduling for LLM Serving (arXiv:2407.00023) | 2024, arXiv | 前置 prompt 大量共享（系统提示词/长上下文），以 prompt 共享树做全局分发与放置决策 | 同上：识别共享前缀→亲和路由；对网关是可选增强 |
| SMetric: Rethink LLM Scheduling for Serving Agents with Balanced Session-centric Scheduling (arXiv:2607.08565) | 2026, arXiv | agentic 负载（agent 发请求、token 消耗远超人类）下，session 级而非请求级的均衡调度 | NexusFlow 的客户端多为 agent 应用：按 session（而非单请求）均衡 + 会话亲和是契合趋势的设计输入 |
| Learning to Route...（见重点 3） | — | — | — |
| End-to-End Latency-Minimizing and Load-Balanced Request Scheduling for Edge LLM Inference in Agentic AI Services (arXiv:2609.17193) | 2026, arXiv | 异构边缘节点上联合最小化 E2E 延迟与均衡负载的调度，考虑推理状态（KV）演化 | 异构供应商≈异构边缘节点，联合优化目标同构；机制为优化/仿真类，落地参考价值中等 |

### 相关性低-中（思想有限可借鉴）

| 论文 | 年份/venue | 核心思想 | 对 NexusFlow 适用性 |
|---|---|---|---|
| Slice-Level Scheduling for High Throughput and Load Balanced LLM Serving (arXiv:2406.13511) | 2024, arXiv | 生成长度不可预测导致负载估计难，把请求切 slice 级调度以平衡吞吐与负载 | 引擎内机制，网关不适用；其"长度不可预测→先验估计"动机与 Astrolabe 一致 |
| CascadeInfer: Length-Aware Scheduling of LLM Serving... (arXiv:2512.19179) | 2025, arXiv | attention backend 对 batch 内请求长度异质性敏感，做 length-aware 级联调度 | 引擎内 batch 组装，网关不适用 |
| FREESH: Fair, Resource- and Energy-Efficient Scheduling... on Heterogeneous GPUs (arXiv:2511.00807) | 2025, arXiv | 异构 GPU 集群上公平性+能效联合调度 | NexusFlow 若引入"成本档位/能耗敏感"目标可参考其公平性建模 |
| DiSCo: Device-Server Collaborative LLM-Based Text Streaming Services (arXiv:2502.11417) | 2025, arXiv | TTFT/TBT QoE 约束下的设备-服务器协同流式服务 | QoE 指标（TTFT/TBT）可作为网关 SLO 度量的参考词汇 |
| Multi-Bin Batching for Increasing LLM Inference Throughput (arXiv:2412.04504) | 2024, arXiv | 多 bin batching 提升吞吐 | 网关不控制 batching，不适用 |
| CALVO: Improve Serving Efficiency for LLM Inferences with Intense Network Demands (arXiv:2603.21257) | 2026, arXiv | 分布式 prefix cache 远程取 KV 成为瓶颈时的调度 | 网关不管理 KV cache，不适用 |
| AQUA: Network-Accelerated Memory Offloading for LLMs in Scale-Up GPU Domains (arXiv:2407.21255) | 2024, arXiv | 网络加速的 GPU 内存 offload | 硬件层，不适用 |
| LeMix: Unified Scheduling for LLM Training and Inference on Multi-GPU Systems (arXiv:2507.21276) | 2025, arXiv | 训练+推理混部调度 | NexusFlow 无训练负载，不适用 |
| HeatCache: Thermal-aware Energy-efficient LLM Inference Scheduling... (arXiv:2609.12449) | 2026, arXiv | 液冷机柜温度感知调度 | 不适用 |

### Prefill-Decode 分离方向（第 2 轮检索，整体对网关适用性低）

P/D 分离是**引擎/集群内部**的 GPU 池划分技术（prefill 计算 bound、decode 内存 bound 分池），NexusFlow 作为转发网关既不控制 GPU 池也不经手 KV 传输，8 篇均无可直接落地的机制，如实列出并说明：

| 论文 | 年份/venue | 一句话核心 | 适用性 |
|---|---|---|---|
| When Does Disaggregation Pay? Simulating Prefill–Decode–Attention–FFN Specialization for Agentic LLM Inference (arXiv:2608.03741) | 2026, arXiv | agentic 多轮工具调用负载改变 prefill/decode 阶段占比，模拟何时 P/D 分离才划算 | 低；提示 agentic 负载特征（多轮、工具调用）与人类负载不同，对网关容量规划有概念参考 |
| SLO-Aware Compute Resource Allocation for Prefill-Decode Disaggregated LLM Inference (arXiv:2603.04716) | 2026, arXiv | P/D 资源配比的 SLO 约束优化 | 低（部署侧） |
| Trinity: Disaggregating Vector Search from Prefill-Decode Disaggregation (arXiv:2512.02281) | 2025, arXiv | 把向量检索从 P/D 分离架构中再分离 | 低 |
| SPAD: Specialized Prefill and Decode Hardware (arXiv:2510.08544) | 2025, arXiv | P/D 专用硬件 | 不适用 |
| PDTrim: Targeted Pruning for Prefill-Decode Disaggregation (arXiv:2509.04467) | 2025, arXiv | 面向 P/D 的模型剪枝 | 不适用 |
| Disaggregated Quantization: Specializing LLM Prefill and Decode (arXiv:2609.26333) | 2026, arXiv | prefill/decode 各自最优的量化格式 | 不适用 |
| Analytical Power-Aware Provisioning for Prefill-Decode Disaggregated AI Inference (arXiv:2609.24639) | 2026, arXiv | 功耗约束下的 P/D 配给解析法 | 不适用 |
| PDD: ...Cross-Datacenter Prefill-Decode Disaggregation (arXiv:2609.13161) | 2026, arXiv | 跨数据中心 P/D 分离 | 不适用 |

### multi 源轮噪声（如实记录）

`--source multi` 返回 8 条中 6 条与主题无关（地质空间问答/健康补充剂/牙科影像/物质使用障碍聊天bot/VYUHA 布线算法/两篇泛综述），OpenAlex+Crossref+EuropePMC 混入大量 biomedical 结果；另 DBLP 源报错（"DBLP 返回了无法解析的内容"）自动跳过。**CS/系统方向确以 arxiv 源相关性最好**，与任务预设一致。

## 三、检索局限（诚实记录）

1. 按标题搜索 "Efficient Training-Free Online Routing..."（NeurIPS 38）未命中：Semantic Scholar 429 限流自动降级（任务已预告属正常），OpenAlex/Crossref 无该条；该篇仅有 venue+DOI 元数据，未取得摘要与机制，**不纳入重点分析**。
2. 三篇重点论文下载时 Pandoc 不可用（缺 `pypandoc`），降级为 PDF 文本提取：公式可能退化（本调研仅用其文字机制描述，不受影响），章节标题结构丢失（靠通读正文定位）。
3. 全部 venue 以脚本输出为准：除 Astrolabe（下载全文内可见 SYSTOR '26 收录信息）外，其余脚本 venue 均为 arXiv；各篇的正式发表信息未逐一在多源中交叉核验。
4. 未做：重点论文的代码仓库复现验证（RouteBalance 给了 github.com/AKafakA/route-balance 但未运行）、收益数字在本项目负载下的实测。

## 四、可落地优化清单（按性价比排序）

1. **Po2C 择优分发**（basedOn: Astrolabe）— `weighted_select` 改为"按权重采样 2 候选 → 按 per-vendor 近期负载分择优"。effort: low。
2. **延迟/错误率信号入权重 + 系数可配置**（basedOn: RouteBalance）— `compute_weight` 增加 EWMA 延迟与 error_rate 项；0.4/0.3/0.3 移入 config 热更新并暴露 quality/latency/cost 预设。effort: low~medium。
3. **Bandit 在线权重自学习**（basedOn: ACQB）— vendor 权重 = 人工配置先验 × 在线 reward 修正（成功/延迟达标/重试率），探索率随时间衰减。effort: medium。
4. **会话/前缀亲和路由**（basedOn: DualMap + Preble + SMetric）— 同会话 sticky 到同一 vendor 以命中上游 prompt cache；agentic 客户端收益最大。effort: medium。
5. **dead-reckoned per-vendor 状态**（basedOn: RouteBalance）— per-vendor EWMA 延迟 + in-flight 计数本地推算（事件驱动更新而非快照轮询），供 1/2/3 共用作负载信号源；注意现 `VendorMetrics.avg_duration_ms` 是全生命期均值（`src/utils/metrics.rs:158-165`），需改滑动窗口。effort: low~medium。
