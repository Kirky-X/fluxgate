# LLM 路由与模型选择算法（bandit / 在线学习方向）—— 第二轮调研笔记

- **调研日期**：2026-10-01
- **调研工具**：liuxiang 论文检索脚本（`search_papers.py --source arxiv --limit 8`，4 轮）+ arXiv abs 页 / HTML 全文补抓（WebFetch）
- **与第一轮的分工**：第一轮（`out/research/papers-routing.md`）覆盖了偏好数据离线训练路由器（RouteLLM）、质量-成本基准（RouterBench/RouterEval）、路由崩溃、级联校准（UCCI）。本轮聚焦 **在线学习 / bandit / 上下文模型选择** —— 不依赖离线标注、边服务边学习，与 NexusFlow 单二进制推理时网关形态更契合。
- **本轮已核对的代码事实**：`route(&self, abstract_model: &str)`（`src/router/router.rs:59`）及全部入口（`route_excluding`/`route_with_session`/`route_with_hints`）只接收抽象模型名 + 可选 session_key/hints，**查询内容从不参与路由决策**；`weighted_select`（`src/router/router.rs:382`，pub 版本 `:424`）；power-of-two-choices（`:484`）；EWMA 第 4 维输入（`:536`）；`grep -rn -i "bandit|linucb|ucb" src/router/` 无结果 —— **无任何 bandit/UCB 实现**。

---

## 一、检索记录（4 轮，全部 `--source arxiv --limit 8`）

| # | 查询词 | 命中 | 本轮深挖/记入的论文 |
|---|--------|------|--------------------|
| 1 | `LLM router learning algorithm` | 8 篇 | Lodestar（简评）；其余多为 MoE 内部路由（Read-ME/PR2）或 EDA 领域（GR-Evolve），不适用 |
| 2 | `bandit routing large language models` | 8 篇 | **BaRP**、**MixLLM**、LLM Bandit、Adaptive Routing under Budget、Correlation-Aware Contextual Bandits、UniScale、Meta-Learning 隐式偏好 |
| 3 | `contextual model selection LLM` | 8 篇 | **LinUCB 非结构化上下文演化**、Pandora's Box 级联、PAK-UCB、GraphRouter、拍卖式选择、Neural Bandit 子任务管线 |
| 4 | `cost aware LLM routing benchmark 2026` | 8 篇 | **SLARouter**、RouteJudge、Batch Prompting 路由、冷启动任务画像、LLMRouterBench/MMR-Bench（第一轮已记 LLMRouterBench） |

第一轮已报告的论文（RouteLLM、RouterBench、When Routing Collapses、Universal Model Routing、UCCI、LLMRank、CARROT、RouterEval、Route to Rome、MMR-Bench、LLMRouterBench、端云协同 2509.24050 等）本轮不再重复深挖。

---

## 二、深挖论文（4 篇，机制提炼到可照着实现）

### 1. Online Multi-LLM Selection via Contextual Bandits under Unstructured Context Evolution（LinUCB 路由）

- **年份 / 标识**：2025 / arXiv:2506.17670v1（检索第 3 轮命中，HTML 全文已抓）
- **机制**（输入→决策→更新，公式照录自 HTML 全文）：
  - **输入**：每轮查询 `Q_t` 的初始上下文向量 `x_{t,1} ∈ ℝ^d`（实验用 384 维句向量）；若上一步反馈 `r=0`（不满意），上下文按黑盒函数演化 `x_{t,h+1} ← g(x_{t,h}, a_{t,h}, R_{t,h}, r_{t,h})`（拼接/改写/用户编辑），**不学习 g**。每个 LLM 臂 k 维护 `A_k ∈ ℝ^{d×d}`、`b_k ∈ ℝ^d`。
  - **决策**（Algorithm 1 line 7）：对每个候选模型 k 求 `θ̂_k ← A_k⁻¹ b_k`，`UCB_k ← ⟨x, θ̂_k⟩ + α·√(xᵀ A_k⁻¹ x)`，选 `arg max_k UCB_k`。实验超参：α=0.675, λ=0.45, H=4 步。
  - **更新**（line 11）：初始化 `A_k ← λI_d, b_k ← 0`；观测反馈 `r ∈ {0,1}` 后只更新被选臂：`A_a ← A_a + x xᵀ`，`b_a ← b_a + r·x`。纯闭式最小二乘，**无梯度训练**。
  - **预算感知变体**（§5.1）：成本估计 `ĉ_k`（经验均值）+ 宽度 `β_k = √(log(2TK/δ)/(2N_k))`；选择分数 `Score_k = UCB_k / max{ĉ_k − β_k, ε}`，在"上界成本 `ĉ_k + β_k` 不超剩余预算"的臂中取最高分——即**单位成本置信回报最高者**。
  - **理论**：sublinear regret `Õ(SL·√(KdTH))`（Theorem 1）；预算版见 Theorem 2。
- **gapType**：**missing** —— NexusFlow 的 EWMA 是"每 vendor 全局标量"，无上下文特征、无探索-利用框架（grep 无 bandit/UCB 实现）。LinUCB 是它的上下文感知 + 乐观探索升级。
- **适用性**：**高，是本轮最贴合单二进制的算法**。决策+更新只用矩阵乘/求逆（d 维，Rust 可手写 Cholesky 或降维到 d≤64 的哈希特征后 O(d³) 可忽略），无外部依赖、无训练管道、O(d²) 内存每模型。**关键落地约束是上下文向量来源**：网关不该内置 384 维 embedding 模型——务实方案是 (a) 对 prompt 做特征哈希（feature hashing / n-gram 哈希到 d 维 0/1 或 tf 向量），NexusFlow 已有前缀哈希经验（会话亲和）；(b) 可选对接供应商返回的 embeddings 端点。起步可先把 `x` 定义为极小的人工特征（消息数、字符长度、是否结构化输出、任务 header、租户 one-hot），先跑通机制再扩维。多步演化 g 对网关意义有限（NexusFlow 逐请求独立），但**单步版（H=1）退化为标准 LinUCB，恰好是 NexusFlow 需要的形态**；预算感知变体可直接与 limiteron 的 Token 预算联动。

### 2. SLARouter: Cost-Optimal LLM Routing with Limited User Feedback under User Satisfaction Guarantees

- **年份 / 标识**：2026 / arXiv:2606.19376v2（检索第 4 轮命中，HTML 全文已抓）
- **机制**（公式照录自 HTML 全文）：
  - **输入**：满意度预测器 `ŝ_{m,t}`（每模型一个输出头；论文用冻结 ModernBERT + per-model MLP 头）；标量虚拟队列 `Q_t`（累计 SLA 违约）；每模型正/负反馈计数 `N_m^+, N_m^-`（用于类别重加权 `p_m = N_m^-/N_m^+`）。
  - **决策**（利用期，Eq. 4）：`m̃ = arg min_m { V·C_{m,t} + Q_t·(α − ŝ_{m,t}) }`。第一项买便宜模型，第二项在队列大（SLA 快违约）时把选择推向高预测满意度模型；`V` 越小 SLA 收敛越快、成本越高，实践取 `V = (Q_max·ε)/ΔC`。
  - **探索**：每轮以 `p_t = min(1, c/√[4]{t})` 概率均匀随机选一个模型，收集**单侧稀疏反馈**（用户对被选模型点赞/点踩）；有反馈才入库 `𝒟` 并对**该模型头**做一步 SGD（masked BCE）；无反馈参数不动。
  - **队列更新**（Eq. 5）：`Q_{t+1} = max{0, Q_t + α − s}`，有反馈用 `s`，无反馈用预测器 `ŝ` 代替——稀疏反馈由此不中断控制回路。可选 LLM-judge 合成反馈补稀疏性（判官-用户一致率 94.4%/87.9%）。
  - **理论**：Lyapunov drift-plus-penalty，`E[Q_t] ≤ γ + O(√t)`，时间平均违约 ≤ 消失项，成本 ≤ `C^OPT + O(β/T^{1/4} + 1/V)`。自报结果：11 个基准全部满足 α 约束、免逐基准调参，SLA 合规方法中成本最低至 1/2.22。
- **gapType**：**missing** —— (a) NexusFlow 无"满意度/SLA"概念（错误率≠满意度，200 响应也可能被用户点踩）；(b) 无约束优化回路（现有 EWMA 只被动统计，不反推预算/降级决策）。
- **适用性**：**高，且可分两档落地**。① **零模型版**（low effort）：`ŝ_{m,t}` 先用现有 EWMA 满意度代理（如 `1 − 错误率`），整个决策式 `argmin{V·C + Q·(α−ŝ)}` 只是一个标量队列 + 每模型 2 个数的比较，完全可塞进 `weighted_select` 前的打分层，且与 `/admin` 热更体系兼容（V、α 可做成 DynamicField）。② **完整版**（medium-high effort）：需要轻量分类器与稀疏在线更新，Rust 侧可用线性/小 MLP 头 + SGD 手写；ModernBERT 依赖是论文实现选择而非算法必需。对多租户网关，SLA 语义可映射为"租户级质量承诺"，Q_t 可按租户维护——这是现有多维权重体系做不到的表达力。

### 3. MixLLM: Dynamic Routing in Mixed Large Language Models

- **年份 / 标识**：2025 / arXiv:2502.18482v1（检索第 2 轮命中，HTML 全文已抓）
- **机制**（公式照录自 HTML 全文）：
  - **查询表征**：`e_n = Encoder(q_n)`（BERT）；用 InsTag（Llama-2-13B）生成 6000+ 细粒度标签、聚成 20 个粗域，然后**无监督微调 encoder**：`L = L_intra + L_inter`，`L_intra` 把嵌入拉向所属域中心（对域中心做 softmax 的交叉熵），`L_inter` 推开不同域中心——依据是"域-质量相关"（如 GPT-4 在 Legal/Math 上更常错）。
  - **每模型轻量预测器**：质量回归器 `p̂_{n,l} = f_l^rq(e_n)`（实践中 random forest）；成本 = 已知输入成本 + 预测输出长度：`ĉ_{n,l} = len^prm·price^prm_l + lêñ^res·price^res_l`，其中 `lêñ^res = f_l^rl(e_n)`（每模型各自 RF/MLP/KNN，MLP <2MB）。
  - **元决策器**：`s_{n,l} = s^trade + α·s^unc − β·s^pen`，取 `arg max_l s_{n,l}`：
    - 权益权衡 `s^trade = [λ/(λ+1)]·p̂ − [1/(λ+1)]·ĉ`（λ = 付费意愿）；
    - 不确定性 `s^unc = e_nᵀ A_l⁻¹ e_n`（LinUCB 式协方差项）；
    - 延迟惩罚 `s^pen = e^{γ·(w_l − ξ·τ)}`，`w_l` = 首 token 延迟 + 生成时长，τ = 可容忍上限（30s）。配置 α=0.01, β=0.1, γ=0.1。
  - **在线持续学习**：上下文 bandit + 策略梯度。共享 3 层 MLP 产生动态反馈分 `s^df`，最终分 `s′ = s + κ·s^df`，置信因子 `κ = 1/(Var[s^df]+ε)`；选择概率 `π(l|e_n) = softmax(s^df)`，二值用户反馈 `r_n` 下更新 `θ := θ − η·∇log π(m_n*)·r_n`（η=0.001）。
  - **自报结果**：GPT-4 质量 97.25% @ 24.18% 成本（λ=1.4）；预测器按模型独立 → 增删模型不需全量重训。
- **gapType**：**improvement** —— 它是 NexusFlow 现有要素的"内容感知化"总装：EWMA 延迟 ≈ 其 `s^pen` 的退化版；`/admin` 热更权重 ≈ 其 `s^trade` 的静态版；而 missing 的部分是 **per-(查询域, 模型) 质量预测器** 与 **不确定性探索项**。
- **适用性**：**中，借鉴价值大于照搬价值**。`s = s^trade + α·s^unc − β·s^pen` 的三合一打分式可直接作为 `weighted_select` 的下一代评分骨架：`s^trade` 用 config 单价 × 预测输出长度（输出长度预测可用简单的按模型历史均值起步）、`s^pen` 直接换成已有 EWMA 延迟、`s^unc` 用 LinUCB 协方差项。**落地代价在特征侧**：需要 embedding/标签基础设施（网关内嵌 encoder 或调外部 embeddings API），tags 管道（InsTag + 聚类 + 对比微调）对单二进制网关偏重，如实标注为 high effort；但其中间形态——"人工小特征 + 每模型随机森林级预测器 + 三项打分"——在 Rust 中可实现（RF 可换为可解释的分段线性/GBDT 小模型），属 medium。

### 4. BaRP: Learning to Route LLMs from Bandit Feedback — One Policy, Many Trade-offs

- **年份 / 标识**：2025 / arXiv:2510.07429v1（检索第 2 轮命中，HTML 全文已抓）
- **机制**（公式照录自 HTML 全文）：
  - **输入**：冻结句向量编码器 h（all-MiniLM-L6-v2，384 维）→ `h(x)`；偏好向量 `w_t = (w^q, w^c)` 在 1-单纯形上（性能/成本权重和为 1），经小 MLP φ 映射；拼接 `z = [h(x); φ(w)]`。
  - **架构**：决策头 `g_θ` = 两层 MLP → K 个模型的 logits → `π_θ(a|x,w)`。
  - **推理决策**（免重训调偏好）：`a*(x,w) = argmax_a π_θ(a|x,w)`；运维/租户**逐请求改 w 即改变行为**（w^c 从 0.2→0.8 时成本 $0.074→$0.015、分数平缓下降）。
  - **训练**（bandit 反馈模拟）：每轮只取被采样臂的 `q_t, c_t`（即便日志里有全模型标签，也**只暴露被选臂**，与部署一致）；偏好 `w_t` 每实例均匀重采样于单纯形 → 一个策略服务全部偏好；奖励 `r_t = w^q·q_t − w^c·c̃_t`，`c̃_t = min(c_t/τ, 1)`（成本封顶校准）；损失 = REINFORCE + baseline + 熵正则：`L = −(r_t − b_t)·log π(a_t|s_t) − β·H(π)`，baseline 取 batch 均值，β=0.05，Adam lr=1e-4, batch 32, 100 epochs。无 off-policy 修正。作者对比 LinTS/LinUCB/ε-greedy 作为决策头，REINFORCE 最优（0.7432 vs 0.6556 平均分）。
  - **自报结果**：比离线全信息路由器高 ≥12.46%，比最大 LLM 高 ≥2.45%。
- **gapType**：**missing** —— NexusFlow 没有"策略可学习"的部件，也没有"单策略 × 多偏好"表达（现在的 trade-off 靠 `/admin` 热改 3 个系数，全局一刀切、多租户无法各配各的）。
- **适用性**：**中高，但前提是先有训练管道与数据**。推理侧极轻（冻结 encoder + 两层 MLP，CPU 毫秒级，可 ONNX/tract 内嵌单二进制），真正成本在离线侧：需要 (query, model, quality, cost) 日志——这正是 NexusFlow 已有的 SQLite 异步日志 + Token 预聚合的产出，也是**第一轮 P0"质量-成本前沿度量"暂缓项的直接下游**：先有度量与数据积累，BaRP 才有原料。quality 标签在网关侧可用"成功完成 + 用户反馈 + 重试/放弃行为"等代理起步。"偏好向量 = 输入参数"的思想对 NexusFlow 有一处零训练即用的映射：把现有 3D 系数（0.4/0.3/0.3 + EWMA 因子）推广为**每租户可覆盖的偏好向量**并允许 API header 传入，语义与 BaRP 的 w 同构，只是先由启发式而非学习策略消费。

---

## 三、其他真实命中（摘要级，未读全文，简评）

| 论文 | 年份 / 标识 | 摘要级要点 | 适用性 |
|------|------------|-----------|--------|
| Lodestar: An Online-Learning LLM Inference Router | 2026 / arXiv:2606.00946 | 在线学习推理路由器，目标是 TTFT 与 GPU 利用率，路由对象是**集群内 GPU 实例** | 中低：路由对象不同（NexusFlow 路由到外部 vendor API），但其"网关内在线学习"系统结构可参照 |
| PAK-UCB Contextual Bandit | 2024 / arXiv:2410.13287v6 | prompt-aware 的在线 UCB 选生成模型（依标题与脚本摘要开头；具体机制未读全文） | 中：定位是"prompt 感知的轻量在线 bandit"，可作为 LinUCB 的低配起步方案（轻量程度待读原文确认） |
| Correlation-Aware Contextual Bandits with Surrogate Rewards for LLM Routing | 2026 / arXiv:2607.09015 | 相关臂 + **代理奖励**（用 ML 模型打分替代真实 bandit 反馈）的理论 bandit | 中：其"代理奖励"思想直接缓解 SLARouter/一切在线路由的反馈稀疏问题——网关可用廉价启发式代理（格式正确性、长度合理性）先喂 bandit |
| Adaptive LLM Routing under Budget Constraints | 2025 / arXiv:2508.21141v2 | 预算约束下的自适应路由（脚本摘要截断于"Previous approaches tre…"，方法细节未读） | 中：与 LinUCB 预算变体同方向，备选参照 |
| Neural Bandit Based Optimal LLM Selection for a Pipeline of Subtasks | 2025 / arXiv:2508.09958v3 | 多子任务管线上每步用神经 bandit 选模型 | 低-中：NexusFlow 是逐请求独立转发，管线场景弱相关 |
| Online Pandora's Box for Contextual LLM Cascading | 2026 / arXiv:2606.07392 | 上下文 Pandora's Box 模型：每轮先决定"查询哪些 LLM API"、再决定选择哪个，两阶段在线决策（摘要原文为 two-phase decision problem / adaptively querying and selecting LLM APIs） | 中低：与第一轮 UCCI 同为级联方向，NexusFlow 的 retry/fallback 链是天然宿主，但需质量评估信号 |
| Task-Aware LLM Routing ... Cold-Start | 2026 / arXiv:2604.09377 | 多级任务画像合成数据解决路由冷启动泛化 | 中：NexusFlow 新供应商入池冷启动时可参照"合成画像"思路（与第一轮 Universal Model Routing 探针建议互补） |
| GraphRouter | 2024 / arXiv:2410.03834v2 | 图结构上做 LLM 选择以利用关系信息（脚本摘要截断，图构造细节未读） | 低-中：若依赖离线建图+图神经网络推理，则对单二进制不友好（待读原文确认） |
| Truthful Reverse Auctions ... Contextual MAB | 2026 / arXiv:2602.14476 | 多供应商**报价**场景下的拍卖式选择 + 上下文 MAB | 低-中：仅当 NexusFlow 做动态计价/竞价接入时有意义 |
| Towards Cost-effective LLMs Routing with Batch Prompting | 2026 / arXiv:2605.28268 | 路由与批提示（多查询合一请求）联合优化 | 中低：网关若做请求合并/聚合可借鉴，改造面大 |
| RouteJudge | 2026 / arXiv:2606.18774 | **路由器级**（非模型级）成对偏好评测的公开平台 | 中：为 NexusFlow 的路由 A/B 与回归测试提供评测方法论参照（"比较路由器决策而非响应质量"） |
| UniScale | 2026 / arXiv:2605.30898 | 模型路由 × 测试时扩展（采样数等）联合在线优化 | 低-中：NexusFlow 不控模型侧推理参数（除 max_tokens 等透传），可操作面窄 |
| LLM Bandit (Preference-Conditioned) | 2025 / arXiv:2502.02743 | 偏好条件化动态路由，与 BaRP 同方向早半年 | 低：BaRP 是其更完整版本，读 BaRP 即可 |
| Learning to Route LLMs from Implicit Cost-Performance Preferences via Meta-Learning | 2026 / arXiv:2606.06178 | 从隐式偏好元学习路由 | 低：机制偏离线元训练 |
| MixLLM 之外 bandit 检索其余（Learning to Route Languages / Knowledge Updating / LLM-Lasso / Read-ME / PR2 / GR-Evolve / Intelligent Router for LLM Workloads / Pyramid MoA / HybridInfer / Multi-Agent Set-Valued Prediction） | 2024-2026 | 分别为多语言 RL、模型编辑、特征回归、MoE 内部路由、MoE-RL 训练稳定性、EDA、prefill/decode 感知集群调度、anytime 级联、端云热约束、agent 集合预测 | 不适用或与第一轮已记主题重叠（Intelligent Router 2408.13510 的 prefill/decode 感知与第一轮 HW-Router 结论一致：运行时信号优于静态权重，已体现在 NexusFlow queue/window 因子中） |

---

## 四、落地建议汇总（按优先级，衔接第一轮）

| 优先级 | 建议 | 依据 | 与 NexusFlow 的接口 |
|--------|------|------|--------------------|
| P0 | **SLA/满意度虚拟队列（SLARouter 零模型版）**：`Q_t += max(0, α − s)` 滑动进打分，`argmin{V·C + Q·(α−ŝ)}`，`ŝ` 先用现有 EWMA 满意度代理，V/α 热更 | SLARouter (2606.19376) | `weighted_select` 打分层 + `utils/metrics.rs`（Q_t 暴露为指标）+ confers DynamicField；effort=low |
| P1 | **LinUCB 打分项（哈希特征起步）**：以人工小特征/特征哈希为 `x`，每模型维护 `A_k, b_k`，UCB 项并入现有加权评分（替换/叠加 EWMA 错误率因子）；预算变体对接 limiteron Token 预算 | LinUCB 论文 (2506.17670) | `src/router/` 新增 bandit 模块（无新依赖，闭式更新）；effort=medium |
| P2 | **三合一评分骨架**：`s^trade(单价×预测长度) + α·s^unc − β·s^pen(EWMA延迟)` 替换静态 3D 系数中的静态项 | MixLLM (2502.18482) | `router.rs` 评分函数重构；输出长度预测可从已有 token 预聚合表取每模型历史均值起步；effort=medium |
| P3 | **租户级偏好向量**：把 3D 系数 + 偏好语义推广为每租户/每请求可覆盖的 `w=(w^q, w^c)` 输入（header/API 参数），为后续学习策略（BaRP）预留接口 | BaRP (2510.07429) | `api/` + `router/` 配置面；effort=low（仅接口，无学习） |
| P4 | **数据积累 → 离线训练小路由器（BaRP 式）**：依赖第一轮 P0 质量-成本度量与日志积累；训练产出 = 冻结 encoder + 小 MLP，可 ONNX 内嵌单二进制 | BaRP (2510.07429) | 长期路线；effort=high |

**与第一轮结论的衔接**：第一轮 P0（质量-成本前沿度量）是本轮 P4（BaRP）和 P1（LinUCB 奖励来源）的共同前置——在线算法都需要"每请求的质量信号"，度量先行不变。第一轮 P2（内容选池、负载选实例）与本轮 LinUCB/MixLLM 是同一方向的不同实现深度：tier 判别器（第一轮）是粗粒度开环版，bandit（本轮）是细粒度闭环版，可共享同一插入点（`route()` 之后、`weighted_select` 之前）。

---

## 五、诚实声明

- 全部论文由 `search_papers.py` 真实命中（4 轮查询词见第一节，与任务建议一致，未增删）；标题/年份/标识以脚本输出为准，均为 arXiv 预印本。
- 4 篇深挖论文（2506.17670、2606.19376、2502.18482、2510.07429）已抓取 arXiv HTML 全文提取公式与伪代码；其中 MixLLM 的 HTML 末尾未含独立 Algorithm 伪代码块，机制系其 Eq.(1)-(22) 的转述。其余论文仅基于 arXiv 摘要页（SLARouter/BaRP/MixLLM/LinUCB 的 abs 页也抓过，深挖以 HTML 全文为准），表中已标注"摘要级"。
- 所有量化结果（sublinear regret、2.22×、97.25%@24.18%、12.46%、$0.074→$0.015 等）为论文自报，未经本环境复现。
- 检索无失败轮次；第 1 轮多数命中为 MoE 内部路由/EDA 领域，属检索主题真实边界，如实记录为不适用而非遗漏。
- NexusFlow 侧代码事实为本轮 `grep` 实测（`route()` 入参、`weighted_select` 位置、bandit 关键词零命中），非转引第一轮。
