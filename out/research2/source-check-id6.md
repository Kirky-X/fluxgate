# 来源核实 — 建议 6（路由负载维度改用在途请求数 in-flight）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（四个来源全部属实，行号/公式/注释逐字命中；gap 本地代码全部核实）**

## 来源 1：LiteLLM least_busy.py (:122-151, :193-215) — ✅ 属实

- 命令：`curl api.github.com/repos/BerriAI/litellm/contents/litellm/router_strategy/least_busy.py?ref=main`（218 行，解码）
- in-flight 计数：:122-123 `log_pre_api_call → _increment(kwargs, 1)`（pre-call +1）；:125-128 `log_success_event → _increment(kwargs, -1)`；:132-135 failure −1；:139-151 async 版本同款——pre-call +1 / success/failure −1 逐行命中。
- 0 下限钳位：:193-194 `if local < 0: set_cache(key, 0, ...)`；:197/:215 `increment_with_floor`（Redis 侧 floor）。
- Redis 降级本地：:195-196 `if redis_cache is None: return`（纯本地模式）；:198-199/:216-217 `except → _warn_unwritable(key, e)`（Redis 读写失败仅警告，本地计数独立先行不受影响）——"0 下限钳位防漂移、Redis 读失败降级本地"属实。

## 来源 2：Kong Gateway least_connections.lua (:27-43) — ✅ 属实

- 命令：`curl api.github.com/repos/Kong/kong/contents/kong/runloop/balancer/least_connections.lua?ref=master`（199 行）
- :27 `bh:insert((addr.connectionCount + 1) / addr.weight, addr)`；:32-33 `updateConnectionCount(bh, addr, delta)`（±1 维护）。
- :39-42 NOTE 注释逐字命中 +1 理由："we use `connectionCount + 1` this ensures that even on a balancer with 0 connections the heighest weighted entry is picked first. If we'd not add the `+1` then any target with 0 connections would always be the first to be picked (even if it has a very low weight)"。

## 来源 3：Higress global_least_request lb_policy.go (:31-96) — ✅ 属实（仓库已迁移，同路径同文件）

- 说明：`alibaba/higress` API 301 → repositories/558188628 → 现 full_name 为 **higress-group/higress**（仓库迁移，非失效）；同路径 `plugins/wasm-go/extensions/ai-load-balancer/global_least_request/lb_policy.go`（300 行）存在。
- :31-96 为内嵌 Redis Lua：:35-53 严格最小选择（`count < current_count` 才换目标）+ 平票蓄水池抽样（`same_count_hits` 递增 + `math.random(same_count_hits) == 1` 换选）；:55 `HINCRBY current_target 1`（选中即计数 +1）——"严格最小 + 平票蓄水池抽样"逐行命中。
- 在途计数随 SSE 长流存活：:284 `func (lb GlobalLeastRequestLoadBalancer) HandleHttpStreamDone(ctx wrapper.HttpContext)` → :293 `lb.redisClient.HIncrBy(..., host_selected, -1, nil)`——递减发生在流完成回调，流式期间计数保持占用，属实。

## 来源 4：vLLM production-stack (routing_logic.py:481-491 + loadaware-routing.rst:42-48) — ✅ 属实

- 命令：`curl api.github.com/repos/vllm-project/production-stack/contents/{src/vllm_router/routers/routing_logic.py,docs/source/use_cases/loadaware-routing.rst}?ref=main`
- routing_logic.py:481 `class LoadAwareRouter(KvawareRouter)`；:486-488 docstring 公式逐字命中："score(i) = matched_tokens(i) / prompt_tokens - beta * relative_load(i)"、"relative_load(i) = (load(i) - mean_load) / max(1, mean_load)"；:490 "routes to the argmax"。
- loadaware-routing.rst:40 "``load`` is the number of in-flight requests on the endpoint"；:44-48 Design notes 第 2 条逐字命中归一化与钳制理由："Load is normalized against the fleet's own mean. An absolute in-flight count has no bounded scale... The denominator is clamped at 1 so an essentially idle fleet reports no imbalance to act on."——建议的"分母钳 1 防近空闲舰队放大"即此句。

## gap 本地代码引用 — ✅ 全部核实（本会话逐一读取）

- src/router/router.rs:613-616 `fn queue_factor_from_len(queue_len) -> 1.0 - (queue_len/100.0).min(1.0)` 精确命中；分母 100 为绝对常数（:615）。
- src/vendor/manager.rs:740-743 `get_queue_length → guard.get(vendor_id).map(|i| i.queue.len())`（:742，建议 :740-743 命中）；注释自证"len() 即 receiver 深度"。
- src/vendor/queue.rs:19 `receiver: Receiver<PendingRequest>`（async_channel）；:41-43 `len() = self.receiver.len()`（建议写 :47-49，实际 len 在 :41-43、:47-49 为 is_empty，行号偏差约 6 行；:49-52 注释"队列长度一律直读 VendorQueue::len()"）。async-channel Receiver::len() 语义 = 已入队未取走消息数——"不含已被 worker 取走、在途执行的请求"成立。
- src/vendor/rate_limiter.rs:40-52 `ConcurrencyLimiter { inner, semaphore: Arc<tokio::sync::Semaphore> }`（:40-41 字段自持信号量、:43-52 构造）——"读 available_permits() 即可零新增状态得 in_flight"的机制基础成立。
- limiteron `/home/dev/projects/limiteron/src/limiters/concurrency.rs:276-285` `async fn peek(&self, cost)`（注释"非消费预检：读可用许可数，不获取"）→ :296-304 `current_snapshot()` 返回 `RateLimitSnapshot { limit, remaining: self.semaphore.available_permits(), reset_secs: 0 }`——"peek(1) 返回 RateLimitSnapshot{remaining}"（建议 :276-304）精确命中。
- src/config/mod.rs:131 `pub max_concurrent_requests: Option<usize>` 精确命中。

## 无法复核项

- 无。四项来源全部打开并核对到行级；唯一注记：Higress 仓库 org 已迁移（alibaba/higress → higress-group/higress，API 301 同仓库），来源真实性与行号不受影响。
