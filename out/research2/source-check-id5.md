# 来源核实 — 建议 5（Retry-After 与 5xx 固定间隔分支补全范围抖动）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，全部数字级引用逐字命中；gap 本地代码行号全部核实）**

## 来源 1：论文 ContinuityBench (arXiv:2607.15899) — ✅ 属实

- 本次执行的命令：`python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "ContinuityBench: Stateful Failover in Multi-Provider LLM Routing" --mode title --limit 2` → 完整命中：[1] "ContinuityBench: A Benchmark and Systems Study of Stateful Failover in Multi-Provider LLM Routing"，Vishal Pandey/Gopal Singh，2026-07-17，DOI:10.48550/arxiv.2607.15899（与建议所引编号一致）。
- 失败模式二细节（C=100 并发固定间隔同步醒来轰击限流后备→socket 耗尽崩溃；修复 t_wait = min(30s, 2^a + U(0,1))）：本会话核实建议 2 时已通过 WebFetch `https://arxiv.org/html/2607.15899v1` 正文确认 6.2 节 "The Failover Retry Storm (Thundering Herd)" 逐点吻合（C=100 涌入严格限流后备、429 固定 2.0s 间隔同步轰炸、耗尽本地套接字崩溃、修复公式逐字一致）。

## 来源 2：论文 Self-Inflicted Denial of Service (DOI:10.68218/2ddp5705) — ✅ 属实（全部数字逐字命中）

- 命令与结果：
  1. `search_papers.py "Self-Inflicted Denial of Service" --mode title --limit 3 --source multi` → [2] 命中：标题与建议一致（Self-DDoS in Distributed Systems... An Analysis of Retry Storms, Thundering Herds, and Cascading Failure），DOI:10.68218/2ddp5705 逐字一致，期刊 Computational and Applied Science，2026-08-02。
  2. Crossref `api.crossref.org/works/10.68218/2ddp5705` → journal-article，摘要确认五种机制（retry storm/thundering herd/cascading failure/autoscaling feedback loop/cache stampede）+ 对应缓解（exponential backoff with jitter/circuit breaker/bulkhead/rate limiting/load shedding）+ "a controlled fault-injection experiment on a Kubernetes test cluster, comparing fixed-interval and jittered-backoff retry policies"。
  3. DOI 解析：doi.org/10.68218/2ddp5705 → 302 → casjournal.ge/index.php/cas/article/view/18（DOI 真实可解析）。
  4. 下载全文 PDF（casjournal.ge/index.php/cas/article/download/18/21，8 页），PyMuPDF 提取文本，逐字核实：
     - **3.1×**："the fixed-interval configuration produced request-rate peaks approximately **3.1×** the steady-state baseline at each one-second boundary during the fault window" ✅
     - **1.4×**："the jittered-backoff configuration kept peak load within roughly **1.4×** of baseline over the same window" ✅
     - **20 客户端**："**Twenty synthetic client workers** issued requests against the gateway" ✅
     - **固定 1s**："1) a fixed 1-second retry interval, and 2) exponential backoff with jitter (base 200 ms, factor 2, ±50 ms jitter, capped at 4 s)" ✅
     - **LitmusChaos**："LitmusChaos was used to inject a pod-network-latency fault (400 ms, 60-second duration)" ✅（建议称"LitmusChaos 说明性实验"，命中）
     - **非统计功效自述**："The experiment is offered as a worked example...; **it is not presented as a statistically powered study.**" 逐字命中 ✅

## 来源 3：Portkey Gateway (retryHandler.ts:108-151) — ✅ 属实（main 分支重新核实）

- 命令：`curl api.github.com/repos/Portkey-AI/gateway/contents/src/handlers/retryHandler.ts?ref=main`（221 行）+ `.../src/globals.ts?ref=main`
- :108 `if (response.status === 429 && followProviderRetry)` → :110-115 `POSSIBLE_RETRY_STATUS_HEADERS.find(...)` 多头解析 → :121-126 `retry-after` 秒→毫秒转换（其余头直接毫秒）→ :144-148 `setTimeout(..., retryAfter)`——建议引用区间 :108-151 精确命中。
- globals.ts:5 `MAX_RETRY_LIMIT_MS = 60 * 1000`、:7-11 `POSSIBLE_RETRY_STATUS_HEADERS = ['retry-after-ms','x-ms-retry-after-ms','retry-after']`——"多头解析（retry-after-ms / x-ms-retry-after-ms / retry-after）"逐字命中。

## gap 本地代码引用 — ✅ 全部命中（本会话逐一读取）

- src/utils/retry.rs:231-233 `if let Some(ra) = retry_after { return ra; }` 精确命中——Retry-After 原样返回零抖动。
- retry.rs:242 `ErrorCategory::RetryableFixed => Duration::from_secs(1)` 精确命中——5xx 固定 1s 零抖动；:28-29 注释"可重试-固定间隔（500/502/503）"与 :42-43 `500 | 502 | 503 => Self::RetryableFixed` 确认"RetryableFixed=5xx"关联；:210-211 文档注释"RetryableFixed（5xx）→ 固定 1s"。
- retry.rs:235-240 RetryableExponential → `limiteron::retry::delay_for_attempt(..., policy.jitter)`（唯一带抖动分支，建议 :235-241 命中）。
- config.toml:73 `jitter = 0.2` 精确命中（[retry] 段）。
- src/router/router.rs:424 `pub fn weighted_select_with(`、:429 `rng: &mut dyn rand::RngCore`——RNG 注入模式（&mut dyn RngCore）精确命中。

## 无法复核项

- 无。三项来源全部打开并核对至数字级（含 PDF 全文下载）；本地代码引用全部读取核实。
