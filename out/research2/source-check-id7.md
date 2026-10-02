# 来源核实 — 建议 7（熔断升级为失败率比例 + 最小样本数，补 429 感知）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，全部阈值数值逐字命中；gap 本地代码全部核实）**

## 来源 1：LiteLLM cooldown_handlers.py:376-416 — ✅ 属实（全部数值命中）

- 命令：`curl api.github.com/repos/BerriAI/litellm/contents/litellm/router_utils/cooldown_handlers.py?ref=main`（672 行）+ `litellm/constants.py`（常量定义）
- 比例 + 最小样本 + 非单部署组（:405-409）：`percent_fails > DEFAULT_FAILURE_THRESHOLD_PERCENT and total_requests_this_minute >= DEFAULT_FAILURE_THRESHOLD_MINIMUM_REQUESTS and not is_single_deployment_model_group` ✅
  - 常量值（litellm/constants.py）：:87-89 `DEFAULT_FAILURE_THRESHOLD_PERCENT = 0.5`（注释 "cooldown a deployment if 50% of requests fail"）、:150-152 `DEFAULT_FAILURE_THRESHOLD_MINIMUM_REQUESTS = 5`（"Prevents cooldown from triggering on first failure"）——"pct>0.5 且 total≥5" 精确命中 ✅
- 429 立即（:400-401）：`if exception_status_int == 429 and not is_single_deployment_model_group: return True` ✅
- 单部署组 100% 门槛（:402-404）：`percent_fails == 1.0 and total_requests_this_minute >= SINGLE_DEPLOYMENT_TRAFFIC_FAILURE_THRESHOLD`（constants.py:147-149 = **1000**，注释 "Minimum number of requests to consider reasonable traffic"）——"total≥1000 的 100% 门槛防误杀" 精确命中 ✅
- 判定异常 fail-safe 到冷却（:266-267）：`except Exception: # Catch all - if any exceptions default to cooling down` ✅；另有 :619 `except Exception as e: # a Redis outage must not stop failing deployments from cooling down` ✅ 双重命中。

## 来源 2：Portkey Gateway circuitBreaker.ts:135-164 — ✅ 属实（2.0.0 分支精确命中）

- 说明：main 分支已无 `src/utils/circuitBreaker.ts`（API 返回无 content），2.0.0 分支存在（与建议 1 核实时的分支一致，建议引用行号与 2.0.0 对齐）。
- `curl api.github.com/repos/Portkey-AI/gateway/contents/src/utils/circuitBreaker.ts?ref=2.0.0`（379 行）：
  - :135 `const minimumRequests = status.cb_config.minimum_requests || 5;` ✅
  - :136-143 样本不足按 0 计：`let currentFailurePercentage = 0; if ((failureCount || successCount) && failureCount + successCount >= minimumRequests) { currentFailurePercentage = (100 * failureCount) / (failureCount + successCount); }`——total < minimumRequests 时百分比保持 0，逐字命中 ✅
  - :157-164 二选一判定：`(failure_threshold && failureCount >= failure_threshold) || (failure_threshold_percentage && currentFailurePercentage >= failure_threshold_percentage) → isOpen = true` ✅
- 引用区间 :135-164 精确命中。

## 来源 3：LiteLLM cooldown_handlers.py:76-87, 561-609 — ✅ 属实

- :76-87 `_EXCEPTION_POLICY_FIELDS`：按异常类型分桶（ContentPolicyViolation/BadRequest/Authentication/Timeout/RateLimit/InternalServerError/ServiceUnavailable/BadGateway/NotFound 各自 `XxxAllowedFails`）——"按异常类型分桶的 allowed_fails_policy" ✅
- :561-609 `should_cooldown_based_on_allowed_fails_policy`：:604-605 `cache_key = f"{base_key}:{cache_key_suffix}"`（按异常类型独立计数）；:606-608 `_increment_allowed_fails(cache, cache_key, ttl=cooldown_time)`——**计数器 TTL = 冷却时长** 逐字命中 ✅
- "分桶仅 LiteLLM 有，Higress/Bifrost 无"：本会话早前已核实 Bifrost 熔断基于响应头（建议 1，无失败计数分桶概念）、Higress global_least_request 为最小连接 LB（建议 6，无 allowed_fails 概念），与该负向声明方向一致；未对两项目做全量代码扫描，此点为间接核实（如实注明）。

## gap 本地代码引用 — ✅ 全部核实（本会话逐一读取）

- manager.rs:120 `CircuitBreakerConfig::new(5, 3, Duration::from_secs(30))` ✅（limiteron failure_threshold=5 为连续失败计数式——types.rs:543/:556/:567 failure 路径 transition_to_open，与 id=1 核实一致）。
- error_map.rs:108-127 `impl ErrorClassifier for NexusFlowErrorClassifier`；:116-118 `Some(("rate_limited", _)) | Some(("validation", _)) | Some(("protocol", _)) => false` 精确命中——RateLimited 不计失败 → "现有 CB 对上游 429 完全无感"成立（CB failure_count 不因 429 增长）。
- metrics.rs:24-38 `SlidingCounter`：:24-36 注释 "60s 窗、10 桶环形（每桶 6s）"、:38 struct 定义——"复用 SlidingCounter 维护 60s 滑窗"的机制基础存在 ✅。
- 路由剔除效果：router.rs:273/:314 `!= VendorStatus::Available → continue`（建议 6 时已核）+ manager.rs:707-709 Throttled（建议 13 时已核）——"软降权不能靠状态值，必须接进权重打折"的架构论据成立。
- papers-resilience 笔记（out/research/papers-resilience.md:21）："7 次查询中 3 次零结果……未找到专门针对'LLM 网关重试策略'的论文"——方向性结论（arXiv 该方向检索弱、成熟实现多在工程界）与笔记一致；但 gap 写"6 次 0 结果"与笔记"3 次零结果（7 次查询）"数字不符（gap 旁注瑕疵，非 sources 项，如实记录）。

## 无法复核项

- 无（就 sources 而言）。三处来源文件全部下载并逐行核对，常量值全部确认；"分桶仅 LiteLLM 有"为负向全称声明，以已核实的 Bifrost/Higress 机制间接支持，未做全量扫描（如实说明）。
