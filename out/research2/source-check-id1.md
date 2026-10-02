# 来源核实 — 建议 1（limiteron 熔断器 slow-call-rate 默认阈值误触发）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，行号命中）**

## 来源 1：Bifrost 企业版熔断文档 — ✅ 属实

- 命令：WebFetch `https://docs.getbifrost.ai/enterprise/circuit-breaker.md`
- 页面存在，描述 Bifrost Enterprise 熔断器基于 HTTP 响应头触发。
- 建议所引原句逐字命中："Trips on HTTP response headers returned by the provider — no latency heuristics or error-rate windows to tune"
- 页面另有 "Bifrost detects the signal in the response headers, opens the circuit, and transparently redirects subsequent requests"；信号匹配三种方式（存在/等于/包含）均针对单一响应头。
- 用途核对：作为对照案例（业界熔断不基于延迟启发式），引文属实。

## 来源 2：Portkey Gateway circuitBreaker.ts（2.0.0 分支）— ✅ 属实

- raw.githubusercontent.com 直连失败（`curl -w %{http_code}` 返回 `000`，连接层失败非 404）。
- 改用 GitHub API（api.github.com 可达）：
  - `curl api.github.com/repos/Portkey-AI/gateway/branches/2.0.0` → 分支存在（commit 8febc1d…）。
  - `curl api.github.com/repos/Portkey-AI/gateway/contents/src/utils/circuitBreaker.ts?ref=2.0.0` → 文件存在（10994 字节，blob c6eab15），base64 解码全文。
- 逐行核对（解码后行号）：
  - `:135` `const minimumRequests = status.cb_config.minimum_requests || 5;` → minimum_requests 默认 5 ✅
  - `:157-164` 打开判定：`(failure_threshold && failureCount >= failure_threshold) || (failure_threshold_percentage && currentFailurePercentage >= failure_threshold_percentage)` → 判定源为 failure_count / failure_threshold_percentage ✅
  - 全文 grep `latency|duration|slow|elapsed` → 零命中 → "代码层面无延迟启发式" ✅
  - 失败判定 `isCircuitBreakerFailure`：`failureStatusCodes?.includes(statusCode) || (!failureStatusCodes && statusCode >= 500)` → 按状态码，无延迟逻辑。
- 建议引用区间 :135-164 与实际代码精确吻合。

## 来源 3：limiteron（本地兄弟库 /home/dev/projects/limiteron）— ✅ 属实

NexusFlow `Cargo.toml:60` `limiteron = { path = "../limiteron", … }` 确认为本地 path 依赖，直接读源码：

- `src/circuit/types.rs:467-472`：execute() 内计时 `let start_time = self.clock.now(); … let elapsed = start_time.elapsed(); … self.record_call_duration(elapsed).await;`（建议写 :465-472，命中）
- `src/circuit/types.rs:580-591`：record_call_duration，`:581` `if elapsed >= self.config.slow_call_duration_threshold` 记慢调用（建议写 :581-591，精确命中）
- `src/circuit/types.rs:596-620`：check_slow_call_rate，rate = slow/total，`≥ slow_call_rate_threshold` 且 Closed 时调用 `transition_to_open()`（调用行 :620，建议写 :596-616，机制主体命中，末尾超出约 4 行，不影响引用实质）
- `src/circuit/types.rs:134-143`：链式 builder `slow_call_duration_threshold`（:134）/ `slow_call_rate_threshold`（:137）存在（建议写 :134-143，命中）
- `src/constants.rs:106`：`DEFAULT_CIRCUIT_BREAKER_SLOW_CALL_DURATION_MILLIS: u64 = 500` ✅
- `src/constants.rs:111`：`DEFAULT_CIRCUIT_BREAKER_SLOW_CALL_RATE_THRESHOLD: f64 = 0.5` ✅
- `src/circuit/types.rs:119-124`：`CircuitBreakerConfig::new()` 未覆盖慢调用字段，默认值确实引用上述两个常量 → "未覆盖即 500ms / 0.5" 成立
- 库自带测试：`types.rs:1401` `test_builder_with_slow_call_config` 验证链式 API；:1292 `test_slow_call_duration_threshold_config`。

## gap 中的本地代码引用 — ✅ 全部命中

- `src/vendor/manager.rs:120`：`CircuitBreakerConfig::new(5, 3, Duration::from_secs(30)).error_classifier(Arc::new(NexusFlowErrorClassifier))`（建议写 :115-123 构造区，命中；仅设置 error_classifier，未覆盖慢调用默认值）
- `src/vendor/manager.rs:674` `fn get_status`；`:695` `CircuitState::Open` 且探针未就绪 → `VendorStatus::Unavailable`（建议写 :692-699，命中；注：探针就绪时放行探测，建议的"Open→Unavailable"是行为主体的简化描述）
- `src/router/router.rs:273`：`if self.vendor_manager.get_status(&vendor.id).await == VendorStatus::Available` → Available 过滤，精确命中
- `grep -n -i "slow\|sleep" tests/circuit_breaker_test.rs` → 零命中（退出码 1）→ "无 slow-call 用例" 属实

## 无法复核项（如实说明）

- gap 中"本会话临时探针实测（tests/zz_slowcall_probe.rs，跑完已删除）"与"评审独立复现"为既往声明，探针文件已不存在，本次无法重放。但其结论与源码机制一致：800ms Ok 调用 → elapsed 800ms ≥ 500ms 记慢调用（types.rs:581）→ rate 1.0 ≥ 0.5（types.rs:596-620）→ transition_to_open，且 on_success 路径不增 failure_count。
- 本次未重新执行探针实验；核实范围为 sources 真实性 + 代码行号引用，均已完成。
