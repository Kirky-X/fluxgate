# LLM 服务调度与负载均衡 — 第二轮论文调研笔记（算法专项）

- **日期**: 2026-10-01
- **工具**: liuxiang `search_papers.py`（4 轮检索，arxiv 源）+ arXiv HTML 全文精读 4 篇（`/tmp/research2_papers/`，MathML alttext 提取 LaTeX 公式）
- **与第一轮的边界**（不重复报告）: 第一轮 `out/research/papers-serving.md` 已覆盖 RouteBalance、Astrolabe(Po2C)、ACQB(CQB-MNL)、DualMap、Preble、SMetric 及 P/D 分离 8 篇（结论：引擎内，网关不适用）；`papers-routing.md` 已覆盖 RouteLLM、RouterBench、When Routing Collapses。本轮检索中再次命中的 RouteBalance、AQUA 均直接排除。本轮聚焦第一轮未覆盖的三个角度：**SLO 感知调度、准入控制、排队论/长度预测**。
- **建议查询中「prefill decode disaggregation scheduling」未执行**：第一轮已跑过近同查询（"prefill decode disaggregation inference"）并明确结论（8 篇均不适用、网关不控制 GPU 池与 KV 传输），本轮基线未变，重跑无新信息量；用「output length prediction」替换（准入控制的关键输入，第一轮未专门搜过）。
- **基线代码核对点**（本轮 grep 验证）：路由权重 `calculate_weight`（`src/router/router.rs:530`，5 因子：model/queue/window/EWMA延迟/EWMA错误，系数热更）+ Po2C 开关（`router.rs:424` `weighted_select_with`）+ 温度平滑；限流 `src/vendor/rate_limiter.rs`（`TokenBucket`:18、`ConcurrencyLimiter` 并发票据:40、`check`/`consume_tokens`/`window_snapshot`:152-188）；会话亲和 `src/router/affinity.rs`。

## 实际执行的检索命令

```bash
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "LLM serving SLO aware scheduling" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "request admission control inference" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "load balancing LLM instances queueing" --source arxiv --limit 8
python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "output length prediction LLM request scheduling" --source arxiv --limit 8
```

第 3 次检索仅返回 3 条（其余查询命中少），如实记录。第 1/2 次各 8 条完整。

## 一、重点论文详析（4 篇，全文精读）

### 1. Beyond Accuracy and Cost: Latency-Aware LLM Query Routing for Dynamic Workloads ⭐ 最直接对标
- **作者/机构**: Shivam Patel*, Akaash R. Parthasarathy* (CMU), Ankur Mallick (Microsoft), Gauri Joshi (CMU)
- **年份/venue**: 2026-05，arXiv:2607.18253v1（cs.AI）
- **解决的问题**: 现有 query router（RouteLLM/HybridLLM/Universal Routing 一类）只优化质量-成本、延迟无感知；系统层 LB（round-robin/JSQ）只优化队列、质量成本无感知。本文做三目标联合路由。
- **机制（可实现级）**:
  1. **质量-成本效用**：`Û_ij(λ) = âcc_ij − λ·cost_ij`（λ≥0 为质量-成本权衡系数）。
  2. **路由决策两种形式**：
     - **硬约束版（式 3）**: `m(i) = argmax_{j: L̂ᵗᶠᵗ_ij(t) ≤ τᵢ} Û_ij(λ)`；可行集为空（所有实例都超 SLO）→ 选估计延迟最小的实例（**而不是违约硬冲**）。
     - **拉格朗日软约束版（式 5）**: `m(i) = argmax_j ( Û_ij(λ) − δ·L̂ᵗᶠᵗ_ij(t) )`，δ≥0 扫描即得 utility–latency 前沿；δ=0 退化为延迟无感知路由。
  3. **SFS 延迟估计器（引擎状态可得时）**: 输入候选实例 j 的驻留集 `R_j(t) = 排队集 ∪ 活跃集`，每请求剩余 prefill token `tok^pre` 与估计 decode token `t̂ok^dec`；按框架调度策略逐 token-batch 确定性模拟，直到新请求产出首 decode token：`L̂ = Σ_{ℓ=1..ℓᵀᵀᶠᵀ} T̂^(ℓ)`。单个 token-batch 处理时间用 4 参数线性模型（式 9）：`T̂ = β₀ + β₁·Σtok + β₂·Σ(c_q·tok^dec) + β₃·Σ(tok^pre·c_q + tok^pre(tok^pre+1)/2)`（β₀ 固定开销 / β₁ 稠密层 / β₂ decode 注意力随上下文 / β₃ prefill 注意力），按实例离线校准（batch 时间 ~4% MAPE，TTFT 全器 ~5% vs 吞吐量基线的 85%）。估计开销 ~10⁻⁴ s。
  4. **平均情形估计器（§4.2，无实例内实时负载——网关场景）**: 用 Limited Processor Sharing (LPS) 队列抽象（最多 k 请求并行、其余 FCFS 排队），`Ŵ^avg = (α/μ)^k / (μ−α)`（式 10，α=到达率，μ=服务容量=峰值输出吞吐），`L̂^avg = Ŵ^avg + tok^pre/θ^pre + T̄^dec`（式 11）。附录 E 证明推导 + 与实测吻合（观测平均并行度 k≈25）。
- **效果**: OnTimeUtility 的 AUC +33%（vs 最佳基线）；δ 扫描下同延迟效用 +40%（vs Shortest Queue）；Poisson 与突发 MMPP-2 到达下均稳健。
- **对 NexusFlow 适用性: 高（improvement，低改动起步）**
  - 现状对照：NexusFlow 的 `latency_factor = ref/(ref+ewma_ms)`（`src/router/router.rs:597-602`）本质是"用近期 EWMA 观测当延迟预测"，对**过载非线性**无感知——负载逼近容量时 EWMA 是滞后指标，等它升高路由已经挤爆。
  - 可落地 ①（低）：用式 (10)(11) 把延迟项升级为**排队论平均估计**：per-vendor 维护 α（到达率，60s/10 桶环形滑窗已有，可直接复用）、μ（由 EWMA 服务时长派生 1/avg_service，或 vendor 配置）、k（上游并发配额，config 已有 per-vendor 并发限制）。`(α/μ)^k/(μ−α)` 在 ρ→1 时发散，天然给"逼近容量"的 vendor 强惩罚；零训练、纯公式、CPU 开销可忽略。SFS 逐 token-batch 模拟需要引擎内部状态（batch 组成/框架策略），**网关对第三方 API 不可得，不适用**——适用且够用的正是 §4.2。
  - 可落地 ②（中）：式 (3) 的硬约束路由是 NexusFlow 没有的形态（per-request SLO 目标 τ）。无客户端 SLO 时先做软约束版（式 5）——δ·L̂ 项可直接并入 `compute_weight` 的线性因子结构，与现有 5 因子形态完全兼容；或把 `latency_factor` 输入从 EWMA 换成 `L̂^avg`。
  - 与第一轮 RouteBalance 结论互证：该文引言再次确认"质量-成本路由延迟无感知会把请求推进过载实例"，而 NexusFlow 第一轮已把 EWMA 延迟/错误率接入权重——本文给出的是**下一步**：EWMA(滞后观测) → 排队论预测(前瞻估计)。
- **证据**: arXiv:2607.18253；全文 `/tmp/research2_papers/2607.18253.txt`（式 1-11、§4.2 LPS 模型、附录 E/F）

### 2. Token Management in Multi-Tenant AI Inference Platforms ⭐ 准入控制直接对标
- **作者/机构**: William J. Cunningham (DataRobot, Boston)
- **年份/venue**: 2026，arXiv:2603.00356v1；正文页眉显示 ACM Conference on AI and Agentic Systems, May 26–29 2026, San Jose（以脚本输出 arXiv + 正文页眉为准）
- **解决的问题**: rate limit 只管请求准入、不感知执行成本异构；dedicated endpoint 闲置浪费；两者都不 work-conserving（空闲容量无法被借用），也无法表达突发维度（prompt 长度突发/输出长度突发/并发会话突发）。
- **机制（可实现级）**:
  1. **三维资源 token pool**: token 吞吐 λ (tok/s)、KV cache χ (bytes)、并发 r (active sequences)；租户 entitlement = 基线配额 (λ_e, χ_e, r_e)，可行性 `Σ λ̂_e ≤ Λ_p`。
  2. **五档服务等级**（保护顺序）: Dedicated(基线保留+可突发+永不收缩, w=1000) > Guaranteed(保留+不可突发, 1000) > Elastic(时间平均保证+可收缩+债务补偿, 100) > Spot(无保证+只吃剩余+最先被压, 1) > Preemptible(可驱逐, 0.1)。
  3. **优先级公式（式 1）**: `w_e = w_κe · (1+α_slo·ℓ*_e/ℓ̄*)⁻¹ · (1+α_burst·b_e)⁻¹ · (1+α_debt·d_e)`，典型系数 α_slo=2.0, α_burst=1.0, α_debt=4.0；等级权重数量级差距(1000/100/1/0.1)保证正常时等级主导。
  4. **债务 EWMA（式 2，PI 控制器类比）**: 服务缺口 `g_e = (λ_e − λ̂_e)/λ_e`（欠服务为正）；`d_e(k) = γ_d·d_e(k−1) + (1−γ_d)·g_e(k)`；γ_d 即抗积分饱和。d_e>0 抬升优先级→公平份额随时间收敛。
  5. **突发强度（式 3）**: `δ_e = Σ_dim max(0, 实际/基线 − 1)`（吞吐+KV+并发三维求和），EWMA 平滑；持续突发渐进压低优先级，防独占。
  6. **准入控制（auth 层有序短路 5 检查）**: ① entitlement 状态合法 → ② max_tokens 缺省值补全（容量规划）→ ③ in-flight < r_e → ④ token 预算(input+max_tokens) 装得下剩余吞吐配额 → ⑤ **pool 争用时：w_e < 准入阈值（当前已准入请求的最小优先级）→ 429 + Retry-After**。请求完成后 gateway 回调实测 token 消耗与延迟，更新 b_e/d_e——准入(执行前)与成本核算(执行后)闭环。
- **效果**: 38% 过载实验中，guaranteed 租户 P99 TTFT 保持 <1.2s，无准入基线恶化到 19s+；spot 被选择性地 429（带 Retry-After），有保障流量零感；capacity 空出后 spot 立即回填（work-conserving）。
- **对 NexusFlow 适用性: 高（missing，中等改动）**
  - 现状对照：NexusFlow 租户限流 = Token/Interval 固定窗口（`check`/`consume_tokens`，`src/vendor/rate_limiter.rs:152/179`）+ 并发票据（`ConcurrencyLimiter`:40）+ GCRA 入站。已覆盖其检查③④与 429+Retry-After；**缺的是**：
    1. **租户优先级/服务等级**——NexusFlow 所有 API key 无差别，拥塞时无差别劣化；
    2. **拥塞感知的选择性准入**——现有 GCRA 是"超自身配额即拒"，不感知**系统**拥塞；式 (1) 阈值机制让"谁被 429"由优先级决定而非先到先得；
    3. debt/burst EWMA（可后置，先上 1+2 已拿到主要收益：过载下保护高优租户的 P99）。
  - 单二进制适配：Redis → 内存表（`DynamicField`/DashMap 同族）；K8s/virtual node/autoscaling 部分**不需要**（NexusFlow 不控制后端扩缩容），只取"auth 层 5 检查 + 优先级公式 + 阈值准入"子集；per-tenant 服务等级与三系数进 config 热更。作者自述"admission control 在 API 边界而非容器/GPU 层"正是网关的天然职责。
  - 与第一轮 resilience 的 DRL 限流论文相比：该机制可解释、公式化、无训练，落地代价低一个量级。
- **证据**: arXiv:2603.00356；全文 `/tmp/research2_papers/2603.00356.txt`（式 1-3、§3.3、§4.3、§5.2 实验）

### 3. Scorpio: Serving Right Requests at the Right Time for Heterogeneous SLOs in LLM Inference
- **作者**: Yinghao Tang, Tingfeng Lan, Bo Pan, Xiuqi Huang, Hui Lu, Wei Chen
- **年份/venue**: 2025-05（arXiv:2505.23022v2）；arXiv abs 页 Comments 字段（本轮 curl 核实）：**Accepted at WISE 2026**（27th Int'l Conf. on Web Information Systems Engineering, Special Track on Innovative Web Technologies, Springer LNCS）
- **解决的问题**: 吞吐优先系统把所有请求同质对待 → SLO 达标率次优；核心洞察是"SLO 异构性不是负担而是资源"——松 SLO 请求的 slack 可以转换成紧 SLO 请求的 headroom。
- **机制（可实现级）**:
  - **goodput 口径**: 拒绝的请求计入分母且不算达标——拒绝不再掩盖违约（`goodput = |R^good|/|R|`，|R| 含被拒请求）。
  - **TTFT Guard**（网关可落地部分）:
    1. **LDF 重排**（Least Deadline First）: deadline = 剩余 TTFT 预算 `S_TT(r_i) − 已等待时间`，最紧急者排前；
    2. **不可达即拒绝**: 预估等待+执行将违约的请求直接拒绝（429），而不是排队后违约；
    3. **天然防饥饿**: 时间推进 → deadline 逼近 → 自动前移；必然违约者被提前清出。
  - **TPOT Guard**（引擎内为主，机制记录如下）:
    1. **TRP**: `TRP(r) = min_{r'∈R(t)} S_TP(r') / S_TP(r)`（相对当前最紧 SLO 的紧迫度，<1 为松请求，随负载自适应）；
    2. **Credit-based Batching**: `C_r += TRP(r)`；`C_r ≥ 1.0` 进 batch、进后 `C_r −= 1.0` → 长期按 TRP 速率获得执行机会；
    3. **VBS 准入**: `VBS(R') = Σ TRP(r)`（松请求按占用比例折算成"虚拟部分请求"），准入判据 `EstimatedTPOT(VBS(R'), L_avg) ≤ min S_TP(r')`，否则拒。
  - **预测模块**: 长度预测器 + 两个解析模型支撑两 guard 的可达性估计。
- **效果**: 高载（QPS 15）goodput +8.8~14.4×、SLO 达标 +40.7~46.5%（vs vLLM/S3/Mooncake）；Azure 生产 trace 累计 SLO-met 1.25~2.11×；调度开销 <1%。消融：TTFT Guard 与 TPOT Guard 单独用都会让另一维违约，需成对。
- **对 NexusFlow 适用性: 中（improvement）**
  - Credit-based batching/VBS 是 decode iteration 层概念，**网关不适用**（如实标注）。
  - **TTFT Guard 可落地**: NexusFlow per-vendor 内部队列现为 FIFO；若引入可选 per-request deadline（请求头如 `x-deadline-ms`，或租户级默认），队列改 LDF + 预估不可达提前 429。与第 2 篇的优先级准入天然组合（deadline 紧急度可作为一个优先级因子）。前提是客户端配合传 SLO——NexusFlow 客户端多为 agent 程序，加 header 成本低，也可从租户配置兜底。
  - goodput 口径值得采纳进 metrics：把"被熔断/拒绝"的请求与"违约"请求分开计数，避免高拒绝率伪装成高达标率。
- **证据**: arXiv:2505.23022；全文 `/tmp/research2_papers/2505.23022.txt`（§3.3-3.4、TRP/Credit/VBS 公式、Algorithm 1）

### 4. Optimizing LLM Inference: Fluid-Guided Online Scheduling with Memory Constraints（理论基座）
- **作者**: Ruicheng Ao, Gan Luo, **David Simchi-Levi**, Xinshang Wang, Zijie Zhou（Stanford/清华 OR 团队）
- **年份/venue**: 2025-04（arXiv:2504.11320v4，2025 更新）
- **解决的问题**: KV cache **内生增长**（每 decode 一步涨一个单位）下的在线调度；超容量触发 LIFO 驱逐→从头重启→**级联驱逐**（驱逐释放的内存被新请求填满→再次溢出→恶性循环）。
- **机制**:
  1. **迭代时间线性模型**: `τ = d₀ + d₁·Σ_{i∈B}(l_{j(i)}+s_i^t)`（固定开销 + 单位 cache token 边际成本）；A100/Llama-2-7B 实测线性拟合良好。 Admission trade-off：大批次摊薄 d₀ 提吞吐，但每请求的 KV 随年龄增长，过度准入 = 未来迭代时间变长 + 内存压力。
  2. **fluid 均衡与稳定域**: 到达/完成/内存增长速率匹配的驻留组成即均衡；内存需求 M* ≤ C 定义稳定域。关键解析结论：**M* 随输出长度近似二次增长**（远离临界分母时 ~ (l')²）——长输出是容量杀手，且分母趋零时发散。
  3. **WAIT（已知输出长度）**: 按 prefill/decode 阶段设阈值队列，请求仅在对应阈值有空间时才推进/准入 → 把驻留组成钉在均衡组成附近，消除驱逐级联。
  4. **Nested WAIT（未知输出长度）**: decode 段嵌套阈值（短请求早完成早释放；存活者按段推进），段边界在线分类输出长度，无需预知长度；加 safety buffer 后有吞吐/延迟/免驱逐保证。
- **效果**: Vidur 仿真（Llama-2-7B/A100）+ 真 GPU 附录验证：扩大经验稳定运行范围；近过载与过载区延迟显著下降（欠载区与基线接近）。
- **对 NexusFlow 适用性: 中低（不单独立项，作为理论佐证）**
  - GPU/KV/迭代层机制**网关不可复制**（如实标注）。可迁移的两点：
    1. **"FCFS 在近容量处触发级联失效、受控准入扩大稳定域"**——为第 2 篇的拥塞阈值准入提供理论支撑：NexusFlow 的并发票据上限本质上就是一个"阈值准入"载体（票据耗尽即排队/拒绝而非无限积压），论点 = 保持票据上限紧贴真实容量比"放行再靠重试救"更优。
    2. **负载的非线性容量代价**（M* ~ (l')²）→ agentic 长输出流量占比高时，容量评估不能按请求数线性外推；token 加权的并发/配额（见下条 CONCUR）是更合理的度量。
- **证据**: arXiv:2504.11320；全文 `/tmp/research2_papers/2504.11320.txt`（§2 模型、式 (1)(2)、§3.1 式 (5) M* 结论、§4-5 WAIT/Nested WAIT）

## 二、其余检索结果速览（标题+摘要级评估）

### 有增量思想（摘要级，未全文展开）

| 论文 | 年份/标识 | 核心思想 | 适用性 |
|---|---|---|---|
| CONCUR: High-Throughput Agentic Batch Inference via Congestion-Based Concurrency Control | 2026, arXiv:2601.22705 | agentic 负载对 KV cache 持续累积施压，在内存耗尽**之前**就出现 middle-phase thrashing 吞吐崩塌；以拥塞信号（而非容量上限）控制并发 | 思想对网关有效：NexusFlow 客户端多为 agent（多轮长上下文），并发票据若按请求数计数会低估 agentic 请求真实负载——**票据配额应按 token（或预估输出长度）加权**；拥塞信号（EWMA 延迟拐点）可作准入闸门 |
| JITServe: SLO-aware LLM Serving with Imprecise Request Information | 2025, arXiv:2504.20068 | 请求信息（长度/资源需求）不精确时仍做 SLO 感知调度 | 思想与网关同构：NexusFlow 对请求也只有不精确信息（无输出长度）。其"按不确定性降级决策"的思路可借鉴到长度预测的使用方式（预测仅作软信号而非硬判据） |
| PackServe: SLO-Aware Request Scheduling for Agentic LLM Serving at Scale | 2026, arXiv:2609.33224 | 集群级：跨长共享前缀保 KV 复用 + token 级延迟 SLO + 高吞吐的联合调度 | 集群内机制网关不可复制；但再次印证 agentic 负载（共享前缀/长会话）是调度一等公民，与会话亲和（已实施）方向互证 |
| Scheduling LLM Inference with Uncertainty-Aware Output Length Predictions | 2026, arXiv:2604.00499 | 单点长度预测不可靠，改用不确定性感知的分布/区间预测驱动 SJF | 若将来做长度预测（第一轮暂缓项），预测输出应是**区间/分位数**而非点值，用于 SJF 排序与准入分档 |
| Efficient LLM Scheduling by Learning to Rank | 2024, arXiv:2408.15792 | 预测长度的绝对值不稳，改学**相对排名**做 SJF，HOL blocking 显著缓解 | 同上：长度预测的最稳形态是 rank（Kendall's Tau 路径），训练目标简单、对误差更鲁棒 |
| OUTLETS: Output-Length Prediction from Speculative Decoding Backbones | 2026, arXiv:2609.01068 | 免外部代理模型：用投机解码骨干内部信号预测输出长度 | 网关不运行模型 → 不适用；但"免额外模型拿到长度信号"思路与 vendor 侧（如上游返回 usage 流）结合或有取巧空间 |

### 相关性低（引擎内/部署层/其他硬件，如实记录）

| 论文 | 标识 | 一句话 | 不适用原因 |
|---|---|---|---|
| Taming Request Imbalance: SLO-Aware Scheduling for Disaggregated LLM Inference | arXiv:2605.02329 | P/D 分离下长尾请求长度造成 prefill HOL blocking | 引擎内 |
| Aladdin: Joint Placement and Scaling for SLO-Aware LLM Serving | arXiv:2405.06856 | 集群级 placement+autoscaling 联合 | 部署层，NexusFlow 不管 GPU |
| SuperInfer: SLO-Aware Rotary Scheduling…on Superchips | arXiv:2601.20309 | 超级芯片上 KV 预算耗尽导致 HOL 的调度+内存管理 | 硬件引擎内 |
| AdaServe: SLO-Customized Speculative Decoding | arXiv:2501.12162 | 按每请求 SLO 定制投机解码步长 | 引擎内 |
| Energy-Aware Scheduling for Serverless LLM Serving on Shared GPUs | arXiv:2606.30391 | 共享 GPU serverless 的能耗感知调度 | 能耗目标不在当前范围 |
| Stage-Aware Communication Scheduling for Disaggregated LLM Serving | arXiv:2603.17456 | P/D 分离多阶段通信调度 | 引擎/集群内 |
| Execution Envelopes: A Shared Admission Contract for Backend AI Execution Requests | arXiv:2605.08267 | 跨服务统一准入契约/标准化格式 | 标准化文档，无调度算法机制 |
| BlockServe / AQUA / DeepRT | arXiv:2607.08930 / 2407.21255 / 2105.01803 | diffusion LLM batching / GPU 内存 offload / CV 边缘软实时 | 领域或层级不符（AQUA 第一轮已判不适用） |
| LAPS: Length-Aware-Prefill LLM Serving System | arXiv:2601.11589 | 按 prompt 长度分离请求降 TTFT | 引擎内 |
| Semi-Clairvoyant Scheduling of Speculative Decoding Requests | arXiv:2505.17074 | 投机解码请求的部分预知调度 | 引擎内 |
| SageSched: …Demand Uncertainty and Hybridity | arXiv:2603.07917 | 需求不确定+计算/内存混合性的调度 | 引擎内为主；其"不确定性"建模与 JITServe 同类 |
| Adaptively Robust LLM Inference Optimization under Prediction Uncertainty | arXiv:2508.14544 | 预测不确定性下的鲁棒优化调度 | 引擎内；鲁棒思想同 JITServe |
| Don't Stop Me Now: Embedding Based Scheduling for LLMs | arXiv:2410.01035 (ICLR'25) | 嵌入预测剩余 decode token 驱动 SRPT | 引擎内排序；网关若做长度预测可复用其"嵌入→长度"特征思路 |
| RouteBalance | arXiv:2606.17949 | （第一轮重点 #1，已实施其延迟定价建议） | 重复，排除 |

## 三、检索局限（诚实记录）

1. 第 3 次检索（"load balancing LLM instances queueing"）**仅返回 3 条**（RouteBalance/LAPS/Latency-Aware Routing），远少于 limit=8，如实记录；该角度的深度信息主要由 RouteBalance（第一轮）与本文重点 #1 补足。
2. 全文公式提取基于 arXiv HTML 的 MathML alttext（LaTeX 源码），本轮 4 篇关键公式（式 1-11、式 1-3、TRP/Credit/VBS、M* 结论）均完整可读；个别行内片段符号若有退化以原文 PDF 为准。
3. venue 以脚本输出为准；仅补充核实了两处：Scorpio 经 arXiv abs 页 Comments 字段确认 WISE 2026 接收（curl 本轮执行）；Token Management 的会议名取自其 HTML 正文页眉（ACM Conference on AI and Agentic Systems 2026），未见独立佐证。
4. 未做：4 篇重点论文的代码仓库复现（Latency-Aware Routing 未提供明确 repo 链接于正文所见部分）；收益数字在 NexusFlow 负载下的实测；`λ`/`δ`/`α_*` 等系数对本项目流量的敏感性分析。
5. 摘要级论文（第二节）未下载全文，机制提炼止于标题+摘要，深度低于第一节。

## 四、可落地优化清单（按性价比排序）

1. **排队论延迟预测替换/增强 EWMA 延迟因子**（basedOn: Latency-Aware Routing §4.2 式 10-11）— per-vendor `Ŵ=(α/μ)^k/(μ−α)`，α 复用现有环形滑窗、k 用 vendor 并发配置、μ 从 EWMA 服务时长派生；进 `compute_weight` 或替换 `latency_factor` 输入。过载逼近时惩罚发散，弥补 EWMA 滞后。effort: low~medium。
2. **拥塞感知的优先级准入（429 谁说了算）**（basedOn: Token Management 式 1 + 准入检查⑤）— per-tenant 服务等级 + 优先级公式（SLO/突发/债务三因子，系数进 config）；系统拥塞（并发票据占用率/EWMA 延迟拐点超阈值）时，低于"当前在途最小优先级"的新请求 429+Retry-After，高优租户零感。现有并发票据/Token 窗口即检查③④，改造成本集中在优先级表 + 拥塞信号判定。effort: medium。
3. **SLO 软约束路由项**（basedOn: Latency-Aware Routing 式 5）— `−δ·L̂` 项并入现有线性权重（与热更系数机制无缝）；客户端可选 `x-deadline-ms` 头 + 租户默认 deadline，可行集为空时改选"估计最快"vendor 而非硬冲（式 3 的空集规则）。effort: medium。
4. **vendor 队列 LDF 重排 + 不可达提前拒绝**（basedOn: Scorpio TTFT Guard）— 依赖 3 的 deadline 信号；per-vendor 等待队列 FIFO→LDF，预估违约即提前 429（防排队后违约，且天然防饥饿）。goodput 口径进 metrics（拒绝/违约分开计数）。effort: medium。
5. **agentic 负载的 token 加权并发**（basedOn: CONCUR + Fluid-Guided M*~(l')² 结论）— 并发票据按预估 token 量加权占用（max_tokens 缺省值兜底），而非每请求一票；防止长会话请求挤占导致的隐性超载。effort: medium。
6. **长度预测启动时的形态选择**（basedOn: Learning to Rank + Uncertainty-Aware Predictions）— 若启动第一轮暂缓的长度预测：先做**相对排名**（稳、训练目标简单），输出**分位数区间**（准入分档用下分位保守值）；而非点估计。effort: high（依赖暂缓项启动，暂不排期）。

> 未列入但已评估：Fluid-Guided 的 WAIT/Nested WAIT（引擎内，思想已并入 #2 的理论依据）、Scorpio 的 Credit/VBS（引擎内）、SFS 逐 batch 模拟（需引擎状态）。
