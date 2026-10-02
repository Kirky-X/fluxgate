# 网关效率与响应缓存 —— 论文检索笔记

- **日期**: 2026-09-30
- **工具**: `python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py`
- **目标**: 为 NexusFlow（Rust + Axum 多厂商 LLM 网关）寻找「网关效率与响应缓存」方向的论文级优化证据
- **全文来源**: 深读的 3 篇已用 `download_paper.py` 下载转 Markdown（Pandoc 不可用，走 PDF 提取路径），存于 `/tmp/papers-gateway/`；其余论文仅基于检索返回的标题+摘要判断

## 检索记录（实际执行的命令）

| # | 查询词 | 结果 |
|---|--------|------|
| 1 | `search_papers.py "LLM response caching semantic cache" --source arxiv --limit 8` | ✅ 8 篇，全部命中语义缓存方向 |
| 2 | `search_papers.py "LLM streaming proxy inference efficiency" --source arxiv --limit 8` | ❌ 空结果（"未找到相关论文"），已换词补偿 |
| 3 | `search_papers.py "API gateway design LLM" --source arxiv --limit 8` | ✅ 7 篇，3 篇网关相关 |
| 4 | `search_papers.py "prefix caching KV cache LLM serving" --source arxiv --limit 8` | ✅ 8 篇，但偏推理服务端（GPU/SSD KV 管理），对代理网关适用性低 |
| 5 | `search_papers.py "LLM gateway routing efficiency" --source multi --limit 8` | ✅ 聚合 27 条去重后 8 条，2 篇相关（DBLP 源失败，属脚本已提示的正常降级） |

说明：原计划 2~4 次查询，因第 2 次空结果补跑一轮，共 5 次。Semantic Scholar 未被单独调用（指定了 arxiv/multi 源）。

---

## 深读论文（已下载全文，提炼可落地机制）

### 1. LaCache: Robust Semantic Caching for LLM Serving

- **年份**: 2026（arXiv:2608.01718，脚本输出 2026-08-03）
- **venue**: arXiv (cs.AI)，Stony Brook University
- **证据**: 全文 `/tmp/papers-gateway/lacache.clean.md`（PDF 提取）

**核心思想**：语义缓存（按 embedding 相似度复用响应）存在**缓存碰撞攻击**面——攻击者注入精心构造的查询，使其缓存键与受害者的良性查询碰撞，让后续良性请求命中攻击者控制的响应。现有防御（困惑度过滤、查询-响应一致性、LLM-as-Judge）都在查询侧，可被 GCG 等梯度优化绕过。LaCache 把完整性校验从查询侧转移到**响应侧**：

- **双阶段检查**（Algorithm 1，`lacache.clean.md:296-311`）：缓存条目从 `⟨h(x), f(x)⟩` 扩为 `⟨h(x), [h(f_k(x)), f(x)]⟩`。命中需同时通过 ① 查询键检查 `h(x)=h(x')`，② **lookahead 检查**——对当前查询投机解码前 k 个 token，验证其与缓存响应前 k token 的哈希/相似度一致。两项都过才返回缓存。
- **开销控制**（§4.3，`lacache.clean.md:313-353`）：前 k token 用轻量 draft 小模型生成（k≤20），缓存未命中时这些 draft token 通过投机解码（Leviathan et al. 2023）被大模型复用，验证 k 个 draft token 只需一次前向。
- **理论保证**（Theorem 1）：在语义可分性 + 前缀一致性两个假设下，检测任何产生语义不同响应的碰撞攻击的概率 ≥ 1−p₂；用 m 个独立索引函数做集成可将检测概率提到 ≥ 1−p₂ᵐ（`lacache.clean.md:236-247`）。
- **实验**：4 组 LLM×embedding 组合下攻击命中率（AHR）降至 0（如 Qwen3+BGE 上 KCA 攻击 AHR 0.80→0.00），良性命中率（BHR）保持 1.00（Table 1，`lacache.clean.md:596-601`）；保留 >90% 良性缓存效用（摘要）。延迟：k=20 时默认骨干 108.5 ms/query，配 draft 模型+投机解码 68.6 ms（`lacache.clean.md:1267-1269`）。

**对 NexusFlow 的适用性**：
- NexusFlow 是**多租户**网关，若引入语义缓存，跨租户缓存投毒是真实威胁（一个 API key 的用户可污染另一个用户的命中结果）。LaCache 的威胁模型与网关场景高度吻合。
- **完整 lookahead 机制适用性中等**：投机解码需要网关侧部署 draft 小模型，超出当前"纯转发+转换"架构，落地成本高。
- **低成本先行方案**（论文 related work 提及，`lacache.clean.md:230`）：per-tenant 键隔离/键加盐（per-tenant key randomization），在 oxcache 的 key 上拼 API key 哈希即可，零模型成本先堵住跨租户投毒。

### 2. Closing the Operational Gap in Semantic Caching

- **年份**: 2026（arXiv:2606.19719，脚本输出 2026-06-18）
- **venue**: arXiv (cs.IR)；PDF 首页脚注标注 **Accepted at EMNLP 2026 (Industry Track)**（`operational-gap.clean.md:64`），作者来自 NYU + Redis
- **证据**: 全文 `/tmp/papers-gateway/operational-gap.clean.md`（PDF 提取）

**核心思想**：语义缓存的模型选型一直用 PR-AUC——一个**只衡量排序质量、与固定阈值可用性无关**的指标。论文证明这导致系统性错误选型：**PR-AUC 最高的模型在运营中往往最差**（摘要；`operational-gap.clean.md:31-34`）。核心机制：

- **两个缓存感知指标**（§1-§3）：① **P-CHR AUC**（Precision–Cache Hit Ratio AUC）——衡量在**每个阈值 τ 对应的缓存利用率**下的精确率，即"精度-利用率"权衡曲线，而非"精度-召回"；② **ORR**（Operational Retention Rate）——部署后（固定 τ）保留了多少离线排序质量。
- **运营差距分解**：离线 PR-AUC 与部署 P-CHR 之差 = **结构性差距**（不可消除，由数据集正例率决定）+ **阈值-效用差距**（可恢复，源于模型分数的绝对位置而非概率校准问题）。
- **修复手段**：阈值-效用差距由**训练目标**决定而非数据规模，且只会对"**在候选池上重归一化分数**"（re-normalizing scores over the candidate pool）或更换训练目标让步（摘要；`operational-gap.clean.md:44-48`）——即 bi-encoder 原始余弦分数与 cross-encoder 分数不能直接混用同一个 τ。
- **配套发布**：LangCache SentencePairs 数据集（1M/8M/40M 对，HF 开源）、LangCache-Embed-v3 与 LangCache Reranker 模型，均面向语义缓存场景精调（§2.1-§2.2，`operational-gap.clean.md:96-102`）。
- **规模依据**：约 **33% 的 LLM 聊天日志请求是重复或语义相似的**（`operational-gap.clean.md:68-75`），评估覆盖 9 个 retriever × 10 个 reranker × 74,265 条测试查询。

**对 NexusFlow 的适用性**：**高，可立即落地**。
- 这是"给缓存决策选型和调参的方法论"，不要求改架构：NexusFlow 若加语义缓存，选 embedding/reranker 模型和设定 τ 时应直接跑 P-CHR/ORR 曲线（可在现有 SQLite 日志上离线回放），而不是看 PR-AUC。
- LangCache 开源模型可直接作为网关侧 embedding/reranker 候选，省去自训。
- 与 `/admin/config` 热更新契合：τ 调整可以做成热更新项，按 P-CHR 曲线在线校准。

### 3. MeanCache: User-Centric Semantic Caching for LLM Web Services

- **年份**: 2024（arXiv:2403.02694，脚本输出 2024-03-05）
- **venue**: 脚本输出 "IEEE International Parallel and Distributed Processing Symposium"；PDF 首页标注 **Accepted at 2025 IEEE 39th IPDPS**（`meancache.md:17`），Virginia Tech + Cisco + UMN
- **证据**: 全文 `/tmp/papers-gateway/meancache.md`（PDF 提取，UTF-8 干净）

**核心思想**：针对 GPTCache 类语义缓存**误命中率高、且无法处理上下文（多轮）查询**的问题，提出用户侧缓存 + 三个可迁移机制：

- **上下文链（context chain）匹配**（Algorithm 1 说明，`meancache.md:334-338, 649-655`）：每个缓存条目记录其**父查询链**（产生该查询的对话历史）。判定命中时要求 ① 新查询与缓存查询 embedding 余弦相似 **且** ② 新查询的会话历史与缓存条目的 context chain 匹配。语义相同但上下文不同的查询不再错误命中——这是多轮 chat 场景误命中的直接解法。
- **PCA 压缩 embedding**（`meancache.md:154-158`）：把向量投影到低维空间加速匹配，存储需求 **−83%**，命中判定提速 **11%**（摘要，`meancache.md:11`）。
- **自适应余弦阈值**：阈值经联邦学习协同计算、按用户查询模式个性化（`meancache.md:159-161`），而非全局固定值。
- **双层 LRU 驱逐**（`meancache.md:1462`）。
- **实验**：对比 GPTCache，F-score **+17%**、precision **+20%**，上下文查询上优势更大（摘要，`meancache.md:11`）。

**对 NexusFlow 的适用性**：
- **context chain 匹配适用性最高、成本最低**：OpenAI/Anthropic chat 请求的 `messages` 数组天然携带完整会话链，NexusFlow 协议层已解析 messages，把"末条 user 消息 embedding 相似"升级为"末条消息相似 + 历史消息链相似"即可，无需任何模型训练。
- PCA 降维 + 自适应阈值同样可直接用于 oxcache 侧。
- **不适用部分（如实说明）**：其"用户设备本地缓存 + 联邦学习训练相似度模型"的核心前提是端侧部署，NexusFlow 是服务端集中式网关，FL 部分不适用；可借用的是 context chain、PCA、自适应阈值三个机制。

---

## 其余检索到论文（仅基于标题+摘要，未下载全文）

| 标题 | 年份 | venue | 与 NexusFlow 的相关性 |
|------|------|-------|----------------------|
| GPT Semantic Cache: Reducing LLM Costs and Latency via Semantic Embedding Caching (arXiv:2411.05276) | 2024 | arXiv | 中：embedding 缓存 + 判定"走缓存还是打活查询"，是网关侧缓存的基本形态参考 |
| Continuous Semantic Caching for Low-Cost LLM Serving (arXiv:2604.20021) | 2026 | arXiv | 中：研究**缓存准入**——决定哪些查询值得进缓存以在预算内最大化收益；对 NexusFlow 可落地为"按请求重复频率准入"（SQLite 日志已可统计），但本笔记仅到摘要级 |
| An Ensemble Embedding Approach for Improving Semantic Caching Performance in LLM-based Systems (arXiv:2507.07061) | 2025 | arXiv | 中：单 embedding 模型鲁棒性不足，用多模型集成做相似度判定；与 LaCache 的 m 个索引函数集成思路同向 |
| From Similarity to Vulnerability: Key Collision Attack on LLM Semantic Caching (arXiv:2601.23088) | 2026 | arXiv | 中（攻击侧证据）：摘要称语义缓存已被 AWS/Microsoft 等大厂采用，且键碰撞攻击现实存在——为 LaCache 防御机制的必要性提供攻击侧佐证 |
| Behavioral Consistency and Transparency Analysis on Large Language Model API Gateways (arXiv:2604.21083) | 2026 | arXiv | 中高：直接研究第三方 LLM API 网关（与 NexusFlow 同类）——其内部路由/缓存/计费策略不透明导致用户侧行为不一致；提示 NexusFlow 应对外暴露确定性行为（如缓存命中是否影响响应、X-Vendor 头一致性） |
| AI-VERDE: A Gateway for Egalitarian Access to LLM-Based Resources (arXiv:2502.09651) | 2025 | arXiv | 中：同领域系统论文，统一接入商业/云/本地开源模型 + 访问管理；架构形态与 NexusFlow 类似，可作对照实现 |
| From Inference Routing to Agent Orchestration: Declarative Policy Compilation with Cross-Layer Verification (arXiv:2603.27299) | 2026 | arXiv | 中高：摘要称 Semantic Router DSL 已在生产环境做逐请求路由——内容信号（embedding 相似度、PII、越狱评分）经加权投影 + 优先级决策树选择模型；对 NexusFlow 的 3D 权重路由是"内容感知维度"的升级方向 |
| REQUEST ROUTING METHODS BETWEEN HETEROGENEOUS LLMs IN A MULTI-MODEL GATEWAY (DOI:10.36871/2618-9976.2026.05.011) | 2026 | SOFT MEASUREMENTS AND COMPUTING | 中：多模型网关内异构路由方法分类学（静态映射、级联 cascade、embedding 分类器等），可作路由策略选型的 checklist |
| ShadowServe: Interference-Free KV Cache Fetching for Distributed Prefix Caching (arXiv:2509.16857) | 2025 | arXiv | 低：分布式前缀缓存的 KV 拉取调度，属推理服务端（vLLM/SGLang 层）；NexusFlow 不执行推理，仅上游供应商若暴露 prompt-cache 计价差异时才有间接参考 |
| （第 4 轮其余：Adaptive KV Cache Reuse、SparseX、Tutti、Sparse Prefix Caching、CXL-SSDs KV 等） | 2025-2026 | arXiv | 低：均为 GPU/SSD 层 KV cache 管理，超出代理网关职责边界，不再展开 |

**多源检索中不相关结果（如实记录）**：`"LLM gateway routing efficiency" --source multi` 混入 AgroNova（温室 IoT）、SafeCodeRL（IoT 代码生成）、Kubernetes GenAI 基准、POKT 去中心化推理、药物研发 Model Gateway 等，已排除；另有一条 LLM4TS 记录元数据明显错配（摘要实为 L4S 路由器队列管理），不可引用。

---

## 对照 NexusFlow 的可落地优化（按优先级）

1. **语义缓存层 + P-CHR/ORR 选型调参**（来源：Closing the Operational Gap）——最高优先级。~33% 请求语义重复意味着直接的成本/延迟收益空间；oxcache 兄弟库已备，/admin/config 可热调 τ；用 SQLite 请求日志离线回放计算 P-CHR 曲线选 embedding 模型。
2. **多轮请求 context chain 命中判定**（来源：MeanCache）——协议层已解析 messages，改缓存判定逻辑即可，防多轮误命中，论文证据 F-score +17% / precision +20%。
3. **多租户缓存键隔离**（来源：LaCache related work 的 per-tenant key randomization 思路）——oxcache key 拼 API key 哈希，低成本堵住跨租户投毒；完整 lookahead 双阶段校验留作后续（需 draft 小模型，effort 高）。
4. **内容感知路由规则**（来源：From Inference Routing to Agent Orchestration）——在 3D 权重之外引入内容信号加权的决策树/规则，生产系统已验证该形态；与热更新配置契合，但改造路由核心 effort 高。
