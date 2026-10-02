# 研究性路由实现算法专项笔记（第 2 轮）——RouteLLM / Semantic Router / FrugalGPT / vllm-sr

- **调研日期**：2026-10-01
- **轮次定位**：算法专项第 2 轮。第 1 轮（`out/research/`、`out/research/projects-researchrouter.md`）已做**功能清单级**调研；本轮下沉到**源码级算法机制**——每个机制的公式、窗口、阈值、采样全写清。第 1 轮笔记里多处自述"矩阵分解/分类器变体细节来自摘要未列全，落地前应读源码"，本轮补齐。
- **取证方式（重要）**：本机 `raw.githubusercontent.com` TLS 被 reset（`curl -v` 实测 `Recv failure: Connection reset by peer`），WebFetch 对 raw 亦 `ECONNRESET`，匿名 `api.github.com` 触发共享 IP 限流（`API rate limit exceeded for 111.29.179.185`）。最终改走 **`gh` CLI 认证通道**（`gh api ... --jq .content | base64 -d`），全部源码为**本轮真实下载并逐行 Read**。下文 `file:line` 指下载到 `/tmp/routellm_study/` 的文件，行号与上游 main 分支一致。
- **第一轮去重**：已 grep 第一轮 `projects-*.md`/`papers-*.md`，确认级联（FrugalGPT 源码级判据）、MF/BERT/sw_ranking 的**算法公式**、Semantic Router 的 top-k 聚合与阈值拟合、APRR 指标均**未在第一轮展开**（第一轮仅列功能名与一句话机制）。本轮不重复第一轮已报的"阈值放请求 model 字段"（第 1 轮建议 #1）、"每请求路由策略"等结论，只补算法层。
- **未取到的证据（如实声明）**：`WebSearch` 工具在本模型不可用（返回 `Current model does not support native WebSearch`），故本轮**全部结论来自 GitHub 源码**（gh api 真实下载），未引用任何需 WebSearch 才能证实的博客/文档数字。RouteLLM 论文正文的 baseline 对比、Cascade-EmbedRM 等论文专属变体**未读原文**，本轮只报仓库里**有源码可查**的机制。

---

## 一、RouteLLM（lm-sys/RouteLLM）——源码级算法

四类路由器 + 随机基线实现在 `routellm/routers/routers.py`（263 行，本轮完整读取）。共同抽象：`Router.calculate_strong_win_rate(prompt) → [0,1]`，`route()` 与固定 `threshold` 比较（`routers.py:41-45`）：

```python
def route(self, prompt, threshold, routed_pair):
    if self.calculate_strong_win_rate(prompt) >= threshold:
        return routed_pair.strong
    else:
        return routed_pair.weak
```

即**所有打分器都归约到一个标量 win-rate**，阈值是唯一的决策旋钮。

### 1.1 MF 矩阵分解打分器（Bradley-Terry 双向，官方推荐）

`routellm/routers/matrix_factorization/model.py`（129 行）：
- 每个模型一个可学习 embedding `P ∈ R^{64×128}`（`model.py:86`，64 个模型对应 `MODEL_IDS` 表，`model.py:6-71`）；L2 归一化（`model.py:110`）。
- prompt 走 OpenAI `text-embedding-3-small`（1536 维）→ 线性投影 `text_proj: 1536→128`（无 bias，`model.py:91-93`）。
- 打分：`logit_m = classifier(P_m ⊙ proj(e_p))`（逐元素积 + 无 bias 线性，`model.py:120`），其中 ⊙ 是逐元素乘、classifier 权重 `w`。
- **win-rate = sigmoid(logit_strong − logit_weak)**（`model.py:122-126`）。

训练（`train_matrix_factorization.py`，253 行）：
- 构造 `(P_win − P_loss) ⊙ e_p` 送入分类器得标量 logit，用 `BCEWithLogitsLoss` 以标签恒为 1 拟合（`train:78-96, 144-156`）——本质是把 Bradley-Terry 成对似然写成二分类。
- **训练稳定技巧：prompt embedding 加高斯噪声** `e_p += randn * alpha`（alpha=0.1，仅训练期，`train:88-90`）——防过拟合嵌入记忆。
- 超参：dim=128, batch=64, epochs=100, lr=3e-4, wd=1e-5, 训练/测试 95/5（`train:207-234`）；数据=Chatbot Arena 对战（filter winner∈{a,b} 且 a≠b）。
- 泛化设计：推理时打分模型对可与训练对不同（`routers.py:139-142` 注释），测试时换强/弱模型无需重训。

### 1.2 sw_ranking：相似度加权在线 Elo（无需训练，最适合 Rust）

`routers/similarity_weighted/utils.py`（112 行）+ `routers.py:133-206`：
- 底座是 **Elo 的 LogisticRegression-MLE 实现**（不是在线递推 Elo）：每场对战复制两份、特征是 ±log(10) 的 one-hot 差分，tie 记两次（`utils.py:60-77`），`LogisticRegression(fit_intercept=False, penalty=None)` 得系数，`elo = 400×coef + 1000`（`utils.py:79-90`），并把 `llama-2-70b-chat` 锚定到 1082 做尺度校准（`utils.py:88-89`）。
- **tier 化（泛化关键）**：把模型按 Elo 排成 `num_tiers=10` 个档，档内共享分数——用**动态规划最小化段内方差**切分（`utils.py:14-51`），新模型映射到某 tier 即可打分。
- **在线相似度加权**：把每条历史对战的首轮嵌入与当前 prompt 嵌入做余弦（`routers.py:190-193`），权重 `w_i = 10 × 10^(sim_i / max_sim)`（`routers.py:173-175`，指数加权、最相似的权重最大 100），用 `sample_weight` 重算加权 Elo（`utils.py:80-82`）。
- 胜率由 Elo 差换算：`weak_wr = 1/(1+10^((S_weak−S_strong)/400))`（`routers.py:202`）。

### 1.3 BERT 序列分类打分器（3 类：weak/tie/strong）

`routers.py:106-130`：`AutoModelForSequenceClassification`，`num_labels=3`；softmax 后 `binary_prob = Σ softmax[-2:]`（即 P(弱不胜：tie 或强胜)），返回 `win_rate = 1 − binary_prob = softmax[0] = P(弱胜)`。把"弱/平/强"三分类折叠成二元决策。

### 1.4 causal_llm：LLM 5-token 打分器（保守回退）

`routellm/routers/causal_llm/model.py`（151 行）：Llama-3-8B 加 5 个特殊 token `[[1]]…[[5]]` 表示"弱模型能打好这个题"的评分（`model.py:26-27`）；贪心生成（`do_sample=False`，`model.py:90`），取第一个特殊 token 位置的 logits，只对 5 个特殊 token 做 softmax（`model.py:111-126`）；`binary_prob = Σ softmax[threshold−1:]`，threshold=4 即 P(score≥4)（`model.py:29-30, 122-127`）。**输出无有效特殊 token 时返回 None，网关层回退 win_rate=1 → 走强模型**（`routers.py:98-102`）——保守兜底。（可行性：每请求跑 8B 前向，对单二进制 Rust 网关代价过高。）

### 1.5 阈值分位数校准（把抽象阈值变成流量比例）

`routellm/calibrate_threshold.py:50-58`：先用路由批量算历史 prompt 的 win_rate 序列，再取 `threshold = win_rates.quantile(1 − strong_pct)`。即"恰好让 strong_pct 比例流量走强模型"的阈值。

### 1.6 APRR 归一化 AUC（路由质量评估指标）

`routellm/evals/evaluate.py:101-114`：`APRR = (router_AUC − 全弱AUC) / (全强AUC − 全弱AUC)`，横轴=强模型调用比例、纵轴=准确率，用 `np.trapz` 积 AUC。把"质量-成本"曲线归一化到 [0,1]：0=全走弱模型，1=全走强模型，中间值=路由器的成本-质量权衡效率。

---

## 二、Semantic Router（aurelio-labs/semantic-router）——嵌入最近邻决策链

### 2.1 1.x 决策链（`main` 分支）
- 查询→嵌入（queries/document 非对称编码，`semantic.py:42-66`）→ 本地索引余弦 top-k（`index/local.py:168-206`；`linear.py:12-28` 余弦 `dot/(|i||q|)`，`linear.py:31-49` `argpartition` 取 top-k，默认 k=5）。
- **组内聚合**：top-k 按 route 名分组 → 每组取聚合值（mean/max/sum，`base.py:1521-1541`；默认 mean，`base.py:419-424`）→ 按聚合分排序。
- **阈值判定**：逐 route 比较 `route.score_threshold`（route 级优先）否则全局 `encoder.score_threshold`；无阈值恒过；全不过返回**空 RouteChoice（显式拒识）**（`base.py:645-654, 700-701`）。

### 2.2 0.x fit：阈值网格随机搜索（`v0.1.16`，`base.py:1683-1861`）
- `fit(X,y,batch=500,max_iter=500)`：先用当前阈值评估一次得 best_acc；每轮调 `threshold_random_search(search_range=0.8)`——为每条 route 在 `[thr−0.8, thr+0.8]∩[0,1]` 上 `np.linspace(num=100)` 建 100 点候选，每条 route **独立随机抽 1 个阈值**，更新后按 accuracy 保留最优（`base_0x.py:1737-1753, 1829-1861`）。朴素随机搜索（无坐标下降/无交叉），500 轮。文档示例输出阈值均为 1/99 倍数，与 100 点网格一致（`threshold_opt.md:262`）。

---

## 三、其他研究性路由实现

### 3.1 FrugalGPT（stanford-futuredata/FrugalGPT）——级联判据 + 预算约束阈值优化
`src/FrugalGPT/`（`llmcascade.py` 298 行 / `llmchain.py` 211 / `optimizer.py` 135，均本轮读取）。
- **reactive 级联判据**：策略是一条 (model, thres) 链，逐级弱→强调用弱模型拿到 (query, response)，用**该模型专属 scorer** 打质量分 `score`，`score > 1 − thres` 即接受当前响应跳出，否则升级下一级（`llmcascade.py:121-142`；等价 `predict_one`：`dist=1−score`，`dist<thres` 即接受，`llmchain.py:180-194`）。
- **per-model scorer**：DistilBert 对 `(query + answer)` 回归/分类打分器，逐模型训练（`llmcascade.py:186-199`；`score_type='DistilBert'`）。
- **预算约束的阈值优化（成本-质量联合优化核心）**（`optimizer.py:58-135`）：
  - 优化变量是**分位数 qual** 而非阈值本身：逐级在"上一级未接受"的样本子集上条件化算阈值 `thres[i] = quantile(d[:,i] | 上级未接受, 1−qual[i])`（`optimizer.py:95-112`），末级固定 `thres=1` 兜底（`optimizer.py:106`）。
  - 约束：`qual` 单调不减（`optimizer.py:86-89`）；成本 `Σ e·C ≤ budget×n`，超预算返回惩罚 `10000`（`optimizer.py:82-84`）。
  - 目标：`max Σ e·L`（`e`=首次达标即接受的阶梯矩阵，`optimizer.py:67-84`）。
  - 求解：`scipy.optimize.brute`（40 点/维网格）→ `fmin`（Nelder-Mead）精修（`optimizer.py:124-131`）。
  - **链选择**：`LLMChain.train` 枚举所有模型排列 `itertools.permutations`，每条链解一次优化，取最高 accuracy（`llmchain.py:107-143`）。
- 与 RouteLLM 的本质差异：RouteLLM 是 **predictive**（提前判难度，二元切换）；FrugalGPT 是 **reactive**（先试弱模型、看输出质量再决定升不升级），阈值由预算约束优化而非人工设定。

### 3.2 vllm-project/semantic-router（Rust/candle 实现）——MLP argmax 多模型选择器
- `candle-binding/src/classifiers/mlp_selector.rs`（536 行）：查询特征向量 → 多层 `Linear(+ReLU)` 前向 → **argmax 直接输出模型名**（`select()` `mlp_selector.rs:181-241`，`forward` `244+`）。输出层每神经元对应一个模型，一次前向完成 N 模型选择（非二元）。
- 对 NexusFlow 的特殊价值：**Rust 实现**（candle 框架可嵌单二进制），比 RouteLLM 的 MF/BERT（PyTorch + OpenAI 嵌入 API）更贴近单二进制 Rust 网关。其 `modernbert_classifier.rs` 多为占位结构（`forward` 返回 zeros，`modernbert.rs:23-31`），非核心算法，不采信。
- 决策层是**声明式规则引擎**（`config/fragments/decision/composite/priority-safe-escalation.yaml`：多信号 AND/OR/NOT 组合；`signal/complexity/escalation.yaml`：阈值 0.10 + 关键词），非学习算法，作为规则路由佐证不单列 finding。

---

## 四、与 NexusFlow 基线对照结论（只列 missing/improvement）

基线已有（不重报）：多维加权随机（weight/queue/window + EWMA + 错误率）、温度平滑、P2C、会话亲和、FallbackChain、熔断/限流/GCRA、SSE、精确响应缓存、环形滑窗熵告警、配置热更。**基线路由全是负载侧信号，无任何"内容/质量/成本"信号**——故本轮学习/质量路由算法基本全是 missing。

| # | 算法 | gapType | 落点 |
|---|------|---------|------|
| 1 | Win-rate 阈值二元路由框架 | missing | 路由前插"难度判别"层 |
| 2 | MF 矩阵分解打分（Bradley-Terry 双向） | missing | 内容选池打分器 |
| 3 | sw_ranking 相似度加权在线 Elo（无训练，纯 numpy） | missing | 最易落地的 win-rate 打分器 |
| 4 | BERT 3 类折叠二元打分 | missing | 内容打分器 |
| 5 | causal_llm 5-token 打分 + 无效保守回退 | missing | 可行性低 |
| 6 | 阈值分位数校准（win_rate→流量比例） | missing | 把静态权重变成可校准参数 |
| 7 | APRR 归一化 AUC（路由质量评估） | missing | 评估层 |
| 8 | Semantic Router top-k 组内聚合 + 每route阈值 + 显式拒识 | missing | 第 4 维（内容）信号 |
| 9 | fit 阈值网格随机搜索 | missing | 阈值离线自动定 |
| 10 | vllm-sr Rust MLP argmax 多模型选择器 | missing | Rust 生态最贴合 |
| 11 | FrugalGPT reactive 级联判据（per-model scorer + 达标即接受） | missing | 质量驱动升级（区别于错误驱动 fallback） |
| 12 | FrugalGPT 分位数参数化 + 预算约束阈值优化 | missing | 成本-质量联合优化决策式 |

**improvement 类**：本轮源码精读中，最贴近"已有算法更优变体"的可落地项是 #3（sw_ranking 纯算法替换/增强现有随机路由的打分器）与 #6（把现有静态 `model.weight` 变成可分位数校准的显式参数），其余因基线完全无内容/质量信号而只能记 missing。

---

## 五、本轮实际下载的证据文件（gh api 认证通道，base64 解码后本地 Read）

RouteLLM（lm-sys/RouteLLM, main）：
- `routellm/routers/routers.py`（263 行）、`routellm/routers/matrix_factorization/model.py`（129）、`train_matrix_factorization.py`（253）、`routellm/routers/similarity_weighted/utils.py`（112）、`routellm/routers/causal_llm/model.py`（151）、`routellm/controller.py`（170）、`routellm/calibrate_threshold.py`（58）、`routellm/evals/evaluate.py`（272）
- 对应 raw URL：`https://raw.githubusercontent.com/lm-sys/RouteLLM/main/<path>`

Semantic Router（aurelio-labs/semantic-router）：
- main：`semantic_router/routers/semantic.py`（178）、`semantic_router/routers/base.py`（1859）、`semantic_router/index/local.py`（338）、`semantic_router/linear.py`（39）、`docs/user-guide/features/threshold-optimization.md`（v0.1.16）、`semantic_router/routers/base.py`（v0.1.16, fit+threshold_random_search）

FrugalGPT（stanford-futuredata/FrugalGPT）：
- `src/FrugalGPT/llmcascade.py`（298）、`llmchain.py`（211）、`optimizer.py`（135）

vllm-project/semantic-router：
- `candle-binding/src/classifiers/mlp_selector.rs`（536）、`traditional/modernbert_classifier.rs`（402，占位未采信）、`config/fragments/decision/composite/priority-safe-escalation.yaml`、`config/fragments/signal/complexity/escalation.yaml`

**诚实边界**：全部算法机制均来自上述本轮真实下载并 Read 的源码行；未读 RouteLLM/FrugalGPT 论文正文，未运行任何模型（无训练/推理验证），所有性能数字（如 RouteLLM "省 85%"）未在本轮复现、未在 finding 中引用。`WebSearch` 工具本轮不可用，`WebFetch` 对 raw/githubusercontent 均连接被拒，故无一条结论依赖网页搜索。