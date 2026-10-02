# 来源核实 — 建议 9（重试放大守门器 λ_eff/f_c 与重试总时间预算）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，λ_eff 公式逐字命中；gap 本地代码全部核实）**

## 来源 1：论文 Trigger, Not Root Cause (DOI:10.3390/fi18070377) — ✅ 属实（公式逐字命中）

- 核实过程：
  1. Crossref `api.crossref.org/works/10.3390/fi18070377` → 标题 "Trigger, Not Root Cause: Re-Examining GitHub's 2024 to 2026 Availability Decline Under Artificial Intelligence-Driven Load"，期刊 **Future Internet** 18(7)，2026-07-20——与建议引用（DOI + 标题简写）一致。
  2. MDPI 页面 https 403，改用 web_reader 抓取全文：§5.3 "A Minimal Model of the Tipping Point" 逐字命中 λ_eff 公式："when a fraction f of requests fails or times out and each failed request is in turn retried, the load actually offered to the resource is not λ but the retry-amplified quantity **λ_eff(f) = λ/(1 − r f), valid while r f < 1**"。
  3. f_c：正文 "The boundary between them is the failing fraction at which the amplified load just reaches capacity, found by setting λ_eff = C"——由 λ/(1−r·f_c)=C 解出 f_c=(C−λ)/(r·C)，与建议公式数学一致 ✅。
  4. "压 r 而非只修触发器"："Disciplined retries, exponential backoff with jitter and a bounded retry budget, lower the effective r, which raises the critical fraction f_c" + "this literature treats the sustaining loop and the vulnerable state, not the trigger, as the root cause" ✅。
- 核实说明：初次用脚本 --mode title 检索该标题未命中（Future Internet 未入 arXiv/OpenAlex 优先返回），靠建议给出的 DOI 直查 Crossref 定位；Zenodo/DataCite 无此 DOI（MDPI 注册）。

## 来源 2：Portkey retryHandler.ts:84-148 — ✅ 属实（重验）

- 命令：重新 `curl api.github.com/repos/Portkey-AI/gateway/contents/src/handlers/retryHandler.ts?ref=main`（221 行）
- :84 `let remainingRetryTimeout = MAX_RETRY_LIMIT_MS;`（总预算起算）；:131-132 `retryAfter >= MAX_RETRY_LIMIT_MS || retryAfter > remainingRetryTimeout`（超预算放弃）；:138 `remainingRetryTimeout -= retryAfter`（递减式预算）——"remainingRetryTimeout 递减式总预算（60s）"命中（MAX_RETRY_LIMIT_MS=60s 于 globals.ts:5，本会话建议 5 核实已确认）。

## 来源 3：ContinuityBench (arXiv:2607.15899) — ✅ 属实（重跑确认）

- `search_papers.py --mode title` 命中：ContinuityBench: A Benchmark and Systems Study of Stateful Failover in Multi-Provider LLM Routing，DOI:10.48550/arxiv.2607.15899。
- "失败模式二即网关侧亚稳态实例（socket 耗尽崩溃）"：本会话建议 2 核实时已由 WebFetch arxiv.org/html/2607.15899v1 正文 6.2 节确认（C=100 并发固定间隔重试同步轰炸 → 耗尽本地套接字 → 代理崩溃），与"亚稳态/自我维持"表述一致。

## gap 本地代码引用 — ✅ 全部核实（本会话执行）

- `grep -c "retry_count\|retries" src/utils/metrics.rs` → **0**（退出码 1）——精确命中"全局指标无重试维度"。
- metrics.rs:101-116 GlobalMetrics 字段：total_requests/successful_requests/failed_requests/total_duration_ns/per_vendor/affinity_hits/affinity_misses/traffic——"只有 total/success/failed/affinity/traffic 五组"的概括与实际相符（另含 duration 累计与 per-vendor 明细，不影响结论）。r、amplification、f_c 确实无法从现有指标计算。
- config.toml:68 `max_retries = 3`、:74 `max_total_attempts = 8` 精确命中——均为次数预算。
- api/mod.rs:131 `let max_attempts = state.config.read().retry.max_total_attempts.max(1);` 精确命中——纯次数非时间预算，"8 次 × Retry-After 30s ≈ 4 分钟"的放大情景成立（Portkey 对照的反面）。
- papers-resilience 笔记论文 2（out/research/papers-resilience.md:34-38，arXiv:2510.04516 ATB/AATB）：gap 称"论文 2 的核心结论：触发器消除后重试放大回路可自我维持"——**轻微转述出入**：笔记论文 2 的核心结论是"纯指数回退造成过度重试+成本，拥塞感知重试（最小反馈推断拥塞）可将 429 最多减 97.3%"；"自我维持回路"的表述实际出自建议 1 的 Trigger Not Root Cause 论文（metastable failure 文献）与 ContinuityBench 失败模式二。方向一致（重试放大危害真实），归属论文有偏——gap 旁注瑕疵，非 sources 项，如实记录。

## 无法复核项

- 无（就 sources 而言）。三项来源全部打开核对至字面/公式级；本地代码全部落点已读取。
