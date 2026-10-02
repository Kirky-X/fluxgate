# 来源核实 — 建议 8（延迟口径分道 + Peak-EWMA + min+buffer 候选集）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，公式/常量/注释逐字命中；gap 本地代码全部核实）**

## 来源 1：LiteLLM lowest_latency.py — ✅ 属实（三处行号全部命中）

- 命令：`curl api.github.com/repos/BerriAI/litellm/contents/litellm/router_strategy/lowest_latency.py?ref=main`（550 行）
- 采样分道（:110-130）：:110-112 仅流式记 TTFT `if kwargs.get("stream") is True: time_to_first_token = _ttft_seconds(kwargs.get("completion_start_time", end_time) - start_time)`；:126-128 per-token 归一化 `normalized_value = safe_divide_seconds(response_seconds, completion_tokens); final_value = normalized_value` ✅
- Timeout 惩罚样本（:193-228）：:224-228 注释与代码逐字 "## Latency - give 1000s penalty for failing" → `append(1000.0)` ✅
- min+buffer 候选集（:482-494）：:485 `lowest_latency = sorted_deployments[0][1]`；:488 `buffer = self.routing_args.lowest_latency_buffer * lowest_latency`；:490 `valid_deployments = [x for x in sorted_deployments if x[1] <= lowest_latency + buffer]`（等价于 ≤ 最低 × (1 + buffer)）；:493 `random.choice(valid_deployments)` 集合内均匀随机——"防打爆最快者"逐行命中 ✅

## 来源 2：Kong latency.lua:34-41 — ✅ 属实（含 10s 常量）

- 命令：`curl api.github.com/repos/Kong/kong/contents/kong/runloop/balancer/latency.lua?ref=master`（253 行）
- :26 `local DECAY_TIME = 10 -- this value is in seconds`——"Kong 用 10s" ✅
- :34-41 逐字命中：`local function decay_ewma(ewma, last_touched_at, rtt, now)` / `td = now - last_touched_at` / `weight = math_exp(-td / DECAY_TIME)` / `ewma = ewma * weight + rtt * (1.0 - weight)`——Peak-EWMA 时间常数衰减；久未采样（td 大）→ weight→0 → 新样本权重 (1−weight)→1（α→1 快速反映恢复），与建议描述一致 ✅

## 来源 3：vLLM production-stack request_stats.py:144-249 — ✅ 属实（TTFT 口径 = 首 token）

- 命令：重新 `curl api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/stats/request_stats.py?ref=main`，md5 与本会话缓存一致（7fabdd7f…），内容为 main 最新。
- 三阶段状态机：:144 `on_new_request`（prefill 计数）→ :193 `on_request_response`（:192 `in_decoding_requests += 1`；:198 注释逐字 **"Update TTFT as time from request start to first token"**）→ :202 `on_request_complete`（first_token_time 判定 prefill/decode 阶段）。
- 建议引用"其 TTFT 严格口径是首 token 而非响应头"由 :198 注释直接证实 ✅。

## gap 本地代码引用 — ✅ 全部核实（本会话逐一读取）

- metrics.rs:130 `pub fn record_request(&self, vendor: &str, success: bool, duration: Duration)`——单 duration 参数精确命中。
- metrics.rs:161-167 EWMA 更新（`EWMA_ALPHA * ms + (1-EWMA_ALPHA) * entry.ewma_latency_ms`，:164-167 表达式命中；α 固定常量，无 Peak-EWMA 时间衰减）。
- manager.rs 四个喂入点口径各异：:353 `pending.enqueued_at.elapsed()`（注释自证"排队 + 限流 + HTTP 全程"）；:900-901 转换流式 `start.elapsed()`（send 发起→响应头）；:1184-1185 透传流式 `start.elapsed()`（注释"send 成功即记成功"）；:1042-1043 透传非流式 `start.elapsed()`（start 早于 limiter acquire，全程）——四个喂入点全部命中，且都汇入同一 ewma_latency_ms。
- router.rs:592-598 `latency_factor_from_ewma(ewma_ms, ref_ms) = ref_ms / (ref_ms + ewma_ms)`——与单一 latency_ref_ms 比较，精确命中。
- config.toml:98-99 `w_latency = 0.0` / `w_error = 0.0` 精确命中（第 4 维当前未启用；:100 latency_ref_ms = 1000.0）。
- router.rs:424-482 `weighted_select_with`：:437-439 Po2C 实验开关注释（"默认 weighted 走经典轮盘赌"）+ :453-459 cumulative 轮盘赌循环——确认无 min+buffer 候选集机制（Po2C 是 power-of-two-choices，非候选集），"纯轮盘赌"表述与代码一致 ✅。

## 无法复核项

- 无。三处来源文件全部下载核对至行级（vLLM 文件重新下载 md5 校验一致）；本地代码全部落点已读取。
