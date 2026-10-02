# LLM 缓存算法（语义缓存 / KV 复用 / 淘汰）—— 第二轮论文调研笔记

- **日期**: 2026-10-01
- **工具**: `python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py`（arxiv 源）+ `download_paper.py`（全文转 Markdown）
- **目标**: 为 NexusFlow 寻找「缓存算法」方向可照着实现的算法点
- **全文来源**: 4 篇已下载并深读，存于 `/tmp/papers-caching2/`（`vcache.md` / `category-aware.md` / `semantic-buffers.md` / `cacheus.md`）
- **与第一轮的边界**: 第一轮 `out/research/papers-gateway.md` 已深读 LaCache（arXiv:2608.01718，投毒防御/lookahead 校验）、Closing the Operational Gap in Semantic Caching（arXiv:2606.19719，P-CHR/ORR 选型指标）、MeanCache（arXiv:2403.02694，context chain 防多轮误命中）。**本轮不重复这三篇的机制**，专注第一轮未覆盖的三个算法维度：① 命中判定的**在线自适应阈值**；② 异构负载下的**差异化策略 + 负载联动**；③ **淘汰/准入策略**（第一轮只在速览表提过 arXiv:2604.20021 一行，未展开）。

---

## 一、检索记录（实际执行的命令与结果）

| # | 实际命令 | 结果 |
|---|---------|------|
| 1 | `search_papers.py "semantic caching LLM similarity threshold" --source arxiv --limit 8` | ✅ 5 篇，3 篇新（vCache / Category-Aware / Async Verified） |
| 2 | `search_papers.py "KV cache reuse eviction policy" --source arxiv --limit 8` | ✅ 8 篇，全部为推理引擎侧 KV 淘汰（GPU/NPU/服务端） |
| 3 | `search_papers.py "prefix cache admission eviction LLM" --source arxiv --limit 8` | ⚠️ 仅 3 篇且均偏推理引擎（hybrid 模型 prefix cache、多租户 GPU KV 准入、pipeline 并行） |
| 4 | `search_papers.py "cache admission policy learning" --source arxiv --limit 8` | ⚠️ 8 篇但噪声大（MoE 专家缓存、硬件预取、测试套件学习等），仅 DEAP Cache 沾边且需离线训练 |
| 5 | `search_papers.py "learning augmented caching online eviction" --source arxiv --limit 8` | ✅ 5 篇，命中学习增强缓存经典线（补第 4 轮缺口） |

共 5 次（超建议的 2~4 次，因第 4 轮噪声过大补跑一轮）。全部为 arxiv 单源，未跑 multi。

---

## 二、深读论文（4 篇，机制提炼到可实现程度）

### 1. vCache: Verified Semantic Prompt Caching

- **年份**: 2025-02-06（脚本输出），脚本检索时为 v5
- **标识**: arXiv:2502.03771v5
- **venue**: 论文页眉标注 "Published as a conference paper at **ICLR 2026**"（`vcache.md:243`）
- **证据**: 全文 `/tmp/papers-caching2/vcache.md`

**问题**：现有语义缓存系统（GPTCache 等）对所有请求用**同一个静态相似度阈值** τ ∈ [0,1]。论文指出静态阈值没有形式化正确性保证——在 embedding 空间稠密的区域产生漏判/误判，且 Figure 3 实验显示**每个缓存条目的最优阈值差异极大**（`vcache.md:347-352`），单一全局阈值不可能同时合适。

**机制（输入特征 → 决策式 → 输出/更新规则）**：

*状态结构*（`vcache.md:266-283`）：每个缓存条目 `x_i` 存 `⟨E(x_i), r(x_i), O(x_i)⟩`，其中元数据 `O(x_i) = {(s(x_j), c(x_j)) : nn(x_j) = x_i, j > i}`——即**该条目历史上所有"落它身上"的后续请求**的（相似度, 命中是否正确）二元组。`s(x) = sim(E(x), E(nn(x)))`；`c(x) = 1` 当且仅当 `r(nn(x)) = r(x)`（回源后真实响应与缓存响应一致）。

*模型*（Eq 9-10，`vcache.md:385-401`）：对每个条目单独拟合 sigmoid
```
Pr(c(x)=1 | x, D) = L(s(x), t, γ) = 1 / (1 + e^{-γ(s(x) - t)})
```
其中 `t ∈ [0,1]` 是**条目专属**决策边界，`γ > 0` 控制陡度。参数由 MLE 给出：解二元交叉熵（Eq 10），输入就是该条目的 `O` 全部样本。sigmoid 形式保证平滑单调，样本少时泛化优于硬阈值。

*决策规则*（Eq 11 + Algorithm 2，`vcache.md:435-455, 505-535`）：因为样本有限，需考虑估计不确定性。取 (1-ε) 置信带上的悲观参数 `t'(ε), γ'(ε)`，算
```
τ̂ = min_{ε∈(0,1)}  [ (1-δ) - (1-ε)L(s(x), t'(ε), γ'(ε)) ] / [ 1 - (1-ε)L(s(x), t'(ε), γ'(ε)) ]   ≥ τ_{nn(x)}(s(x))
```
然后抽 `u ~ Uniform(0,1)`：**u ≤ τ̂ 则 explore（回源 LLM），否则 exploit（返回缓存）**。用户只需给定最大错误率 δ，不需要调 τ。

*更新规则*（Algorithm 1，`vcache.md:325-352`）：explore 时计算 `c = 1(r(x) = r(y))`，把 `(s(x), c(x))` 并入 `O(y)`；若 `¬c(x)` 则把新条目插入数据库 `D`；返回回源的真实响应。即 explore 天然产生标签，**无需任何离线训练**。

*保证*（Theorem 4.1，`vcache.md:521-529`）：在数据 i.i.d. 且 sigmoid 族正确建模的前提下，`Pr(vCache(x) = r(x) | D) ≥ 1-δ` 对任意 x 成立。

*实验*：比静态阈值 GPTCache 命中率高至多 **12.5×**、错误率低至多 **26×**（摘要 `vcache.md:11`）；τ 计算单次更新 ≈1.5ms（`vcache.md:1270-1272`）。

*已知代价*（Limitations，`vcache.md:690-698`）：响应长于几个词时字符串匹配不够，需 **LLM-as-a-judge 判响应等价**（Algorithm 1 L8），多一次 LLM 推理——论文说明可**异步放在关键路径外**，输出单 token（yes/no）故开销小。这是落地时必须照做的一步。

**对 NexusFlow 的适用性**：命中判定层的算法本体（逻辑回归 + ε 置信界 + 均匀随机开关）**纯 CPU、在线、无训练**，单二进制完全放得下（一个 `Beta`/sigmoid + 一个 RNG）。真正的门槛是 **embedding 服务**（vCache 用 HNSW + 3 个 embedding 模型），以及 judge 那次异步回源。NexusFlow 现状 `src/api/response_cache.rs` 只有 canonical JSON 精确键（`response_cache.rs:212` 存 bytes + `Some(self.ttl())`），没有任何相似度判定——所以这是语义缓存阶段的**missing 算法**。落地顺序建议：先做第一轮的 per-tenant 键隔离 + context chain，再把静态 τ 换成 vCache 的 δ 机制（δ 可挂 `/admin/config` 热更，与 NexusFlow 现有 confers DynamicField 契合）。

---

### 2. Category-Aware Semantic Caching for Heterogeneous LLM Workloads

- **年份**: 2025-10-29（脚本输出 v1）
- **标识**: arXiv:2510.26835v1
- **作者**: Chen Wang, Xunzhuo Liu, Yue Zhu, Alaa Youssef, Priya Nagpurkar, Huamin Chen（IBM 系）
- **证据**: 全文 `/tmp/papers-caching2/category-aware.md`

**问题**：异构负载下统一缓存策略失配。论文给出生产工作负载的命中率长尾分布（Table 1，`category-aware.md:175-192`）：头部类别（代码生成 35% 流量命中率 55%、API 文档 25%/45%）可行；尾部五类（对话 chat 15%/12%、金融 10%/8%、法务 8%/10%、医疗 4%/6%、垂直域 3%/7%）命中率 6–12%，全部**低于向量库的经济盈亏平衡点**，被 40% 流量排除在缓存外。

**机制**：

*类别四属性 → 差异化策略*（§3.1-3.4）：embedding 空间密度（代码/API 稠密→紧阈值 0.88-0.90，会话稀疏→松阈值 ≤0.78）、重复模式（幂律 vs 均匀）、内容时效（0.01%/天 → 天级 TTL；80%/小时 → 分钟级 TTL）、计算成本（贵模型命中收益大→配额按经济价值而非流量分配）。**核心诊断**：固定阈值 0.80 时稠密代码空间产生 15% 误匹配，松到 0.90 降到 3%；稀疏会话空间在 0.80 则漏掉有效改写（§3.1）。

*混合架构降低 miss 成本*（§5）：把 **HNSW 搜索放在内存**（~2KB/条目）而**文档存外部**（按 ID 主键取，5ms）。寻优早停——HNSW 遍历时**返回第一个超过类别阈值的匹配**而非 top-k，阈值检查几乎免费（§5.3）。命中判定 Algorithm 1（§5.1）顺序：取类别配置 → `allowCaching` 合规闸（HIPAI/GDPR 类直接禁缓存，查询永不进缓存）→ embed → 带阈值的 HNSW → **miss 立即返回（零外部访问）** → **TTL 校验先于外部取文档** → 按 ID 取。

*盈亏平衡*（Eq 1-5，§4.4 / §5.5）：向量库 `L = 30 + 5h + (1-h)T_llm` → 需 `h > 30/(T_llm - 5)`（T=200ms 时 15.4%，T=500ms 时 6.1%）；混合架构 `h > 2/(T_llm - 5)`（T=200ms 时 **1.0%**，T=500ms 时 0.4%）——**盈亏点降低 10-15×**，使 5-15% 命中率的尾部长尾重新经济可行。

*淘汰打分*（§6.4）：`eviction score = priority × 1/age × hitRate`，按经济价值加权而非纯 LRU。

*负载联动*（Eq 6-7，§7.5）：定义负载因子
```
λ = min( 1, (L_p/L_target)·w_L + (Q/Q_target)·w_Q )      w_L + w_Q = 1
τ(λ) = τ_0 - λ·δ_max        t(λ) = t_0·(1 + λ·(β_max - 1))
```
`L_p` 模型延迟分位（如 P95）、`Q` 队列深度。**模型忙 → 放松相似度阈值 + 延长 TTL 换命中率、削到达模型的流量**；空闲时收紧。例：代码类别 τ_0=0.90/δ_max=0.05/t_0=7天/β_max=2，λ=0 → τ=0.90 TTL=7天；λ=1 → τ=0.85 TTL=14天。h_0=0.45 放松 0.05 后到 0.50，模型流量 55%→50%（-9%）。**诚实标注**：论文自述 9-17% 的流量削减是"理论投影"，基于"阈值放松→命中率线性上升"的假设，尚未在生产负载实证（`category-aware.md:560-563`）。

**对 NexusFlow 的适用性**：**高，且大部分是 improvement 而非 missing**。关键洞察——**分类本身可以零开销**：论文列出三种类别来源，其中"客户端在请求元数据显式指定"和"按端点划分"**零分类开销**，只有 prompt 分类器有延迟成本（§7.2）。NexusFlow 请求天然带 `model` 字段和端点，**把类别 = (endpoint, model) 或租户声明即为免费分类**，无需轻量分类器。负载因子 λ 的两个输入（延迟分位、队列深度）NexusFlow 路由侧**已有**（EWMA 延迟因子、QueueManager 队列/并发票据），把 λ 接到缓存 TTL 与阈值上是纯公式接线。混合架构（in-memory HNSW + 外部文档）对应"oxcache 存热命中键 + SQLite 存响应体"的分层——但注意第一轮 P2-1 已用 oxcache 直接存响应体 bytes，规模大时要评估外部化收益。

---

### 3. When Classic Cache Policies Fail: Learning-Augmented Replacement for Semantic Retrieval Buffers

- **年份**: 2026-07-01（脚本输出 v1）
- **标识**: arXiv:2607.00394v1
- **venue**: 论文 PVLDB Reference Format 标注为 **PVLDB**（`semantic-buffers.md:28`）；作者 Yushi Sun (Tencent LIGHTSPEED), Bowen Cao, Wai Lam (CUHK)
- **证据**: 全文 `/tmp/papers-caching2/semantic-buffers.md`

**问题**：把语义缓存的替换问题形式化为 **online semantic cache replacement with switching costs**——条目按 embedding 相似度匹配，**命中质量是连续量（相似度加权）而非二值**（§1）。

**关键实证发现（对 NexusFlow 选型有直接警示）**：在 MemoryBench-Full 两个数据集（LoCoMo, DialSim）上跑 8 种替换策略，**经典启发式 LRU、LFU 一致地不如朴素 FIFO**——原因是语义负载**缺时间局部性、缺频率集中性**（摘要, `semantic-buffers.md:11`）。这不是理论断言，是 8 策略对比实验。

**机制（SOLAR，两个耦合机制）**：

*机制一 —— regret 门控"何时修改"（Algorithm 1, `semantic-buffers.md:593-609`; Eq 5-7）*：
```
Init: c ← 0;  τ ← τ_0;  α ← 0.1
for each step t:
    R_t ← top-k(S_t, q_t);  observe qual_t
    δ_t ← 1 - qual_t;  c ← c + δ_t          # 累积 miss cost
    e_t ← experience item
    if c ≥ τ:                              # regret 累积过阈值才准入
        admit e_t; if full: evict via Alg.2
        c ← 0
        τ ← (1-α)·τ + α·δ_t               # 阈值在线自适应
```
判据来自成本目标：修改划算的条件是 `c ≥ λ·f(未来改善)`，在温和假设下简化为阈值 `c ≥ τ`，理论最优 `τ* = √(2λ/L)`（库存论 EOQ 结果，L 是平均损失率）；L 未知且非平稳故用 EWMA 估计（Eq 7）。**修改率自调节 ~17%**——缓存够好时累积慢、修改少；陈旧时累积快、修改多。修改总次数有界 `N ≤ T/τ`（Theorem 5.2）。

*机制二 —— 后验引导"改哪个"（Algorithm 2, `semantic-buffers.md:663-680`; Eq 8）*：每条目维护 Beta 后验 `μ_i ~ Beta(α_i, β_i)`，两个反馈通道：
- **正向**：条目进入 top-k → `α_i ← α_i + 1`
- **时间衰减**（隐式遗忘）：Δ 步未被取用 → `β_i ← β_i + 0.05`，让后验漂向低效用；**没有这条，早期有用的条目会积累永久正证据、永远无法被淘汰**（论文原文强调这点）
- **新颖性加分** `b_i = γ/(1 + age(i)/Δ_nov)`，防止刚准入的条目在观察不足时被提前淘汰
- 分数 `s_i = ṽ_i + b_i`（ṽ_i 从 Beta 采样，Thompson 式），**驱逐 `argmin s_i`**；新条目以 Beta(1,1) 先验准入

*保证*：竞争比 **≤3**（常数，与 cache 容量 K、时间跨度 T 无关；FIFO 是 Ω(K)），eviction regret `O(√(KT log T))`，匹配 `Ω(√(KT))` 下界到对数因子（摘要）。*复杂度*（§4.5）：不修改时 O(1) 累积 + O(k) 后验更新；修改时 O(K) 采样 + O(K) argmin；摊销 O(K)。*实验*：紧 cache 尺寸下相对 FIFO **5–75% 相对提升**，并刻画了 working set 边界的相变。

**对 NexusFlow 的适用性**：这是语义缓存的**淘汰/准入算法本体**，纯在线、零训练、O(K) 内存、单二进制 Rust 放得下（一个 `[f64; 2]` 的 Beta 数组 + 一个年龄分桶）。但要如实标注两点：(a) **前提是语义缓存已经存在**——对精确键缓存，命中是二值的、`qual_t` 无相似度加权，SOLAR 的连续质量建模退化成 vCache 式的 0/1 反馈，价值减半；(b) NexusFlow 现状 `response_cache.rs:303` 用的是 `oxcache::backend::default_memory_backend()`，底层是 **Moka TinyLFU**（`../oxcache/src/backend/memory/moka/backend.rs:646` → `moka_memory()`；`../oxcache/src/backend/score.rs:78` 注明 "Moka 是高性能内存缓存，使用 TinyLFU 驱逐策略"），**是频率敏感策略——SOLAR 的实验恰好说明这类频率启发式在语义负载上可能不如 FIFO**。也就是说：一旦从精确键走向语义键，现有淘汰策略不是"够用"，而是**方向可疑**，这是启动语义缓存时必须一并处理的。

---

### 4. Robust Learning-Augmented Caching: An Experimental Study

- **年份**: 2021-06-28（脚本输出 v1）
- **标识**: arXiv:2106.14693v1
- **venue**: 页脚标注 "Proceedings of the 38th International Conference on Machine Learning, PMLR 139, 2021"（`cacheus.md:73-74`）——即 **ICML 2021**；作者 Chłędowski, Polak, Szabucki, Zołna
- **证据**: 全文 `/tmp/papers-caching2/cacheus.md`

**问题**：学习增强缓存（learning-augmented caching）——用 ML 预测器帮决定淘汰什么，同时靠经典在线算法的鲁棒性兜底。两大流派：
- **reuse-distance 预测式**（Lykouris & Vassilvitskii 2018 PredictiveMarker、Rohatgi 2020 LMarker、Wei 2020 BlindOracle）：预测每个请求项"下次何时再来"（reuse distance——Belady 离线最优唯一看的统计量），预测误差 η_reuse 记为 L1 距离
- **最优策略预测式**（Antoniadis 等 2020 Trust&Doubt、RobustFtPD/PR）：预测"离线最优会淘汰谁"，误差 η_cache 记为最优配置与预测器配置的对称差大小

**机制（combiner 双专家切换）**（`cacheus.md:340-360`）：把非鲁棒的学习增强算法与经典鲁棒算法（MARKER 或 LRU）配对，用 combiner 组合——**同时模拟两个算法，实时追踪各自累计成本，某一方显著优于另一方时切换**。确定性 combiner 追溯到 Fiat 等 1994；随机化 combiner（Blum & Burch 2000）可把乘性开销压到 1+ε，代价是一个依赖 ε 的加性常数。

**实验结论**（摘要 + §5.2, `cacheus.md:11, 878-882`）：
- 最好的是 **BlindOracleD**（Wei 2020）与 **RobustFtPD**（Antoniadis 2020）——这两个**最简单**的方法既能很好利用好预测器，开销又低，且鲁棒性不逊于其他（`cacheus.md:728, 878-882`）
- 在 CRC (Cache Replacement Championship) 数据集 + Parrot（DNN 预测器）上，**预测器因训练数据稀缺而过拟合时，增强算法仍与经典 MARKER 相当**——理论鲁棒性在实践中得到实证
- 反过来说：其余方法叠加太多开销，耦合好预测器时更差却没换来更好鲁棒性

**对 NexusFlow 的适用性**：论文的实验是**字节流式的地址序列 + DNN 预测器**（Parrot），与 LLM 响应缓存的相似度检索负载不同，**Parrot/DNN 预测器部分不适用**，如实标注。但它给出的结论形态对单二进制网关高度可迁移：**"好预测器 + 经典策略兜底 + 成本追踪切换"是学习增强缓存里性价比最高的结构**。落到 NexusFlow：经典专家 = 现有 Moka TinyLFU（不动，零风险），预测专家 = 一个**廉价启发式**（per-key 历史命中间隔的 EWMA → 预测"下次何时再来" → 按 Belady 规则"淘汰最晚再来者"；无历史则退化为 FIFO），combiner = 追踪两方累计 miss 成本加权切换。**不需要任何模型训练**，用日志表即可在线更新。若哪天日志数据足够再换 DNN 预测器，combiner 骨架不变。

---

## 三、其余检索结果速览（仅基于标题+摘要，未下载全文）

| 标题 | 标识 | 关联 | 对 NexusFlow 的相关性 |
|------|------|------|---------------------|
| Asynchronous Verified Semantic Caching for Tiered LLM Architectures | arXiv:2602.13165 (2026) | 摘要级 | 中：分层 static(精选离线条目) + dynamic(在线) 缓存 + **异步验证**，与 NexusFlow "精确缓存=静态层、语义缓存=动态层" 的路线图同构，异步验证的工程形态可参考 |
| ConvoCache: Smart Re-Use of Chatbot Responses | arXiv:2406.18133 (2024) | 摘要级 | 低-中：语音聊天场景的语义缓存复评，与 MeanCache 同赛道但受众窄 |
| vCache 检索同批：GPT Semantic Cache / Continuous Semantic Caching / Ensemble Embedding / Key Collision Attack | 见第一轮笔记 | 第一轮已列 | 本轮不重复 |
| **KV 复用/淘汰（第 2 轮检索 8 篇全为推理引擎侧）**：RR-Evict（arXiv:2609.32278, 2026, agentic 前缀缓存细粒度淘汰）、Not All Tokens Are Worth Caching（arXiv:2605.18825, 2026, 学习式语义感知前缀淘汰）、Leyline: KV Cache Directives for Agentic Inference（arXiv:2606.01065, 2026）、Randomization Boosts KV Caching（arXiv:2601.18999, 2026, LRU 敏感性+随机化）、Continuum: KV Cache TTL（arXiv:2511.02230, 2025, 多轮 agent 调度 + KV TTL）、PCR: Prefetch-Enhanced Cache Reuse for RAG（arXiv:2603.23049, 2026）、ShadowServe（arXiv:2509.16857, 2025） | — | 摘要级 | **低**：这些论文管理的是**推理引擎内部 GPU/NPU/SSD 上的 KV 张量块**，NexusFlow 不执行推理、无 KV 块可管，**超职责边界**。唯一可间接借鉴的是 Continuum 的"KV 缓存 TTL"概念——它印证了第一轮已实现的会话亲和 TTL 表（oxcache TTL 表 + 命中续期）方向正确；Leyline 摘要提到 "KV-aware routing"，佐证前缀感知路由在工程界是被承认的路由维度 |
| **Prefix cache 准入/淘汰（第 3 轮，仅 3 篇）**：Marconi（arXiv:2411.19379, 2024, 混合模型 prefix cache）、Preserving Admission Responsibility in Multi-Tenant LLM Prefix Caches（arXiv:2608.01657, 2026, 多租户共享 GPU prefix cache 的准入责任）、WavePP（arXiv:2609.35263, 2026） | — | 摘要级 | **低-中**：Marconi/WavePP 纯推理引擎；arXiv:2608.01657 的多租户准入责任问题（一个租户物化 KV 块挤掉另一个租户的可复用状态）**在概念上**与第一轮已做的"多租户缓存键隔离"同源，但 NexusFlow 不共享 KV 状态，仅作旁证 |
| **Cache admission policy learning（第 4 轮）**：DEAP Cache（arXiv:2009.09206, 2020, 端到端学习 prefetch+admission+eviction 三策略）、RL-based Adaptive Context Caching（arXiv:2212.11709, 2022）、MoE Expert Caching 评估（arXiv:2608.07911, 2026）、Confidence-Gated Admission for HW Prefetching（arXiv:2609.04040, 2026）、FaultLens（arXiv:2608.26746, 2026） | — | 摘要级 | **低/不适配**：DEAP 与 RL 方案都需**离线训练流水线**，对单二进制网关代价过高；MoE 专家缓存、硬件预取、测试套件学习均非缓存算法本身。**但这轮噪声最大，因此补跑了第 5 轮换词**（learning augmented caching），才命中 CACHEUS 这条纯在线可落地的线 |
| CompetReuse / Blurry-LRU 线 | arXiv:1802.05399 (2018), arXiv:2106.14693 | 摘要级 + 深读 | 中（作为 CACHEUS 的前置工作被引用）：预测 reuse distance + Belady 规则 → 预测错时靠 eviction chain 修正兜底，即本笔记深读篇 4 的 Predict 档算法本身 |
| BlindOracle 上界改进 / Paging with Succinct Predictions | arXiv:2410.01760 (2024), arXiv:2210.02775 (2022) | 摘要级 | 低：纯理论（竞争比上界），无算法可实现性增量 |

---

## 四、检索局限（如实记录）

1. 第 3 轮 "prefix cache admission eviction LLM" 仅返 3 篇、第 4 轮 "cache admission policy learning" 噪声极大——这两个**问题描述的算法方向（KV 层准入/淘汰、离线学习型准入）恰恰是 LLM 网关最不需要的**（网关不持有 KV、不跑训练集群）。arXiv 检索天然偏推理系统论文，这是选题与语料的错配，不是检索失败。
2. 所有检索均为 arxiv 单源，未跑 multi/Semantic Scholar（第一轮已用 multi，本轮定位在 CS/AI 主题论文，arxiv 源相关性最佳）。
3. 深读 4 篇中，vCache（ICLR 2026）与 SOLAR（PVLDB）的实验配置（GTE/E5 embedding、LoCoMo/DialSim 记忆数据、CRC 字节流数据集）与 NexusFlow 的实际 LLM 响应缓存负载**不完全同类**，收益数字（12.5× 命中、5-75% 相对 FIFO 提升）**不可直接外推到 NexusFlow 部署**，只能作为方向性证据。具体收益须在 NexusFlow 自己的 SQLite 请求日志上离线回放验证。
4. Category-Aware 的 9-17% 流量削减是论文**自述的理论投影**，未经生产实证（`category-aware.md:560-563`）。本笔记转述时已保留此限定。

---

## 五、对照 NexusFlow 基线的可落地优化（按性价比）

**先说基线事实（本次实读代码核实）**：响应缓存在 `src/api/response_cache.rs`，精确 canonical JSON 键 + 租户隔离，TTL 从 `[cache].ttl_secs` 热读（默认 300s，`response_cache.rs:164-165`），存储 backend 是 `oxcache::backend::default_memory_backend()`（`response_cache.rs:303`，测试与生产同路径）——oxcache 该函数返回 **MokaMemoryBackend**（`../oxcache/src/backend/memory/moka/backend.rs:646`），即 **TinyLFU 频率敏感淘汰 + 自动过期**（`../oxcache/src/backend/score.rs:78`）。会话亲和另用 oxcache TTL 表。**结论：当前无任何相似度判定、无自适应阈值、无按负载/类别差异化策略、淘汰策略是频率启发式。**

| 优先级 | 建议 | 论文 | gapType | 落地代价 |
|-------|------|------|---------|---------|
| ★1 | **负载因子 λ 联动缓存 TTL**：`λ = min(1, (L_p/L_target)·w_L + (Q/Q_target)·w_Q)`，下游模型忙时延长 TTL、空闲时收紧。`L_p`（延迟分位）与 `Q`（队列深度）**NexusFlow 路由侧已有**（EWMA 延迟、QueueManager），纯公式接线，`/admin/config` 热更 `w_L/w_Q/target` | Category-Aware §7.5 Eq 7 | improvement（现有固定 TTL 300s → 负载自适应） | **low**：不需要 embedding、不需要语义缓存，可在**精确缓存**上立即做 |
| ★2 | **按 (endpoint, model) 差异化 TTL 与配额**：论文实证不同类别最优 TTL 差 3 个数量级（代码天级 / 股票分钟级）。NexusFlow 请求自带 `model` 字段，**零分类开销**即可切类别；淘汰可加经济价值权重 `priority × 1/age × hitRate` | Category-Aware §3.1-3.4, §6.4 | improvement | **low-medium**：配置项 + 键前缀化，改动集中在 response_cache.rs |
| ★3 | **淘汰策略从 TinyLFU 换成"鲁棒专家+预测专家"双专家切换**：鲁棒专家保留 TinyLFU 不动，预测专家 = per-key 历史命中间隔 EWMA 预测 reuse distance → Belady 规则选"最晚再来者"淘汰，combiner 追踪两方累计 miss 成本加权切换。**零训练**（用日志表在线更新） | CACHEUS 摘要/§2.2-2.3/§5.2 | improvement（现有 TinyLFU → 更优变体） | **medium**：可先在精确键缓存上做（键有明确的历史复用信号），是最容易验证学习增强结构的入口 |
| ★4 | **语义缓存命中判定用 vCache 的 δ 机制替代静态 τ**：每条目 sigmoid + MLE + (1-ε) 置信悲观界 → 探索概率 τ̂，用户只配 δ（挂 /admin/config）。纯 CPU、在线、无训练；judge 那次回源必须异步放关键路径外 | vCache Eq 9-11, Algorithm 1-2, Theorem 4.1 | missing（当前无相似度判定） | **medium-high**：门槛在 embedding 服务（本地小模型或外部 API）与向量索引，非判定算法本身 |
| ★5 | **语义缓存的准入/淘汰用 SOLAR 的 regret 门控 + Beta 后验**，**不要沿用 TinyLFU**：论文 8 策略对比实证 **LRU/LFU 一致差于朴素 FIFO**（语义负载无时间局部性/频率集中性），TinyLFU 是频率敏感策略，方向可疑。判据：累积 `c += (1-qual)`，`c ≥ τ` 才准入，`τ` EWMA 自适应（修改率自调节 ~17%）；驱逐 = Beta 采样 + 新颖性加分 argmin，含时间衰减 β+=0.05 防早期条目永不可淘汰 | SOLAR Abstract, Algorithm 1-2, Eq 5-8 | missing（语义缓存淘汰层）+ 对现有 TinyLFU 的警示 | **medium**：算法纯在线 O(K)、无训练、可实现；但前提是语义缓存已上线（精确键下 `qual` 二值化，价值减半） |
| ★6 | **静态层/动态层分层的异步验证缓存**：静态层=精选高频条目、动态层=在线学到的条目，验证异步化 | Asynchronous Verified Semantic Caching arXiv:2602.13165 | missing | **low-medium**（摘要级，未深读，机制待确认） |
| — | 不建议启动：**KV 张量层**的准入/淘汰（RR-Evict、Marconi、ShadowServe、Continuum 等 10+ 篇） | 第 2/3 轮检索 | — | **不适用**：NexusFlow 不执行推理、不持有 KV 块，论文管理的是 GPU/NPU/SSD 上的张量块，超出职责边界。仅"KV-aware routing"概念与 Continuum 的 TTL 思路作为会话亲和（第一轮已实现）的旁证 |

**与第一轮建议的衔接关系**：第一轮 P2-1 已把"精确响应缓存落地 oxcache + per-tenant 键隔离，语义缓存二期"写入报告。本轮在二期（语义缓存）之外，找到了**两个不依赖语义缓存、可以立刻做的改进（★1 ★2）**和一个**在现有精确缓存上就能验证学习增强结构的中等代价改进（★3）**；而第一轮明确暂缓的三项中，"语义缓存（等日志数据）"本轮**新增了启动依据的算法级证据**（vCache 的 δ 机制使语义缓存第一次有了无需调 τ 的形式化正确性保证 + Category-Aware 证明长尾类别的盈亏点可低至 1%），但**仍取决于日志数据就绪度**，本轮未找到任何"可以不等日志数据就开语义缓存"的证据，故维持暂缓判断。
