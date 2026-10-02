# 来源核实 — 建议 4（出站 HTTP 超时治理：零超时 + 60s 硬编码含排队 + 流式无超时）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，行号/引文精确命中；gap 本地代码引用全部核实成立）**

## 来源 1：Cloudflare AI Gateway request-handling — ✅ 属实（逐字命中）

- 命令：WebFetch `https://developers.cloudflare.com/ai-gateway/configuration/request-handling/`
- TTFT 口径语义逐字命中："The timeout is based on when the first part of the response comes back."（且页面说明首部分（如流式响应）在时限内返回则网关继续等待——正是建议 (c) 引用的语义）
- per-request 头存在：`cf-aig-request-timeout`（示例 `--header "cf-aig-request-timeout: 5000"`，毫秒），"per-request `cf-aig-*` headers that work with any provider endpoint"。

## 来源 2：Portkey Gateway retryHandler.ts + globals.ts — ✅ 属实（main 分支精确命中）

- 命令：`curl api.github.com/repos/Portkey-AI/gateway/contents/{src/handlers/retryHandler.ts,src/globals.ts}?ref=main`（base64 解码，221/310 行）
- retryHandler.ts:84 `let remainingRetryTimeout = MAX_RETRY_LIMIT_MS;`（总时间预算起算）；:129-137：429 且有 Retry-After 头时，`retryAfter >= MAX_RETRY_LIMIT_MS || retryAfter > remainingRetryTimeout` → `retrySkipped = true` + throw 放弃重试（:129 注释 "break the loop if the retryAfter is greater than the max retry limit"）；:138 `remainingRetryTimeout -= retryAfter` 预算扣减——"MAX_RETRY_LIMIT_MS=60s 总时间预算，Retry-After 超预算即放弃"逐点命中（建议引用 :84-148 精确）。
- globals.ts:5 `export const MAX_RETRY_LIMIT_MS = 60 * 1000; // 60 seconds` 精确命中。

## 来源 3：vLLM production-stack request_stats.py — ✅ 属实

- 命令：`curl api.github.com/repos/vllm-project/production-stack/contents/src/vllm_router/stats/request_stats.py?ref=main`（328 行，解码存 /tmp）
- :144-249 确为 prefill/decoding 分阶段计时：:144 `on_new_request`（in_prefill_requests 计数）、:194 `on_request_response`（:199-201 ttft = timestamp - request_start_time 写入 ttft_monitors MovingAverageMonitor）、:202 `on_request_complete`（以 first_token_time 是否置位区分"reached decode stage" vs "failed in prefill stage"，:226-237）。
- "prefill/decoding 分阶段计时，为 TTFT 口径超时提供观测基础"与代码一致（TTFT 一词在代码中显式出现：ttft_monitors/ttft 变量）。

## gap 本地代码引用 — ✅ 全部核实成立（本会话读取）

- manager.rs:150 `reqwest::Client::new()` 精确命中；`grep -n -i timeout manager.rs` 全文仅命中 CB timeout(:120)、Retry-After 解析(:450)、CB 探针(:731)、非流式 60s(:773-774)、限流窗口(:1273-1496)——Client 无任何 connect/read/total timeout，"出站请求可无限挂起"成立。
- manager.rs:773-774 `let timeout_duration = Duration::from_secs(60); timeout(timeout_duration, response_rx)` 精确命中；:769 `enqueued_at: Instant::now()`（建议 :768，差 1 行，入队时刻）→ 超时自入队起算、包含排队，"排队 50s 只剩 10s 执行"的机制成立。
- 两条流式路径无任何 timeout 包裹：execute_stream_request(:839，send :885-891)、execute_passthrough_stream_request(:1139，send :1172-1179)（send 位置与建议一致）。
- permit 存活：:1080 `let _permit = permit; // Hold permit during request`（透传非流式，建议 :1080 精确命中）；流式 :857/:1163 acquire（建议 :1164 命中），且 :1187-1189 注释"permit move 进返回的 Stream，流结束才释放，并发上限对流式生效"+ unfold 状态机（None 分支 drop(permit)）——两条流式路径 permit 均随流存活（代码注释自证，recId=5④）。"上游不返回响应头 → send().await 无限挂起 → permit 已被占住 → 并发放大"链条成立。

## 无法复核项（如实说明）

- 无。本次三项 sources 全部打开并核对；本地代码引用全部读取核实。未运行编译或测试（核实范围为来源真实性与代码引用，不涉及行为实验）。
