# 来源核实 — 建议 2（熔断/重试/换路覆盖到透传与流式路径）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，引用一致；gap 本地代码行号全部命中）**

## 来源 1：OpenRouter (docs/api-reference/errors) — ✅ 属实

- 命令：WebFetch `https://openrouter.ai/docs/api-reference/errors`
- Pre-stream 静默换 provider：原文 "Silently retry with a different provider endpoint if fallback routing is enabled"；Mid-stream Note："If an attempt fails before any tokens reach you, OpenRouter automatically tries a backup provider."
- 首字节送达后 failover 停止：原文 "Failover also stops once part of the answer has reached you, since your application already holds output from the first provider."
- 与建议引用的"pre-stream 失败静默换 provider、首字节送达后 failover 停止的边界语义"逐点吻合。

## 来源 2：LiteLLM (router.py:12989-13131 + :7183-7258) — ✅ 属实（main 分支精确命中）

- 命令：`curl api.github.com/repos/BerriAI/litellm/contents/litellm/router.py?ref=main`（base64 解码，14754 行，存 /tmp/litellm_router_main.py）
- :12989-13131 = `async def async_get_healthy_deployments`（:12989）——候选过滤管道逐级：team filter(:13018) → web search filter(:13026) → health-check filter(:13044) → cooldown 过滤(:13063-13066)。"重挑前过健康/冷却过滤"属实。
- :7183-7258 = `async def _maybe_run_weighted_failover`（:7183）——失败排除集跨跳累积：:7208-7209 `prev_excluded | {failed_id}`、:7233 写回 `meta["_failover_excluded_ids"]`；重挑前过滤：:7221-7222 `(all_ids - cooldown_ids) - excluded`；:7249 `run_async_fallback` 触发换路。"失败排除集跨跳累积"属实。

## 来源 3：论文 ContinuityBench (arXiv:2607.15899) — ✅ 属实

- 命令：`python3 /root/.zcode/skills/liuxiang/scripts/search_papers.py "ContinuityBench: Stateful Failover in Multi-Provider LLM Routing" --mode title --limit 2`
- 论文存在：标题 "ContinuityBench: A Benchmark and Systems Study of Stateful Failover in Multi-Provider LLM Routing"（建议为简写，主体一致），作者 Vishal Pandey, Gopal Singh，2026-07-17，DOI:10.48550/arxiv.2607.15899（与建议所引 arXiv 编号一致）。
- WebFetch `https://arxiv.org/html/2607.15899v1` 正文核实"三失败模式及修复"：
  - 竞态：第 6.1 节 "State Corruption via Concurrency Race Conditions"（并行请求竞写共享会话历史缓存，CPR 降至约 28%）+ 修复（会话状态隔离/深拷贝/读写锁）✅
  - 惊群：第 6.2 节 "The Failover Retry Storm (Thundering Herd)"（C=100 并发涌入严格限流 fallback API，429 固定间隔重试同步化轰炸）+ 修复（指数退避加抖动 t_wait = min(30s, 2^a + U(0,1))）✅
  - 载荷：无独立小节，为论文核心问题 "the continuity gap"——无状态 failover 静默丢弃对话历史（failover 载荷不含历史），修复为 History-Forwarding 有状态代理（750 次事件 CPR 99.20% vs 无状态近 0%）✅
  - 注：建议的"三失败模式"概括中"载荷"对应论文核心问题（continuity gap/历史丢失）而非第 6 节独立小节；摘要另提出 CPR/CLO 两指标。引用实质成立。
- 附注：论文摘要称开源了评估框架 continuity-bench，但 GitHub 搜索（api.github.com/search/repositories?q=continuity-bench）未找到对应仓库（返回的 3 个同名/近名仓库均为无关项目）；此点不属建议引用范围，不影响结论。

## gap 中本地代码引用 — ✅ 全部命中（本会话逐一读取核实）

- CB 包裹唯一性：src/vendor/manager.rs:530 `async fn execute_http_with_retry`，:541 `circuit_breaker.execute`（CB 包裹，建议 :541-553 命中）；:577 `execute_http_with_retry_inner` 注释自证"由 execute_http_with_retry 经 CB 包装后调用"。
- 调用链：manager.rs:332 `Self::perform_http_request(`（worker 队列循环内，:340 传 `&instance.circuit_breaker`）→ :410 `async fn perform_http_request` → :433 `Self::execute_http_with_retry(`（建议 :332/:433 精确命中）；:745 `pub async fn execute_request`（建议 :745 精确命中）。
- 三条裸路径无 CB：`awk 'NR>=839&&NR<=1053' manager.rs | grep -c -i circuit` → 0；`awk 'NR>=1054&&NR<=1138'` → 0；`awk 'NR>=1139&&NR<=1220'` → 0（与建议"awk 分段 grep 无 circuit 引用"一致）。
- 裸路径定义与 send：manager.rs:839 `execute_stream_request`（send 于 :885-891，建议 :885-895 命中）；:1054 `execute_passthrough_request_inner`；:1139 `execute_passthrough_stream_request`（send 于 :1172-1179，建议 :1172-1177 命中）。
- 裸路径返回类型：:1005-1017 `execute_passthrough_request → Result<Value, NexusFlowError>`（:1012）；:1139-1143 `execute_passthrough_stream_request → Result<impl Stream<Item=Result<Bytes,…>>>`（:1143）——原始 Value/Bytes 而非 UnifiedResponse，命中。
- api 层：src/api/mod.rs:114 `pub(crate) async fn execute_request_with_fallback`，签名绑定 UnifiedRequest(:118)/UnifiedResponse(:121)，函数体 loop+excluded 累积+route_excluding，止于 :162（建议 :114-162 精确命中）；fallback 循环仅被转换非流式调用：src/api/openai.rs:362、src/api/anthropic.rs:321（均精确命中）。
- 触发面：src/api/openai.rs:56 `let use_passthrough = decision.vendor.type_ == "openai"` 精确命中；:58 `if use_passthrough`；:65 透传流式（execute_passthrough_stream_request）；:165 透传非流式；:192-193 注释"（透传无 fallback）"逐字命中；:90-127 流式分支 Body::from_stream(reframe_passthrough_stream) 后直接 into_response（:126-127），确认"先 send 后立即交出 body 流、无首字节前换路"。

## 与建议描述的两处细微出入（不改变结论）

1. limiteron check_slow_call_rate 的 transition_to_open() 调用行（id=1 相关）——本条不涉及。
2. 本条建议 gap 行号与实际代码完全一致，未发现出入；仅来源 3 的"载荷"概括对应论文核心问题而非独立小节（见上）。
