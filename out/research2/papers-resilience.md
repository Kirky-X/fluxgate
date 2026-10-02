# 限流/拥塞/故障检测算法论文调研笔记（第二轮·算法专项）

- **日期**: 2026-10-01
- **方法**: liuxiang 检索脚本（`python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py`），共 13 次检索（arxiv 源 8 次 + multi 源 5 次）；按标题+摘要筛相关性，对 3 篇最相关论文抓取全文（arXiv HTML / MDPI 全文 / PDF 下载+PyMuPDF 提取）做机制级提炼。
- **防重复**: 先读取第一轮笔记 `out/research/papers-resilience.md` 及全部 `out/research/*.md`，汇总已覆盖论文标识清单（66 个 arXiv/DOI 标识）。第一轮已覆盖且本轮再次命中的 arXiv:2511.03279（DRL 限流）、arXiv:2510.04516（客户端侧 ATB/AATB 重试）**不重复报告**。
- **对照基线**（本轮实读代码确认）：
  - FallbackChain 仅做 target 选择（`src/vendor/resilience/fallback.rs:83-100`，priority 升档、排除已失败 vendor）；
  - fallback 循环复用同一 `unified_request` 全量透传（`src/api/mod.rs:114`，`execute_request_with_fallback`，`max_total_attempts` 为链级静态上限）；
  - 重试延迟：Retry-After **直接覆盖、不加 jitter**（`src/utils/retry.rs:231-233`）；5xx 固定 1s 无 jitter（`src/utils/retry.rs:242`）；429 无头走 `limiteron::retry::delay_for_attempt`（指数+jitter，`src/utils/retry.rs:235-241`）；
  - 会话亲和失败解绑已实现（`src/api/openai.rs:368`）。

---

## 一、检索记录（13 次，含失败项，如实记录）

| # | 查询词 | 源 | 结果 |
|---|--------|----|------|
| 1 | adaptive rate limiting algorithm 2025 | arxiv | 8 条；命中 1 条第一轮已覆盖论文（2511.03279），其余 7 条不相关 |
| 2 | congestion control API client retry | arxiv | 仅 1 条：2510.04516（第一轮已覆盖 ATB/AATB） |
| 3 | outage detection ejection algorithm | arxiv | **0 结果** |
| 4 | retry budget tail latency | arxiv | **0 结果** |
| 5 | hedged requests tail latency replication | arxiv | **0 结果** |
| 6 | failure detector gossip protocol | arxiv | 1 条（2609.35792，域不匹配，见次要论文） |
| 7 | LLM API failover provider outage | arxiv | **1 条强相关：ContinuityBench（2607.15899）** |
| 8 | outlier ejection load balancer unhealthy hosts | arxiv | **0 结果** |
| 9 | adaptive timeout failure detection Phi accrual | arxiv | **0 结果** |
| 10 | retry storm exponential backoff thundering herd | multi | DBLP 解析失败（已知降级）；OpenAlex/Crossref 16 条去重 8 条，命中 Self-DDoS、GitHub 可用性 2 篇相关 |
| 11 | overload control admission control inference serving | arxiv | 1 条边界相关（Fluid-Guided Scheduling，见次要论文） |
| 12 | retry budget client side throttling | multi | DBLP 失败；17 条去重 8 条，无直接相关（NIST SP 800-204A 作背景参考） |
| 13 | anomaly detection service health ejection microservices | multi | DBLP 失败；18 条去重 8 条，微服务异常检测 ML 论文若干（需训练设施，见次要论文） |

**覆盖面诚实声明**：13 次检索中 6 次 0 结果，4 次 DBLP 源解析失败（脚本已知降级行为）。**"故障检测/outlier ejection 经典方向"检索空洞**——Phi Accrual 故障检测器、hedged requests、retry budget、outlier ejection 等系统工程经典算法在 arXiv 上 5 个查询全部 0 结果或域不匹配；这类内容多发表于 NSDI/DSN/USENIX 或工程文档（如 Envoy outlier detection 文档），非本轮脚本源可覆盖。本轮实际收获集中在 **LLM 网关故障转移**（论文 1）与**重试风暴定量模型**（论文 2）两个子方向。

---

## 二、重点论文（已抓全文，机制级提炼）

### 论文 1. ContinuityBench: A Benchmark and Systems Study of Stateful Failover in Multi-Provider LLM Routing
- **年份/标识**: 2026（2026-07-17 提交，v1），arXiv:2607.15899（cs.LG，预印本，16 页）
- **核心问题**: 生产 LLM 部署中"高 API 可用性 ≠ 对话连续性"。主供应商 outage/严格限流时，无状态 failover（只转发最后一条用户消息）维持 uptime 但静默丢弃对话历史。
- **机制提炼（可照着实现）**：
  - **History-Forwarding**：failover 时转发完整对话 `failover_payload = list(messages)`；统一 Provider 接口归一化端点签名为 `chat(messages, model, temperature, max_tokens)`，Anthropic 适配器做格式翻译（系统提示提取、角色映射），使同一载荷可无损转发到任意后端。
  - **CPR 指标**：`CPR = (1/N) Σᵢ preserved(fᵢ)`；preserved ∈ {0,1} 由 LLM 判官对后备响应做二元事实核查（校验响应是否体现预注入对话中的事实锚点），报 95% Wilson 置信区间。
  - **CLO 指标**：`CLO = (1/N) Σᵢ (ℓᵢᵗʳᵉᵃᵗ − ℓᵢᵇᵃˢᵉ)`，故障转移与基线系统端到端延迟的成对差均值，报均值与 P95。
  - **失败模式一（并发竞态）**：C=5 时接近完美，C=100 时 CPR 崩至约 28%——共享全局历史缓存在异步任务交错下被并行改写，会话 A 载荷携带会话 B 锚点；修复为**每会话在异步让出前深拷贝隔离**。
  - **失败模式二（同步惊群）**：固定间隔重试（精确等 2.0s）使 100 个并发协程同步醒来轰击严格限流的后备（15 RPM）→ 429 → 同步休眠再同步重试 → 耗尽本地 socket、代理崩溃、CPR 归零。修复：`t_wait = min(30s, 2^a + U(0,1))`（a 为 0 起始尝试索引），最多 5 次重试，读超时 300s。
- **证据数字**（自述）：750 次 failover 事件（150 对话×5 运行，C=100，故障注入于末轮探针），Treatment CPR **99.20%**（Wilson 95% CI: 98.27–99.63）vs Baseline **0.00%**；CLO 均值 +59ms、中位 −450ms、P95 +13,614ms（9–11 轮大载荷对话）；按对话长度 7/9/11 轮 CPR 97.7%/100%/100%，无随长度退化。
- **对照 NexusFlow 基线 → gapType: improvement**：
  1. **History-Forwarding 本身已隐式具备**：`execute_request_with_fallback` 对同一 `unified_request` 做 `request.clone()` 全量透传（`src/api/mod.rs:114-121`），客户端的完整 messages 随 fallback 转发到下一 vendor——与论文 Treatment 等价，无需改造。本轮结论：**该机制不构成缺口，但论文提供了验证其有效性的量化方法**。
  2. **真实缺口 A：Retry-After 分支的同步惊群**。`retry_delay_for` 中 Retry-After 直接返回不加 jitter（`src/utils/retry.rs:231-233`），5xx 固定 1s 分支同理（`src/utils/retry.rs:242`）。当上游对一批并发请求返回同一固定 Retry-After，它们会在同一时刻醒来重试——正是论文失败模式二的触发条件。修复：`ra + U(0, ra)` 或 `ra × (1 + U(0, jitter))` 全范围抖动；改动 ~5 行。
  3. **真实缺口 B：fallback 连续性质量度量缺失**。GlobalMetrics 只有请求数/延迟/错误率，无法回答"fallback 后响应是否保持了上下文一致性"。CPR 可降级采用：离线抽样评测（定期取 fallback 日志样本，人工或 LLM 判官核查），不必在线运行 LLM 判官。CLO 可直接实现：fallback 成功请求打标，按 primary/fallback 分组对比延迟分布（现有 SQLite 日志已含所需字段）。
- **适用性**：高。缺口 A 是低成本高确定性修复；缺口 B 属可观测性补全。论文自身局限如实注明：合成数据、GPT-4o 判官潜在偏差、仅 OpenAI→Anthropic 单链路、纯文本非流式端点、每会话单次故障注入；NexusFlow 恰好是双协议网关，其"Anthropic 适配器格式翻译"环节 NexusFlow 已有（protocol/ 模块）。
- **证据**: https://arxiv.org/abs/2607.15899 （全文 HTML 已抓取）

### 论文 2. Trigger, Not Root Cause: Re-Examining GitHub's 2024 to 2026 Availability Decline Under Artificial Intelligence-Driven Load
- **年份/标识**: 2026，DOI:10.3390/fi18070377，*Future Internet* 18(7):377（MDPI，开放获取）
- **核心论点**: GitHub 两年可用性下降的触发器是 AI coding agent 造成的机器速度负载，但**根因是 metastable failure（亚稳态故障）**——触发消除后系统仍自我维持失败状态。区分"触发 vs 根因"是弹性设计的前提：只修触发器（限流 agent）不消除亚稳态，必须压制重试放大回路。
- **机制提炼（可照着实现）——重试放大定量模型**：
  - **有效到达率**：`λ_eff(f) = λ / (1 − r·f)`，成立条件 `r·f < 1`。λ = 原始请求到达率；r = 重试预算（平均每个失败请求产生的重试次数）；f = 失败率。物理含义：每秒 C 个失败请求产生 C·r 个重试，重试也按失败率 f 再失败，几何级数放大。
  - **亚稳态临界点**：令 `λ_eff = C`（服务容量）解出临界失败率 `f_c = (1 − λ/C) / r = (C − λ) / (r·C)`。f < f_c 时系统收敛（重试流量可被吸收）；f ≥ f_c 时进入亚稳态——即使原始触发（部署事故/上游抖动）消失，重试流量本身足以维持饱和。
  - **守门决策式**（NexusFlow 落地形态）：每窗口（10~30s，与现有环形滑窗对齐）估计 f（vendor 级 EWMA 错误率，已有）、λ（入站速率，60s/10 桶滑窗已有）、r（重试请求数 ÷ 失败请求数，需新增两个计数器）；计算 `amplification = 1/(1 − r·f)` 与 `f_c`。当 `f > θ·f_c`（θ≈0.8，留安全边际）时：收紧该路径 `max_total_attempts`（压 r，把放大因子拉回 1 以下）或对该 vendor 触发熔断/shed；r·f → 1 时 amplification 发散，必须告警。
  - **准入控制/负载卸载规则集**：按客户端身份与优先级分类；token bucket + 429 + Retry-After（NexusFlow GCRA 已有前半）；高水位时优先卸载最低优先级类，**永不卸载健康检查**。
  - **QoS tiering**：Tier1 交互式流量 > Tier2 关键自动化 > Tier3 批量/投机性 agent 流量；优先级调度器 + 高水位 429。
  - **Shuffle sharding**：N 租户映射到 k 个隔离 cell，形成 C(N,k) 个 shard，使单点故障/坏租户的影响面以碰撞概率为上界（AWS 隔离模式的形式化）。
- **证据数字**（自述）：对 GitHub 2024–2026 公开事故报告的纵向分析；AI agent 流量放大重试回路导致 availability 持续下降，2026 年中转急。
- **对照 NexusFlow 基线 → gapType: missing**：
  - r（重试预算）无估计、放大率无监控、f_c 无告警——三类信号全部缺失；但**全部输入信号在现有代码中可获得**：EWMA 错误率已有、环形滑窗已有、重试路径集中（`src/utils/retry.rs` + `src/vendor/resilience/`），只需加重试计数器与一个窗口内纯计算守门器。无训练、无集群设施，纯推理时计算，适配单二进制形态。
  - QoS tiering / 优先级 shed 缺失：现有"每日时段门控"是时间维度门控，不是请求优先级维度。对 LLM 网关的映射：交互式短请求 vs 批量长请求可按 max_tokens/流式标记粗分级（内容 tier 路由已暂缓，此处只需 2~3 档粗分类用于 shed 顺序，不等价于那个暂缓项）。
  - Shuffle sharding：适用于多租户熔断隔离（避免单个坏租户的重试行为拖垮全租户共享的熔断/限流状态）；NexusFlow 已有租户隔离缓存，但限流/熔断是全局共享。落地代价中-高（需重构 limiteron 状态的租户分片），列为可选。
- **适用性**：**高（本轮最有算法价值的发现）**。重试放大守门器是"用已有信号 + 两个新计数器 + 一个公式"即可落地的亚稳态防护，且与已实施的"重试-熔断联动"（第一轮建议 3）互补：联动解决"单 vendor 坏了别白打"，守门器解决"全系统重试自我供给时主动降 r"。预期收益：防止上游抖动演化为网关自身亚稳态（论文 1 失败模式二即网关侧亚稳态实例——socket 耗尽崩溃）。
- **证据**: https://www.mdpi.com/1999-5903/18/7/377 （全文已抓取；公式 λ_eff、f_c 见其重试放大章节）

### 论文 3. Self-Inflicted Denial of Service (Self-DDoS) in Distributed Systems: An Analysis of Retry Storms, Thundering Herds, and Cascading Failure
- **年份/标识**: 2026，DOI:10.68218/2ddp5705，*Computational and Applied Science* 1(2):141-148（Georgia，开放获取；作者 Business and Technology University / Georgian Technical University）
- **核心论点**: 把 retry storm、thundering herd、cascading failure、autoscaling 反馈回路、cache stampede 统一为"自我施加的负载放大"一类，逐个配对缓解模式。
- **机制提炼**：
  - **五机制 × 缓解对照表**：retry storm → 指数退避+jitter；cascading failure → circuit breaker + bulkhead；retry storm/thundering herd → 限流/token bucket；全部（最后防线）→ load shedding（按优先级选择性丢弃低优请求）；cache stampede → request coalescing/single-flight（并发重复请求合并为一个回源）。
  - **受控故障注入实验**：K8s 三节点集群 + LitmusChaos 注入 400ms 网络延迟（60s），20 个合成客户端，两种重试配置对比：固定 1s 间隔 → 到达下游的请求率峰值约 **3.1×** 稳态基线（每个 1s 边界同步尖峰）；指数退避+jitter（base 200ms、factor 2、±50ms jitter、cap 4s）→ 峰值约 **1.4×** 基线。作者自述这是"说明性 worked example，非统计功效研究"。
- **对照 NexusFlow 基线 → gapType: improvement**：
  - 对照表逐项核验：退避+jitter ✅、熔断 ✅（limiteron）、bulkhead ✅（并发票据随流存活）、限流 ✅（GCRA + 固定窗口）；**缺口 1：load shedding 无**（与论文 2 tiering 合并实施）；**缺口 2：single-flight 无**——精确响应缓存（canonical JSON 键）没有 in-flight 合并，同租户并发同 key 请求会各自回源（对 LLM 网关收益限于非流式+完全相同请求的场景，且要小心不合并流式请求，收益有限、列为可选）。
  - 实验数字的直接启示：NexusFlow 已有的 jitter=0.2 比例抖动方向正确；真正危险的是论文 1/3 共同指出的**无抖动分支**（Retry-After 覆盖、5xx 固定 1s）。
- **适用性**：中。主要价值是提供"机制×缓解"完整性核验清单 + 固定间隔 vs 抖动退避的对照数字（3.1× vs 1.4×）作定性佐证。证据等级偏低（工程期刊、作者自认非统计功效研究、单一故障类型小集群），不据此单独立项，其缺口与论文 1/2 的建议合并。
- **证据**: https://doi.org/10.68218/2ddp5705 （PDF 全文已提取，8 页）

---

## 三、次要论文（标题+摘要级评估，不立项）

### 4. Serverless gossip training of LSTM failure detectors (arXiv:2609.35792, 2026)
NASA C-MAPSS 涡轮机数据的设备预测性维护：gossip 学习训练 LSTM 故障检测器。**域不匹配**——这是设备故障预测，不是服务健康检测；且需训练管线，与单二进制网关不符。仅记录，说明"故障检测"检索在该脚本源上的现状。

### 5. Optimizing LLM Inference: Fluid-Guided Online Scheduling with Memory Constraints (arXiv:2504.11320, 2025)
Fluid 模型指导的 LLM 推理 GPU 在线调度（内存约束、token-by-token 内存增长）。属 serving 内部调度（同第一轮 CascadeInfer 的粒度差异），网关不可直接引入，仅记录。

### 6. 微服务异常检测 ML 方向（multi 源命中）
- Unsupervised Anomaly Detection in Cloud-Native Microservices via Cross-Service Temporal Contrastive Learning（DOI:10.20944/preprints202512.2703.v1, 2025）
- Graph Neural Network-Based Anomaly Detection for Trace of Microservices（DOI:10.2139/ssrn.4111928, 2022）

两者均需全链路 trace 数据 + 模型训练设施，对单二进制推理时网关落地代价不成比例；NexusFlow 若未来积累 enough 日志可重访。仅记录为该方向学术现状。

### 7. NIST SP 800-204A: Building secure microservices applications using service-mesh architecture（DOI:10.6028/nist.sp.800-204a, 2020）
标准文档非论文；含服务网格弹性/安全配置建议，作背景参考，不据此改造。

### 8. Building Resilient Systems: Error Handling, Retry Mechanisms, and Predictive Analytics in Event-Driven Architecture（DOI:10.32996/jcsts.2025.7.7.34, 2025）
EDA 错误处理/重试/预测分析综述性短文，证据等级低，仅记录。

---

## 四、对 NexusFlow 的落地优化建议（按优先级，含对照第一轮）

1. **重试放大守门器（亚稳态防护）** — 依据论文 2。新增 2 个计数器（vendor 级重试数/失败数）→ 每 10~30s 计算 r、amplification=1/(1−r·f)、f_c=(C−λ)/(rC)；f > 0.8·f_c 时收紧 max_total_attempts 或提前熔断，r·f→1 告警。输入信号全部已有，纯推理时计算。工作量：中。位置：`src/utils/metrics.rs` + `src/vendor/resilience/`。
2. **Retry-After 与固定间隔分支加抖动（消除同步惊群）** — 依据论文 1 失败模式二 + 论文 3（3.1× vs 1.4×）。`retry_delay_for` 的 Retry-After 分支（`src/utils/retry.rs:231-233`）与 5xx 固定 1s 分支（`src/utils/retry.rs:242`）叠加全范围抖动 `ra + U(0, ra)` / `1s + U(0, 1s)`。工作量：低（~5 行+单测）。
3. **fallback 连续性度量（CPR/CLO 降级版）** — 依据论文 1。CLO：fallback 成功请求打标、按 primary/fallback 分组对比延迟（现有 SQLite 日志字段够用）；CPR：离线抽样 + LLM 判官或人工核查。工作量：中。
4. **优先级 shed（Tier1 交互 > Tier3 批量）** — 依据论文 2 tiering + 论文 3 load shedding。按流式标记/max_tokens 粗分 2~3 档，高水位时按档卸载（429 + Retry-After），永不卸载健康检查。与内容 tier 路由暂缓项无关（仅 shed 顺序，不做路由）。工作量：中。可与建议 1 同一窗口控制器实现。
5. **single-flight 请求合并（可选）** — 依据论文 3 对照表。仅非流式+同 canonical key 的并发请求合并回源。收益场景有限，列为可选。

**与第一轮的关系**：第一轮建议 1（拥塞感知 429 重试）、3（重试-熔断联动）与本轮建议 1（守门器）、2（去同步抖动）正交互补——前者管"单次重试打不打/怎么打"，本轮管"全体重试的总量与同步性"。第一轮已覆盖的 ATB/AATB（2510.04516）本轮再次命中未重复报告。

## 五、诚实性备注

- 所有论文均来自本轮脚本真实输出（标题/年份/标识以脚本与抓取页面为准）；6 次查询 0 结果、4 次 DBLP 解析失败均如实记录于第一节。
- 论文 1（arXiv HTML）、论文 2（MDPI 全文）、论文 3（PDF 下载 + PyMuPDF 文本提取）为全文级提炼；其余为标题+摘要级。
- 证据等级声明：论文 1 为未经同行评审的预印本（750 事件、合成数据、LLM 判官）；论文 2 为 MDPI 期刊（对公开事故报告的纵向分析，非受控实验）；论文 3 为低知名度工程期刊且作者自述实验"非统计功效研究"。引用其数字时均注明为原论文环境结果，不外推为 NexusFlow 收益。
- 故障检测经典算法（Phi Accrual、hedged requests、outlier ejection）在本脚本覆盖的源中检索空洞，未找到可引入的新算法，如实说明而非凑数。
- 本轮未运行任何 NexusFlow 测试/构建（调研任务，无代码改动）；代码基线事实均来自实读源码文件并标注行号。
