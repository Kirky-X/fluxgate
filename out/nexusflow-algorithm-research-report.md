# NexusFlow 算法专项调研报告（第二轮）

**日期**：2026-10-01 · **范围**：算法专项（路由/LB、弹性、缓存、调度限流），在第一轮 16 条建议全部实施后的新基线上进行
**调研笔记**：`out/research2/`（8 篇主题笔记 + 10 份来源核实记录 `source-check-id*.md`）
**本轮运行检查**：`cargo test --offline --lib` → **111 passed / 0 failed**（本报告撰写会话复跑通过）；`cargo check --offline` 通过（调研会话运行，撰写会话未单独重跑）；两个临时探针测试（slow-call 误触发、Config 字段丢弃）调研会话运行后已删除、无法重放，其结论由源码机制静态支撑（详见附录 6.3）

---

## 1. 执行摘要

主流网关弹性判定无统一定式：LiteLLM/Portkey 用「失败率比例 + 最小样本」，Higress 用连续计数 + 探活，Bifrost 企业版只看响应头——但「计数式且无最小样本」只有 NexusFlow 在用，且 limiteron 熔断默认 slow-call 阈值（500ms/50%）会把正常延迟的 LLM 响应误判为 Open（探针实测与独立复现逐位一致）。前沿沿三个方向推进：EWMA 滞后观测正被排队论前瞻估计替换（LPS 模型在过载逼近时发散，这一惩罚是 EWMA 结构上给不出的）；重试被建模为亚稳态放大回路（λ_eff=λ/(1−r·f)，须压重试率而非只修触发器）；缓存 TTL/淘汰与负载因子联动。在线学习路由的门槛比常被引述的低：SLARouter 零模型版只需现有错误率信号加两个标量，真正依赖 (query,model,quality,cost) 日志的只有 BaRP 一类离线训练路线。NexusFlow 的路由骨架（5 因子加权 + 温度平滑 + Po2C + 熵告警）在开源网关中属第一梯队（Portkey 开源版 LB 严格弱于它，Bifrost 加权随机是静态的），但信号采集层硬伤（延迟 EWMA 三口径混流、负载只看排队不看在途、w_latency=0 未启用）、弹性只覆盖 4 条执行路径中的 1 条、入站限流配置整段被 serde 静默丢弃三点拉低实际水平。最有价值的前 3 条：**P0-1**（一行配置消除熔断误触发）、**P0-2**（弹性覆盖 1/4→4/4）、**P0-3**（入站限流真实落地）；P0-13 与 P0-6 是性价比最高的两个低 effort 修复。

## 2. 主流项目算法版图

### 2.1 弹性层（熔断/重试）
- **LiteLLM**（`cooldown_handlers.py:376-416`）：失败率 pct>0.5 **且** 窗口样本 total≥5 **且** 非单部署组才冷却；429 立即冷却；单部署组用 total≥1000 的 100% 门槛防误杀；判定异常 fail-safe 到冷却。另有按异常类型分桶的 allowed_fails_policy（计数器 TTL=冷却时长）——分桶为 LiteLLM 独有（对 Higress/Bifrost 间接核实）。
- **Portkey Gateway**（`circuitBreaker.ts:135-164`，2.0.0 分支）：failure_count≥threshold **或** failure_pct≥threshold_percentage 二选一，样本 <minimum_requests(5) 时百分比按 0 计；全文无延迟启发式。重试有 60s 总时间预算（`globals.ts:5`），Retry-After 超预算即放弃（`retryHandler.ts:129-138` 递减式），支持 retry-after-ms/x-ms-retry-after-ms/retry-after 多头解析（:108-151）。
- **Higress** failover：failureThreshold=3 连续计数 + 探活恢复，无比例与最小样本。
- **Bifrost 企业版**：基于上游 HTTP 响应头触发，文档原文 "Trips on HTTP response headers returned by the provider — no latency heuristics or error-rate windows to tune"。
- **OpenRouter**：pre-stream 失败静默换 provider，首字节送达后 failover 停止。
- 四家三种形态，无统一定式；但「计数式且无最小样本」只有 NexusFlow 在用。

### 2.2 路由 / LB 负载信号
- **least-busy 家族统一用「在途请求数」而非排队深度**：LiteLLM least_busy（pre-call +1 / success/failure −1、0 下限钳位、Redis 失败降级本地，`least_busy.py:122-151/:193-215`）；Kong (inflight+1)/weight（`least_connections.lua:27-43`，+1 理由有注释）；Higress global_least_request（严格最小 + 平票蓄水池抽样，计数随 SSE 长流存活，`lb_policy.go:31-96/:284-293`）。
- **延迟信号分口径**：LiteLLM 流式记 TTFT、非流式 per-token 归一化，失败写 1000s 惩罚样本，min+buffer 候选集内均匀随机（`lowest_latency.py:110-130/:193-228/:482-494`）；Kong Peak-EWMA 时间常数衰减 exp(−td/10s)（`latency.lua:34-41`）；vLLM production-stack prefill/decoding 三阶段状态机，TTFT 严格口径是首 token（`request_stats.py:144-249`）。
- **亲和软化**：Bifrost 会话键在「身份∥session」之下 + SetNXWithTTL 收敛并行首绑定（`sessionaffinity.go`）；production-stack score = 亲和收益 − β·relative_load（**β=0 是纯缓存收益 argmax，并非硬粘**；`routing_logic.py:481-491`）。
- **权重与冷却**：LiteLLM 四级权重回退链（请求级 → weight → rpm → tpm → 均匀兜底，`simple_shuffle.py:43-67`）；fail-open 仅健康检查驱动的冷却可绕过（`router.py:13067-13074`）；冷却条目存显式剩余时间（`cooldown_cache.py`）。

### 2.3 入站限流
Kong：多 limit 多窗口叠加 + partition_by 按模型分桶 + consumer/IP/header/path/provider 匹配维度，sliding 为默认，calendar 窗口按 IANA 时区（period 仅 month/week）。Cloudflare：fixed vs sliding 对外语义（12:09+12:11 各十次共 20 次全放行 vs 滑动拒后 10 次）。

### 2.4 缓存
Bifrost 语义缓存惰性过期 + 异步物理删除（`search.go:248-271`）。论文侧新证据（§3.5）：语义负载下 LRU/LFU 一致差于朴素 FIFO——语义键场景下 Moka TinyLFU 方向可疑。

## 3. 前沿论文算法

1. **排队论延迟前瞻**：Beyond Accuracy and Cost（arXiv:2607.18253, 2026）§4.2 LPS 模型 Ŵ=(α/μ)^k/(μ−α)，ρ→1 时发散；式 3 硬约束 + 空集规则、式 5 拉格朗日软约束。配套 JITServe（arXiv:2504.20068, 2025）：请求信息不精确时把预测当软信号。
2. **重试亚稳态建模**：Trigger, Not Root Cause（DOI:10.3390/fi18070377, Future Internet 18(7), 2026）λ_eff=λ/(1−r·f)、f_c=(C−λ)/(r·C)，结论「压 r 而非只修触发器」；ContinuityBench（arXiv:2607.15899, 2026）失败模式二：固定间隔重试使 100 并发同步醒来轰击限流后备→socket 耗尽崩溃，修复 t_wait=min(30s, 2^a+U(0,1))；Self-Inflicted DoS（DOI:10.68218/2ddp5705, 2026）对照：固定 1s 间隔峰值约 3.1× 基线 vs 指数+抖动约 1.4×（20 客户端单故障类型，作者自述非统计功效研究，仅定性参考）。
3. **多租户优先级准入**：Token Management in Multi-Tenant AI Inference Platforms（arXiv:2603.00356, 2026）：五档服务等级、优先级公式（三因子：等级权重/突发/debt EWMA，典型 α=2.0/1.0/4.0，完整式见 P1-16）、有序准入「新请求优先级 < 在途最小优先级 → 429+Retry-After」；自报数字（38% 过载下 guaranteed P99 TTFT <1.2s vs 19s+）为其平台环境结果。
4. **在线学习路由（门槛低于常被引述）**：SLARouter（arXiv:2606.19376, 2026）Eq 4/5：argmin{V·Ĉ_m + Q·(α−ŝ_m)} + 虚拟队列 Q←max(0, Q+α−ŝ)，零模型版 ŝ 可用现有错误率代理——整个决策式只是「标量队列 + 每模型两个数」；LinUCB（Online Multi-LLM Selection…, arXiv:2506.17670, 2025）闭式更新无训练管道 + sublinear regret + 预算感知变体；代理奖励（arXiv:2607.09015, 2026）缓解反馈稀疏。
5. **缓存联动与淘汰**：Category-Aware Semantic Caching（arXiv:2510.26835, 2025）Eq 6-7 TTL 负载联动 λ=(L_p/L_target)·w_L+(Q/Q_target)·w_Q（**L_p 是延迟分位而非均值**）、类别差异化可零开销（按端点）；SOLAR（arXiv:2607.00394, 2026）8 策略实证：语义负载下 LRU/LFU 一致差于 FIFO；Robust Learning-Augmented Caching（arXiv:2106.14693, ICML 2021）为淘汰升级预留「预测器 + 经典策略兜底」结构。
6. **并发控制**：CONCUR（arXiv:2601.22705, 2026）agentic 负载应以拥塞信号/资源量而非请求数控制并发；Fluid-Guided Scheduling（arXiv:2504.11320, 2025）稳定域内存需求随输出长度近似二次增长。

## 4. 差距分析（NexusFlow 现状 vs 版图）

**先说强的**：路由骨架（weight/queue/window + EWMA 延迟/错误率因子、温度平滑 w^(1/T)、Po2C、Shannon 熵 + top1>0.9 告警）属开源网关第一梯队；SSE 字节层定界、双协议 IR、零拷贝透传、常量时间密钥比较均扎实。

**三点拉低（均经源码核实）**：

| 差距 | 现状落点 | 版图水位 |
|---|---|---|
| 熔断误触发 | `manager.rs:115-123` 用 `CircuitBreakerConfig::new(5,3,30s)` 未覆盖库默认 slow-call 500ms/0.5（limiteron `constants.rs:106/:111`）；探针实测 4×800ms 全 Ok → state=Open | 四家均无延迟启发式或均有最小样本/比例门槛 |
| 弹性覆盖 1/4 | CB 只包裹 `execute_http_with_retry`（`manager.rs:541-553`，唯一链路 worker 队列路径）；三条裸路径：透传非流式 `manager.rs:1054`、流式 `:839`、透传流式 `:1139`（awk 分段 grep 零 circuit 引用）；fallback 循环仅非流式转换路径调用（`openai.rs:362`、`anthropic.rs:321`）；而 `openai.rs:56` `use_passthrough = type_=="openai"` 使最常见 openai→openai 与全部流式裸奔 | OpenRouter/LiteLLM 全路径 failover + 首字节边界语义 |
| 入站限流名不符实 | `config.toml:39-55` [rate_limit] 段被 `Config`（`config/mod.rs:464-489`，无字段、无 deny_unknown_fields）serde 静默丢弃（探针实测序列化无 rate_limit key）；实际生效 `Governor::new()`（`router_builder.rs:62`）内置兜底 `Matcher::User["*"]`+TokenBucket 100/10（limiteron `governor.rs:578-606`）→「全站 10 rps 单桶」，per-endpoint/burst/租户全不存在 | Kong/Cloudflare 多窗口、多维度、per-endpoint |
| 信号层硬伤 | 延迟 EWMA 三口径混流：`metrics.rs:130` 单 duration 参数，四个喂入点（`manager.rs:353` 排队+HTTP 全程 / `:901`、`:1185` 流式响应头 / `:1043` 透传全程）进同一 `ewma_latency_ms`（`metrics.rs:164-167`）；且 `config.toml:98-99` w_latency=0.0/w_error=0.0——第 4 维根本没启用。负载信号：`router.rs:613-616` queue_factor 输入 `queue.len()`（`manager.rs:740-743`，receiver 深度），不含在途，分母 100 绝对常数跨 vendor 不可比 | in-flight 计数 + 分口径延迟 + 相对化归一 |
| 时段门控跨午夜判反 | `rate_limiter.rs:155-156` `now>=*start && now<*end`：22:00-06:00 配置凌晨恒 false → worker 拒绝（`manager.rs:288-296`）+ get_status Throttled（`:707-709`）→ 路由剔除（`router.rs:273/:314`）；`get_window_info` 窗口外分支（`manager.rs:1276-1282`）同样非跨午夜感知且喂 window_factor | LiteLLM `access_windows.py:35-75` 有跨午夜分支 + 每窗口时区 |
| 重试抖动缺口 | `retry.rs:231-233` Retry-After 原样返回零抖动；`:242` 5xx 固定 1s 零抖动（仅 429 无头分支 :235-241 带 jitter=0.2） | 全 jitter + 总时间预算（Portkey 60s） |
| 熔断对 429 无感 | `error_map.rs:116-118` RateLimited 不计失败 → 被限流 vendor 以满权重继续接流量 | LiteLLM 429 立即冷却 |
| 缓存 | `response_cache.rs:164-165` ttl() 全局单一 300s；默认关闭（`config.toml:117-118` enabled=false）；语义键场景 TinyLFU 方向可疑（SOLAR） | 类别差异化 + 负载联动 TTL |

## 5. 引入与优化建议清单（19 条：P0×7 / P1×8 / P2×4）

核实状态图例：**verified-A** = 事实核查员逐项核实（`out/research2/source-check-id*.md`，结论 confirmed）；**verified-B** = 本报告撰写会话抽查核实（给出命令）；探针类既往实测如实注明。

### P0 —— 正确性与信号修复

**P0-1 修复熔断器 slow-call-rate 默认阈值对 LLM 延迟的误触发**（effort: low）
机制：构造 `CircuitBreakerConfig` 时链式设置 `.slow_call_duration_threshold(Duration::from_secs(60))`（高于任何真实端到端延迟，等效关闭；链式 API limiteron `types.rs:134-143`）或 `slow_call_rate_threshold≥1.0`，使熔断只由 ErrorClassifier 判定的真实失败驱动；慢调用率可保留为 /admin 独立观测指标。
Gap：`manager.rs:115-123` 未覆盖默认 500ms/0.5；探针实测 4×sleep(800ms) 全 Ok → results=[(0,true),(1,false),(2,false),(3,false)]、state=Open、failure_count=0（探针已删，机制由 `types.rs:581` 记慢调用、`:596-616` 比例判定静态支撑）；get_status Open→Unavailable（`manager.rs:695`）→ 路由全局剔除（`router.rs:273`）。`tests/circuit_breaker_test.rs` 无 slow-call 用例。
来源：Bifrost 文档（响应头触发原文）、Portkey `circuitBreaker.ts:135-164`、limiteron 源码。收益：消除健康 vendor 周期性被剔 30s 的隐性故障；可用同款探针回归。
核实：✅ verified-A（source-check-id1：三来源逐字命中、行号全中；探针既往实测无法重放，已如实注明）。

**P0-2 熔断/重试/换路覆盖到透传与流式路径（当前 4 条执行路径只有 1 条受保护）**（effort: high）
机制：熔断与换路收敛到统一执行入口。工程要点：(a) 三条裸路径返回原始 Value/Bytes 而非 UnifiedResponse（`manager.rs:1006-1017`、`:1139-1143`），CB 包裹需泛型化或逐路径写映射；(b) `execute_request_with_fallback`（`api/mod.rs:114-162`）绑定 Unified 类型，透传换路需平行实现排除集循环；(c) 流式「首字节前换路」需改 `openai.rs:58-127` 流式分支为「先拿响应头判定成功，再交出 body 流」。可分期：一期透传非流式（收益最大改动最小），二期流式首字节前换路。
Gap：见 §4 表；`openai.rs:192-193` 注释自认「透传无 fallback」。
来源：OpenRouter 首字节边界语义（文档原文）、LiteLLM `router.py:12989-13131` 候选过滤管道 + `:7183-7258` 失败排除集跨跳累积、ContinuityBench 三失败模式。
核实：✅ verified-A（source-check-id2：来源与全部行号命中；「载荷」对应论文核心问题而非独立小节，已注明）。

**P0-3 入站限流落地：[rate_limit] 段当前被整段丢弃，Governor 跑库内置默认规则**（effort: medium）
机制：两步。(a) Config 增加 rate_limit 段，load 后经 `Governor::apply_config`（limiteron `governor.rs:1664`，带回滚语义）下发；匹配维度用 `Matcher::User`（认证后 API key 派生 user id 注入 RequestContext；`MatchCondition::User` 读 X-User-Id 或命中 "*"，`engine.rs:311-318`）。(b) per-endpoint 现实约束：Matcher 枚举六变体无 path（`rule.rs:64-87`），且 apply_config 不重建 Custom 匹配器注册表（`governor.rs:1674-1677` 注释）——二选一：注册 Custom（热更受限）或 rate_limit_middleware 内按端点表自筛 path（`router_builder.rs:27-47` 现只透传 path），后者更简单。
Gap：见 §4 表；`di.rs:29-51` minimal_flow_config 从未被请求路径消费。任务背景声称的「GCRA 入站限流」与源码不符（limiteron GcraLimiter 存在但 NexusFlow 零使用——`grep -rn "ShardedSlidingWindow|GcraLimiter|SlidingWindowLimiter" src/` 零命中，本会话执行）。
来源：Kong 多窗口/维度、Cloudflare fixed/sliding、limiteron 能力边界。
核实：✅ verified-A（source-check-id3；探针既往实测无法重放，由「Config 无字段 + serde 默认忽略未知字段」静态独立支撑）。

**P0-4 出站 HTTP 超时治理：连接/读取零超时 + 60s 硬编码含排队 + 流式无超时**（effort: medium）
机制：三层模型：(a) Client 设 connect_timeout（如 10s）；(b) 非流式总超时从上游 send 发起起算、per-vendor 可配（VendorConfig 增 timeout_ms，默认 60s）；(c) 流式引入 TTFT 语义超时（send→响应头这一段，对齐 Cloudflare「首部分返回后愿意继续等」语义）+ chunk 间 idle timeout（如 30s）。
Gap：`manager.rs:150` `reqwest::Client::new()` 零超时，出站可无限挂起；非流式唯一超时 `:773-774` 60s 硬编码包住 response_rx（入队后，`enqueued_at` :769）——排队 50s 只剩 10s 执行；两条流式路径无任何 timeout 包裹，上游不回响应头时无限挂起并占住并发票据（permit 随流存活，`:1080`/`:1163-1189` 注释自证）= 并发放大。
来源：Cloudflare request-handling（"timeout is based on when the first part of the response comes back" 原文 + cf-aig-request-timeout 头）、Portkey 60s 预算、production-stack 分阶段计时。
核实：✅ verified-A（source-check-id4）。

**P0-5 Retry-After 与 5xx 固定间隔分支补全范围抖动，消除同步惊群**（effort: low）
机制：`retry_delay_for` 内两个无抖动分支叠加全范围抖动：Retry-After 分支返回 ra + U(0, ra)（jitter 复用 [retry].jitter 并对该场景默认提到 0.5~1.0），5xx 由固定 1s 改 1s + U(0, 1s)；RNG 注入对齐 `router.rs:424` 的 `&mut dyn rand::RngCore` 模式，或返回「基准+幅度」由调用方采样。
Gap：`retry.rs:231-233` Retry-After 原样返回；`:242` RetryableFixed 固定 1s；仅 429 无头分支带 jitter=0.2（`config.toml:73`）。上游对一批并发请求返回同一 Retry-After 时同刻醒来同时重打。
来源：ContinuityBench 失败模式二（socket 耗尽崩溃 + 修复公式）、Self-DDoS 对照（3.1× vs 1.4×，定性）、Portkey 多头解析可顺带采纳。
核实：✅ verified-A（source-check-id5：Self-DDoS PDF 全文下载核对至数字级）。

**P0-6 路由负载维度改用在途请求数（in-flight），排队深度只作次级信号**（effort: low）
机制：VendorInstance 增在途计数，三等价实现任选：① 读现有 `Arc<Semaphore>` 的 available_permits()（`rate_limiter.rs:40-52` wrapper 已自持，in_flight = max−available，零新增状态）；② limiteron `ConcurrencyLimiter::peek(1)` 返回 snapshot（`concurrency.rs:276-304`）；③ acquire/drop 自增 AtomicI64。把 compute_weight 的 queue_factor 输入从 queue_len 改为 in_flight。进阶：相对化 (in_flight−mean_fleet)/max(1, mean_fleet)。
Gap：见 §4 表；20 请求在途、队列空时该 vendor 满分空闲，加权随机会持续砸流量直至队列堆积——信号滞后整整一个请求时长。
来源：LiteLLM least_busy、Kong least_connections（+1 注释）、Higress 蓄水池平票 + 长流存活、production-stack 归一化与钳制理由（rst 原文）。
核实：✅ verified-A（source-check-id6）。

**P0-13 修复 TimeWindow 时段门控的跨午夜判定缺陷**（effort: low）
机制：两处判定改跨午夜感知：`if start <= end { now >= start && now < end } else { now >= start || now < end }`——落点 `rate_limiter.rs:152-157` 与 `manager.rs:1276-1282`（后者喂 window_factor 参与路由权重）。进阶：每窗口可选时区字段（当前硬编码 `chrono::Local`，两处同）。
Gap：start=22:00/end=06:00 的夜间档配置在凌晨 2 点判 `02:00>=22:00` 为 false → check false，后果链（worker 拒绝 + Throttled 剔除 + window_factor 惩罚）全部命中——跨午夜配置在它本该生效的整个时段把 vendor 剔出路由。TimeWindow 无配额语义（`accounts_tokens()` false，`:173-175`），是可用性门控 bug 而非配额泄漏。
来源：LiteLLM `access_windows.py:35-75`（跨午夜分支 + ZoneInfo + team_ids 豁免，逐字命中）；Kong calendar（限定：period 仅支持 month/week，LiteLLM 才是完整参照）。
核实：✅ verified-A（source-check-id13；首轮记录实际位于 `out/research/module-vendor.md:190`，gap 原引笔记名有出入，已修正）。

### P1 —— 弹性升级与信号分道

**P1-7 熔断触发式升级为「失败率比例 + 最小样本数」，并补 429 感知**（effort: medium）
机制：limiteron 只支持连续次数阈值，需在 NexusFlow 侧自建窗口判定层：per-vendor 60s 滑窗（复用 `metrics.rs:24-98` SlidingCounter）。关键架构决策：路由层只接受 Available（`router.rs:273/:314`），Throttled/Unavailable 路由效果同为剔除——「软降权」必须把窗口失败率接进 weighted_select 权重打折（如 fail_pct>0.5 且 total≥5 时权重 ×0.1），这是新接口改动。429 感知：ErrorClassifier 把 RateLimited 判为不计失败（`error_map.rs:116-118`），须绕过 CB 分类器自建独立计数。
Gap：`manager.rs:120` 连续 5 次失败计数式的两个失效模式——低流量 5/5 全挂误杀；高流量慢性 30% 错误率永远凑不齐「连续 5 失败」开不了路；且 429 风暴对熔断完全不可见。
来源：LiteLLM `cooldown_handlers.py:376-416`（阈值数值逐字命中）+ 分桶 policy、Portkey `circuitBreaker.ts:135-164`（样本不足按 0 计）。
核实：✅ verified-A（source-check-id7；「分桶仅 LiteLLM 有」为负向全称，间接核实）。

**P1-8 延迟信号口径分道（TTFT/端到端）+ Peak-EWMA 时间常数衰减 + min+buffer 候选集，先修口径再启用 w_latency**（effort: medium）
机制：VendorMetrics 拆三组独立 EWMA：ttft_ms（流式采样点 = send() 返回即响应头到达；严格首 token 口径需分帧层记首个 data 帧，网关侧以响应头为近似须文档如实标注）、e2e_ms（非流式把排队段剥出单列 queue_ms）、error_rate 不变。EWMA 改 Peak-EWMA：α_dyn = 1−exp(−Δt/τ)，τ 可配（Kong 10s）——久未采样 α→1 快速反映恢复，高频 α≈0.1 平滑抗噪。同落点补 min+buffer 候选集：按延迟排序，候选集 = 延迟 ≤ 最低 ×(1+buffer)（buffer 默认 0.5），集合内均匀随机，与 Po2C 并列。完成后才把 w_latency 调非零。
Gap：见 §4 表；`weighted_select_with`（`router.rs:424-482`）是纯轮盘赌，无候选集机制。
来源：LiteLLM `lowest_latency.py`（分道/惩罚样本/候选集三处命中）、Kong `latency.lua:34-41`、production-stack TTFT 严格口径（:198 注释）。
核实：✅ verified-A（source-check-id8）。

**P1-9 重试放大守门器（λ_eff/f_c 亚稳态防护）与重试总时间预算**（effort: medium）
机制：两个计数器（attempts_retried/requests_failed）每 10~30s 算 r、f、λ；amplification = 1/(1−r·f)，f_c = (C−λ)/(r·C)；守门：f > 0.8·f_c 时把该路径 max_total_attempts 临时收紧到 1（压 r）并告警；r·f>0.95 告警（发散）。同时加单请求重试总时间预算（如 60s 含所有退避等待）：fallback loop 顶部记 Instant，每次 sleep 前检查剩余，超限冒泡原错误。
Gap：`grep -c "retry_count|retries" src/utils/metrics.rs` = 0（核查员执行），GlobalMetrics 无重试维度；`config.toml:68/:74` max_retries=3 与 max_total_attempts=8 均为次数预算——8 次 × Retry-After 30s 可让单请求占用 worker 与票据 4 分钟。
来源：Trigger, Not Root Cause（λ_eff 公式逐字命中）、Portkey 递减式预算、ContinuityBench 失败模式二即网关侧亚稳态实例。注：原 gap 将「触发器消除后回路自我维持」归于 papers-resilience 笔记论文 2，经核实该表述实出自本条论文 1 与 ContinuityBench（笔记论文 2 为 arXiv:2510.04516 拥塞感知重试），本报告按正确归属引用。
核实：✅ verified-A（source-check-id9，含上述归属修正）。

**P1-10 会话亲和从硬 return 软化为评分项，亲和键补租户维度与 SetNX 首绑定收敛**（effort: medium）
机制：(1) 软化：命中后不直接 return，乘亲和增益 weight×(1+boost_factor)（可配热更）；或 production-stack 形态 score = 亲和收益 − β·相对负载——注意 β=0 是纯缓存收益 argmax 而非硬粘，NexusFlow 现状比 β=0 更极端。(2) 键隔离：`AffinityTable::key` 改 `affinity:{api_key_hash}:{session_key}`，复用响应缓存提取逻辑。(3) bind 拆出 bind_if_absent 供首绑定（backend 无原子 setnx 时 get+set 包裹，竞态窗口可接受），迁移绑定仍无条件 set。
Gap：`router.rs:356-366` 亲和命中即 `return Some(RoutingDecision)`，五因子全不参与（注释写「boost 而非硬粘」但实现是硬选）；`affinity.rs:48-50` 键为 `affinity:{session_key}`，session_key 直接来自客户端 X-Session-Id——两租户同 session id 互相劫持；`affinity.rs:64-73` bind 无条件覆盖（注释「重复绑定覆盖旧值」），并行首请求互相覆盖。
来源：Bifrost sessionaffinity.go（SetNXWithTTL 收敛）、production-stack β 语义、LiteLLM 请求级权重表。
核实：✅ verified-B（本会话 `sed -n '356,366p' src/router/router.rs` 显示命中即 return；`sed -n '44,75p' src/router/affinity.rs` 显示键格式与无条件 set）。

**P1-14 SLA/满意度虚拟队列打分项（SLARouter 零模型版）——不需要每请求质量信号**（effort: low）
机制：每租户（或全局）标量虚拟队列 Q ← max(0, Q + α − ŝ)；决策 score_m = V·Ĉ_m + Q·(α − ŝ_m) 取 argmin（V 小则 SLA 收敛快成本高，V=(Q_max·ε)/ΔC 起步）。零模型版 ŝ_m 直接用 1−该模型 EWMA 错误率（`metrics.rs:164-166` 现成，本会话复核 `ewma_error_rate` 更新存在），Ĉ_m 用 [pricing] 单价；V/α 进 [routing] 热更。落点为 compute_weight 倒数项或前置打分层（与建议 10/15 同一插入点）。
Gap：EWMA 错误率只被动统计，不反推任何降级/预算/路由决策；无反馈通道。修正说明：草案曾把学习型路由整体归入「等度量底座」，属过度引申——papers-routing.md 第四节将零模型版列为可立即实施，本条立项。
来源：SLARouter（Eq 4/5 + Lyapunov drift-plus-penalty）、代理奖励论文（arXiv:2607.09015）。P1 定级理由：当前无租户 SLA 消费方、ŝ 代理质量弱（错误率≠满意度）；运营确有 SLA 需求可升 P0。
核实：✅ verified-B（EWMA 错误率信号存在，本会话 `sed -n '160,167p' src/utils/metrics.rs` 复核）。

**P1-15 LinUCB 打分项：人工特征/哈希特征起步，闭式更新无新依赖**（effort: medium）
机制：每模型维护 A_k（d×d）、b_k；决策 UCB_k = ⟨x,θ̂_k⟩ + α√(xᵀA_k⁻¹x)，θ̂_k=A_k⁻¹b_k 取 argmax；更新只动被选臂 A_a+=xxᵀ、b_a+=r·x（纯闭式最小二乘，Rust 手写 Cholesky 或 d≤32 直接求逆）。特征 x 起步人工小特征（消息数/字符长/是否结构化输出/租户 one-hot），后续 n-gram 特征哈希（亲和层已有前缀哈希经验）；奖励 r 用现有成败信号。预算感知变体 score = UCB/max(ĉ−β, ε)。
Gap：路由内容盲——`route/route_with_session/route_with_hints`（`router.rs:59/:66/:76`）只接收抽象模型名；`grep -rniE "bandit|linucb|ucb" src/router/` 零命中（exit 1，本会话执行）。新 vendor 冷启动只能靠均匀随机撞，LinUCB 置信半径天然给出冷启动探索量。
来源：arXiv:2506.17670（Algorithm 1/Theorem 1/预算变体）、PAK-UCB（arXiv:2410.13287，低配参照；机制未读全文，如实标注）。
核实：✅ verified-B（签名与零命中均本会话复核）。

**P1-16 拥塞感知优先级准入：把 429 的决定权从先到先得交给优先级**（effort: medium）
机制：(a) auth 段给 API key 增可选 tier 字段（dedicated/guaranteed/elastic/spot），依赖建议 3 的租户身份。(b) 优先级公式 w_e = w_κe·(1+α_slo·ℓ*_e)⁻¹·(1+α_burst·b_e)⁻¹·(1+α_debt·d_e)，等级权重数量级差距（1000/100/1/0.1）保证正常时等级主导，debt EWMA 让公平份额随时间收敛，burst EWMA 防独占。(c) 准入：拥塞信号（并发票据占用率或 EWMA 延迟拐点）触发时，新请求优先级 < 当前在途最小优先级 → 直接 429+Retry-After；永不卸载健康检查。实施位置 rate_limit_middleware 或 handler 入口，不动 limiteron 内核。与已暂缓的「内容 tier 路由」无关——只需 2~4 档粗分类用于 shed 顺序。
Gap：`auth.rs:88` `validate_key(&self, key: &str) -> bool` 只返回 bool（本会话实读），无租户/等级维度；Governor 的 RequestContext 只带 path+method（`router_builder.rs:33-35`）。
来源：arXiv:2603.00356（式 1 + 有序准入⑤；自报数字为平台环境结果）、Trigger 论文 QoS tiering + 永不卸载健康检查、Higress critical/sheddable 二分。
核实：✅ verified-B（validate_key 签名本会话实读）。

**P1-17 排队论延迟前瞻估计（LPS 平均情形模型）替换/增强 EWMA 滞后项**（effort: medium）
机制：per-vendor 前瞻 Ŵ = (α/μ)^k/(μ−α)：α = 到达率（60s/10 桶滑窗可派生，但 `SlidingCounter::window_total` 是私有 fn（`metrics.rs:95`）——需新增 pub 的 per-vendor 窗口计数访问器，小改动；此点未单独复核，如实标注）；μ 从 EWMA 服务时长派生（依赖建议 8 先修口径）；k = vendor 并发上限（`config/mod.rs:131`）。接入：软约束版把 −δ·Ŵ 并入 compute_weight 线性因子（与热更系数机制无缝）；硬约束版接受可选 x-deadline-ms 头 + 租户默认 deadline，可行集（Ŵ≤τ）为空时改选 Ŵ 最小者。SFS 引擎级逐 batch 模拟不适用（网关对第三方 API 无引擎内部状态），只取论文 §4.2 平均情形估计器。
Gap：latency_factor（`router.rs:592-598`，`ref/(ref+ewma)`）是纯滞后观测；ρ→1 时排队论项发散，天然给逼近容量的 vendor 强惩罚——EWMA 结构上给不出。Po2C（`router.rs:504-510`）同样只用 EWMA。
来源：arXiv:2607.18253 §4.2（式 10/11/3/5）、production-stack relative_load 归一化互证、JITServe 软信号用法。
核实：✅ verified-B（latency_factor 与 Po2C 落点经 source-check-id8 + 本会话复核；window_total 私有性未单独复核）。

### P2 —— 能力增量

**P2-11 出站 Token/Interval 窗口从固定窗口换滑动窗口（首选 ShardedSlidingWindowLimiter）**（effort: low）
机制：`AdvancedRateLimiter::Token/Interval` 变体 inner 换 `limiteron::limiters::ShardedSlidingWindowLimiter::with_clock`（limiteron `sharded_sliding_window.rs:107-117` 支持时钟注入，保持 MockClock 测试路径，本会话实读确认）。**不要用 SlidingWindowLimiter**：new() 已标 `#[deprecated(since="0.1.1")]`（`sliding_window.rs:54-56`）且 allow() 硬编码 `Instant::now()`（:80-84），破坏时钟注入。
Gap：`rate_limiter.rs:139-146` Token/Interval 委托 FixedWindowLimiter（本会话实读：`FixedWindowLimiter::with_clock`）——窗口翻越点前后各打满一半配额可瞬时 2 倍速率打到上游 → 429。
来源：Cloudflare fixed/sliding 对照（12:09+12:11 共 20 次例）、Kong sliding 默认。
核实：✅ verified-B（本会话 `sed -n '139,146p' src/vendor/rate_limiter.rs`、limiteron 两文件实读、src 内零使用 grep 零命中）。

**P2-12 精确响应缓存 TTL 负载联动 + 按类别差异化（前提：先启用缓存；延迟信号依赖建议 8）**（effort: low）
机制：ttl() 改两段：base = ttl_secs × category_multiplier(model_id)（按 (endpoint, model) 零开销划分），再乘负载调整 λ = min(1,(L_p/L_target)·w_L+(Q/Q_target)·w_Q)，实际 TTL = base×(1+λ·(β_max−1))。输入口径：论文 L_p 是延迟分位（P95）——不引入分位估计器时可用 EWMA 均值替代并声明口径降级，且须待建议 8 修完口径再接。w_L/w_Q/β_max/类别乘数进 [cache] 热更。
Gap：`response_cache.rs:164-165` ttl() 返回全局单一 300s（本会话实读）。两个前提如实声明：(a) 缓存默认关闭（`config.toml:117-118` enabled=false，本会话实读），默认部署下本条无可服务流量；(b) 现成 EWMA 正是建议 8 判定需先修的污染序列，本条排其后。语义缓存维持第一轮暂缓——papers-caching 自结论「未找到任何可以不等日志数据就开语义缓存的证据」。
来源：arXiv:2510.26835（Eq 6-7/类别差异化/淘汰打分）、Bifrost 惰性过期、arXiv:2106.14693 兜底结构。
核实：✅ verified-B（ttl() 与 enabled=false 本会话实读）。

**P2-18 agentic 负载的 token 加权并发票据（票据按预估 token 量计费）**（effort: medium）
机制：票据 cost 从固定 1 改为预估 token 量：cost = clamp(input_tokens + max_tokens 缺省兜底, min, max)（max_tokens 缺省兜底是容量规划前提）。落点：`ConcurrencyLimiter` wrapper（`rate_limiter.rs:57-67`，本会话实读：`acquire_owned()` 单票据）加 cost 参数——limiteron `ConcurrencyLimiter::acquire(cost)` 原生支持（`concurrency.rs:193-215`，本会话实读）。建议 6 的 in-flight 信号同步改 token 加权（同一计数点、不同加权维度）。
Gap：四处 acquire（`manager.rs:299/863/1080/1164`）均无 cost，max_tokens=8 短请求与 32k 长会话等价占票；agent 多轮长会话下按请求数系统性低估真实负载。
来源：CONCUR（arXiv:2601.22705）、Fluid-Guided Scheduling（arXiv:2504.11320）。诚实限定：两篇论文是引擎内 KV 语境，网关取其「按资源量计数」思想。
核实：✅ verified-B（wrapper 与 limiteron acquire(cost) 本会话实读；manager 四处 acquire 行号经 source-check-id4/id6 交叉覆盖）。

**P2-19 冷却与候选集的低成本判定改进包（四级权重回退 / fail-open 安全网 / min_cooldown 提示）**（effort: low）
机制：三项独立小改动捆绑：(a) 权重回退链：请求级权重（header/API 参数）→ 配置 model.weight → vendor 容量自动权重（直接用 limiteron window_snapshot 的 limit）→ 全空均匀兜底——零配置接入新 vendor。(b) 全冷却 fail-open：仅当冷却是主动健康检查驱动（可配开关）才允许回落已冷却 vendor，真实失败冷却不允许绕过。(c) 冷却条目存 {timestamp, cooldown_time}，读侧算 remaining = timestamp+cooldown−now；全组冷却时把 min(cooldown) 作为 Retry-After 返回。
Gap：(a) 权重只有手工 `model.weight`（`config/mod.rs:81/:87` default_weight=1.0，本会话实读）；(b) 全冷却时 Validation 错误冒泡（`router.rs:104-107`，本会话实读），无降级选项；(c) fallback 耗尽错误（`api/mod.rs:147`）不携带「多久后重试」提示（本会话实读 :145-150）。
来源：LiteLLM `simple_shuffle.py:43-67` 四级链、`router.py:13067-13074` fail-open 限定、`cooldown_cache.py:135-157/:206-223` 显式剩余时间。
核实：✅ verified-B（三处源码落点本会话实读）。

## 6. 附录

### 6.1 论文与项目链接
论文（12 篇）：arXiv:2607.15899（ContinuityBench）、arXiv:2607.18253（Latency-Aware Routing/LPS）、arXiv:2606.19376（SLARouter）、arXiv:2506.17670（LinUCB Multi-LLM Selection）、arXiv:2607.09015（代理奖励）、arXiv:2603.00356（Token Management）、arXiv:2601.22705（CONCUR）、arXiv:2504.11320（Fluid-Guided Scheduling）、arXiv:2504.20068（JITServe）、arXiv:2510.26835（Category-Aware Semantic Caching）、arXiv:2607.00394（SOLAR）、arXiv:2106.14693（Robust Caching, ICML 2021）；正式发表/DOI：10.3390/fi18070377（Trigger, Not Root Cause, Future Internet 18(7)）、10.68218/2ddp5705（Self-DDoS）。项目：LiteLLM（github.com/BerriAI/litellm）、Portkey Gateway（2.0.0 分支 circuitBreaker.ts）、Higress（已迁移 higress-group/higress，API 301 同仓库）、Bifrost（docs.getbifrost.ai）、Kong（developer.konghq.com）、Cloudflare AI Gateway、OpenRouter 文档、vLLM production-stack、limiteron（本地 path 依赖 `Cargo.toml:60`）。

### 6.2 检索方式与核实覆盖（如实说明）
- 检索：`search_papers.py "<查询词>" --source arxiv|multi`（CS/AI 主题 arxiv 相关性最好；Semantic Scholar 429 自动降级属正常）；`--mode title` 按题精确查。GitHub 代码：raw 直连失败（curl 返回 000）时改走 `api.github.com/contents/...?ref=` base64 解码。Trigger 论文未入 arXiv/OpenAlex，靠 DOI 直查 Crossref 定位；Self-DDoS 下载 PDF 全文核对至数字级。
- 核实覆盖：10 份留档核实记录（source-check-id1~id9、id13，覆盖建议 1-9 与 13，结论全部 confirmed）；其余 9 条（10/11/12/14/15/16/17/18/19）由本报告撰写会话抽查核实，命令与结果已随条目标注，全部命中。材料所述「11 项源码核实」与留档 10 份存在 1 项计数出入，以留档为准。两项探针实测（建议 1 慢调用误触发、建议 3 Config 字段丢弃）为调研会话既往声明、文件已删无法重放，但结论均由源码机制静态独立支撑。
- 测试基线：本撰写会话执行 `cargo test --offline --lib` → 111 passed / 0 failed / 0 ignored（0.48s）；`cargo check --offline` 未在本会话单独重跑（被 test 编译覆盖）。

### 6.3 与第一轮建议的关系
- 第一轮 16 条已全部实施完毕（本轮基线），首轮报告见 `out/nexusflow-optimization-report.md`、首轮笔记 `out/research/`。
- 本轮 19 条构成：建议 1-12 为草案经评审修订后保留（修订点：建议 3 机制改为 limiteron 真实能力、effort 上调；建议 7 改为「三态接入权重打标」+ 429 不计失败前提；建议 11 首选 ShardedSlidingWindowLimiter；建议 12 补缓存默认关闭前提；建议 10 修正 β=0 语义；建议 5 定量降为定性；建议 1 删去无据引申——详见各条目）；建议 13-19 为新增 7 条，填补评审指出的遗漏（跨午夜门控 bug、SLA 虚拟队列、LinUCB 打分层、拥塞感知优先级准入、排队论前瞻、token 加权票据、LiteLLM 判定改进包）。
- 已暂缓方向的新证据：**语义缓存维持暂缓**（不等日志数据的证据未找到），但新增硬约束——启动时必须一并重估淘汰层（SOLAR：语义负载下 LRU/LFU 差于 FIFO，Moka TinyLFU 方向可疑）；**内容 tier 路由、离线质量-成本度量维持暂缓**；同时修正「学习型路由必须等度量底座」的错误捆绑——P1-14/P1-15 只需现有错误率/成败信号即可启动。
- 实施次序建议：先修正确性与信号（P0 七条，多数 effort=low：P0-1/5/6/13 合计约一天量级），再接 limiteron 已具备的滑窗/GCRA/租户原语与打分层在线学习（P1），离线训练路线继续等度量底座。
