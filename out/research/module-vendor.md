# 研究笔记：NexusFlow 厂商管理模块（src/vendor/）

- 日期：2026-09-30
- 范围：`src/vendor/`（mod.rs, manager.rs, pool.rs, queue.rs, rate_limiter.rs, status.rs, cost.rs, resilience/{mod,circuit_breaker,fallback,retry}.rs），共 2031 行；并追查了调用方（src/api、src/router、src/config、src/utils）与 Cargo.toml。
- 方法：通读全部源码 + grep 全仓库调用点。本环境**无法编译验证**（路径依赖兄弟库 ../dbnexus、../limiteron 等不存在，`cargo check` 报 "failed to read /home/dev/projects/dbnexus/Cargo.toml"；`cargo test`/`cargo clippy` 同理均未运行）。所有结论基于源码阅读；`cargo metadata --no-deps` 可运行，其输出为 feature 结论的依据。

---

## 1. 模块职责

`VendorManager`（manager.rs:115）是网关对上游 LLM 供应商的统一出口，职责五块：

1. **实例生命周期**：启动时按 config 为每个 enabled vendor 创建 `VendorInstance`（manager.rs:195-217 `initialize()`；219-261 `get_or_create_instance`），并 spawn 每厂商一个常驻 worker 消费请求队列。
2. **请求执行（4 条路径）**：队列化非流式、直接流式、零拷贝透传非流式、零拷贝透传流式（见 §2 数据流）。
3. **限流三级**：RPS 令牌桶（`TokenBucket`）+ 并发信号量（`ConcurrencyLimiter`）+ 业务窗口（`AdvancedRateLimiter`：TimeWindow/Interval/Token/None 四态枚举）。
4. **韧性**（代码存在但**编译裁剪**，见 §5 问题 #1）：三态熔断器、FallbackChain、backon 重试助手。
5. **状态与度量**：`VendorState`（4 态原子状态机）、`GlobalMetrics`、`WindowInfo`（喂给 router 的 3D 权重）、成本核算（`CostCalculator` + `CostTracker`）。

另有 `ConnectionPool`（pool.rs）——`reqwest::Client` + `Semaphore` 的并发包装，**生产代码零使用**（仅 tests/pool_test.rs 引用；VendorManager 直接用裸 `reqwest::Client::new()`，manager.rs:145）。

## 2. 数据流

### 2.1 队列化非流式路径（唯一完整路径）

```
API 层 (api/openai.rs:168, api/anthropic.rs:136)
  └─ VendorManager::execute_request (manager.rs:592-624)
       ├─ instances 读锁取 Arc<VendorInstance> (597-605)
       ├─ 构造 PendingRequest{ UnifiedRequest, oneshot::Sender } (609-614)
       ├─ instance.queue.send().await   ← async_channel bounded(100)，满则等待 (616)
       └─ 60s 固定 timeout 包裹 oneshot (618-623)

每厂商常驻 worker（get_or_create_instance 内 tokio::spawn, manager.rs:256-258）
  process_vendor_queue (manager.rs:263-336)，循环：
    1) rate_limiter.acquire().await        ← 先烧 RPS 令牌 (271)
    2) queue.recv().await                  ← 取请求 (274)
    3) advanced_limiter.check() 失败→回 RateLimited (276-281)
    4) concurrency_limiter.acquire().await ← 在 worker 循环内等并发 permit（背压点）(284)
    5) tokio::spawn 每请求任务 (291-331)：
         perform_http_request (338-369)
           ├─ 按 config.type_ 选 converter（openai/anthropic，346-354）
           ├─ request_to_external → JSON body (357)
           └─ execute_http_with_retry (438-470)
                └─ [cfg resilience] CircuitBreaker::call 包裹 (445-465)   ← 编译裁剪，见 #1
                     └─ execute_http_with_retry_inner (474-561) 手写重试循环
           成功 → advanced_limiter.consume_tokens (307-310)
               → [cfg cost-tracking] CostCalculator.calculate + CostTracker.record (314-327)
           → oneshot 回传 (330)
```

### 2.2 手写重试循环（execute_http_with_retry_inner，manager.rs:474-561）

- 网络 timeout/connect 错误：重试 ≤2 次，退避 500ms×次数（495-499）。
- 非 2xx：读 body 记 warn 日志，`ErrorCategory::from_status_code` 分类（utils/retry.rs:40-51：429→RetryableExponential，500/502/503→RetryableFixed，400/401/403/404 及其他→Fatal），可重试且 `retry_count<5` 则退避后重试；指数 1000ms×2^n（n 截到 4），固定 1s（529-539）。
- 2xx：`response.json()` → `converter.parse_response` 转统一格式（549-559）。

### 2.3 三条旁路（不走队列、不走 worker）

| 路径 | 位置 | 限流 | 重试/CB | metrics | token 计量 |
|---|---|---|---|---|---|
| execute_stream_request | manager.rs:626-683 | rate_limiter + advanced check + permit | 无 | 无 | 无 |
| execute_passthrough_request | manager.rs:687-776 | 同上 | 无 | **有**（698-701，全仓库唯一 record 点） | usage.total_tokens 回填（769-773） |
| execute_passthrough_stream_request | manager.rs:779-819 | 同上 | 无 | 无 | 无 |

流式两路径的并发 permit 即取即弃：execute_stream_request 显式 `drop(permit)`（manager.rs:650, 680，注释 "For now, we drop it"）；passthrough stream 的 `_permit` 在函数返回时释放，**不覆盖流的实际传输生命周期**（804-805）。

### 2.4 运维/路由读路径

- `get_status`（manager.rs:563-583）：config.enabled → instances → advanced_limiter.check()（→Throttled）→ VendorState（恒 Available，见 #5）。
- `get_queue_length`（585-590）与 `get_all_status`（857-867）从 `QueueManager` 读——恒 0（见 #3）。
- `get_window_info`（870-931）：把 AdvancedRateLimiter 变体映射为 `WindowInfo{max_tokens, used_tokens, window_end}` 供 router 计算 3D 权重；TimeWindow/Interval 分支用 chrono 本地时间换算成 `Instant`。
- `get_circuit_state` / `force_circuit_open`（172-193）：cfg(resilience)，编译裁剪。

## 3. 关键算法与数据结构

### 3.1 三级限流（rate_limiter.rs）

- **TokenBucket**（17-34）：委托 `limiteron::limiters::TokenBucketLimiter`（path 依赖，Cargo.toml:48；limiteron 源码本环境不存在，`allow()` 内部语义未能核实）。`acquire()` 是 `while !allow(1) { sleep(100ms) }` **轮询**而非通知（29-33）。
- **ConcurrencyLimiter**（39-68）：`Arc<tokio::sync::Semaphore>` + limiteron 包装；`acquire()` 返回 `OwnedSemaphorePermit`（drop 自动释放）。
- **AdvancedRateLimiter**（76-195）：业务窗口枚举。
  - `check()`（128-142）：TimeWindow 按 `Local::now()` 判 `start<=now<end`；Token 判 `used<max`；Interval 恒 true（134）；None 恒 true。
  - `consume_tokens`（144-148）：仅 Token 变体 `fetch_add`。
  - `reset`（150-177）/`should_reset`（179-194）：**should_reset 只对 Interval 变体返回 true**；Token 变体永不触发重置（见 #2）。
  - 后台任务每 60s 扫一遍 instances 调 reset（manager.rs:203-214）。

### 3.2 熔断器（resilience/circuit_breaker.rs，cfg(resilience) 下编译）

- 无锁三态：`ArcSwap<State>` + 3 个 AtomicU64（failure_count/success_count/half_open_calls），82-99。
- 默认参数：failure_threshold=5, success_threshold=3, open_timeout=30s, half_open_max_calls=1（62-71）。VendorInstance 用 `CircuitBreakerConfig::default()`（manager.rs:105）。
- 转换：Closed 下 retryable 失败计数 ≥5 → Open（156-173）；Open 后每次 `call()` 先 `maybe_half_open()`，超 30s 惰性转 HalfOpen 并重置 half_open_calls/success_count（107-116, 125）；HalfOpen 内 `fetch_add` 后第 ≥2 个调用直接返回 `Err(Open)`（131-135）；探测成功 success_count ≥3 → Closed（139-145）；retryable 失败 → 回 Open（146-150）；非 retryable 失败忽略（151）。
- 状态机与 manager 的组合：CB 包裹**整个**重试循环（manager.rs:448-458），即一次 CB 调用最多含 1+5 次 HTTP 尝试；`CircuitBreakerError::Open` 映射为 `VendorUnavailable{vendor: "... (circuit open)"}`（459-464）；失败判定用 `NexusFlowError::is_retryable`（error.rs:119 → utils/retry.rs:162-172，从错误字符串提取状态码）。

### 3.3 FallbackChain（resilience/fallback.rs，同样仅测试引用）

- `targets: Vec<FallbackTarget{vendor_id, model_id, priority}>`，priority 小者优先；`next_after_failure`（83-100）= 触发策略匹配 ∧ 找到 failed 的 priority ∧ 取 priority 更大的最小者。
- 5 种触发策略（25-36）：Http5xx / Timeout / RateLimited / CircuitOpen / All；判定依据是**解析错误字符串**——`is_circuit_open` 靠 `vendor.contains("circuit open")`（104-106），状态码靠 `extract_status_code` 从括号内解析（utils/retry.rs:143-159，失败默认 500）。类型化信息在 `NexusFlowError` 里丢失、退化为字符串协议，是该设计的固有脆弱点。

### 3.4 重试助手（resilience/retry.rs）

- `with_retry`：backon `ExponentialBuilder::default().with_jitter().with_max_times(3)`，重试**所有**错误（33-41）。全仓库无生产调用（grep 证据：仅 utils/retry.rs:20 转出口）；manager 用的是自己的手写循环。两套重试并存且互不相干。

### 3.5 成本核算（cost.rs + utils/cost_tracker.rs，record 点在 cfg(cost-tracking) 下）

- `CostCalculator`（cost.rs:34-80）：`Arc<RwLock<HashMap<(vendor_id, model_id), PricingEntry>>>`；`calculate = (prompt/1000)*in + (completion/1000)*out`；表未命中或锁中毒返回 0.0（55-71）；`update_entry` 支持运行时改价（74-79）。
- `CostTracker`（utils/cost_tracker.rs:27-104）：micro-USD（×1e6 取整 `.round()`）AtomicU64 累加 + DashMap 按 vendor/model 分桶；`record(0.0)` no-op；`snapshot()` 出总账。
- 定价来源：`config.pricing`（config/mod.rs:143-148），config.toml:97-105 配了两条（mock-openai/gpt-4、mock-anthropic/claude-3-opus）。

### 3.6 厂商状态机（status.rs）

- `VendorStatus` 4 态 Available/Busy/Throttled/Unavailable（7-12）存 `AtomicU8`；`VendorState` 另有 `error_count: AtomicU8`（**255 回绕**，17, 50-52）与 `status_changed: AtomicU64`（**只写不读**，41-47）。
- `mark_throttled` 忽略 backoff 参数（70-72），无自动恢复计时。
- **src/ 内无任何 mark_*/inc_error/update_status 调用方**（grep 证据，见 #5）。

## 4. 重点问题的直接回答

- **连接池**：`ConnectionPool`（pool.rs:45-82）是"共享 Client + Semaphore"模式，但生产零使用；VendorManager 用裸 `reqwest::Client::new()`（manager.rs:145，默认连接池参数）。真实并发控制由 `ConcurrencyLimiter`（=100，manager.rs:103）承担。
- **请求队列**：每厂商一条 `async_channel::bounded(100)`（manager.rs:226 硬编码 100），单 worker 消费、每请求 spawn；队列满时 `send().await` 背压，叠加调用方 60s 超时。`QueueManager` 是无写入方的残件（#3）。
- **限流与熔断状态机**：三级限流如 §3.1；熔断器三态机如 §3.2，但**整个被 feature 裁剪，生产二进制中不存在**（#1）；即便存在，默认参数下半开无法收敛（#4）。
- **重试与 fallback 组合**：请求路径内 = 手写分类重试（429 指数 / 5xx 固定 / 超时短重试）；跨厂商 fallback = `FallbackChain` 存在但**零接线**，API 层单 vendor 失败直接冒泡给客户端（api/openai.rs:166-169 无 fallback 循环）。即：**当前生产行为是"重试到死也不换厂商"**。
- **健康检查**：纯被动且残缺——无主动探测任务（后台任务只做 limiter reset，manager.rs:203-214）；被动侧 VendorState 无写入方、GlobalMetrics 只覆盖透传路径。`/health` 是网关自身常量 ok（api/health.rs:17-27）。admin `reset_vendor` 是 no-op（api/admin.rs:108-118）。
- **成本核算**：设计完整（定价表 + micro-USD 累加），但 record 点在 `cfg(cost-tracking)` 下 → 永不编译，运行时成本恒 0（#1）；即便启用，也只覆盖队列路径成功响应，透传不记账（manager.rs:769-773 只回填 token）、流式无 usage 无从记账。

## 5. 发现的问题与优化切入点（按影响排序）

### #1【致命】`resilience` / `cost-tracking` feature 未在 Cargo.toml 声明 → 熔断、FallbackChain、backon 重试、成本核算全部编译裁剪

- 证据：Cargo.toml:16-26 的 `[features]` 表只有 database/default/full/infrastructure/logging/protocol/router/sdforge-integration/vendor-management/web-server（`cargo metadata --no-deps` 实测输出一致，无 tracing-subscriber 之外的隐藏项）；依赖表无 `arc-swap`、无 `backon`（Cargo.toml:31-78）。
- 被裁剪的代码：`src/vendor/resilience/` 整个模块（mod.rs:10）、CB 包裹（manager.rs:445-465）、`IsRetryable for NexusFlowError`（error.rs:119）、`get_circuit_state`/`force_circuit_open`（manager.rs:172-193）、CostTracker（utils/mod.rs:13）、worker 内成本记账（manager.rs:314-327）。
- 后果：AGENTS.md 与项目背景声称的"limiteron GCRA + 熔断器"、"成本核算"在可构建产物中不存在；429/5xx 仅有手写重试，无熔断保护；`cargo build --features resilience` 会直接报 feature 不存在。
- 连带：tests/circuit_breaker_test.rs:1-3 与 tests/fallback_test.rs:17 引用 `nexusflow::vendor::resilience`，默认 feature 下 `cargo test` 这两个目标应编译失败（本环境缺兄弟库无法实证；CI 的 `--all-features`（.github/workflows/ci.yml:65,100）也救不了未声明的 feature）。
- 优化切入：在 `[features]` 补 `resilience = ["dep:arc-swap", "dep:backon"]`、`cost-tracking = []` 并加入 `full`；或删除死代码。二选一，当前状态最差（代码在、编译不出）。

### #2【高】Token 窗口 `used_tokens` 永不重置 → 窗口耗尽后厂商永久 RateLimited

- 证据：`should_reset` 只 match Interval（rate_limiter.rs:179-194 `_ => false`）；后台任务仅在 `should_reset()` 为真时调 `reset()`（manager.rs:207-212）；`reset()` 虽支持 Token 变体（rate_limiter.rs:160-174）但无调用方；admin `reset_vendor` 是 no-op（api/admin.rs:108-118）。
- 后果：`consume_tokens` 单调累加（manager.rs:307-310, 769-773），达到 `max_tokens`（config.toml 两个 vendor 均为 10,000,000）后 `check()` 永假（rate_limiter.rs:135-139）→ 该厂商所有新请求永久返回 RateLimited，直到进程重启。
- 优化切入：给 Token 变体实现基于 `last_reset` 的窗口判定（复用 Interval 的 60s 扫描），或在 `check()` 内惰性重置。

### #3【高】`QueueManager` 无写入方 → 队列长度恒 0，3D 权重的 queue_factor（×0.3）恒定失效

- 证据：queue.rs:59-69 只有 `new`/`get`，全仓库无 insert；manager.rs:586, 862 从中读取 → 恒 `unwrap_or(0)`；真实队列 `instance.queue.len()` 从未暴露。
- 后果：router.rs:280-282 → compute_weight（router.rs:290-300）中 `queue_factor_from_len(0)=1.0` 恒定；admin 端点报的 queue_length 恒 0（api/admin.rs:84, 98,103）。3D 权重实际只有 2D。
- 优化切入：`get_queue_length`/`get_all_status` 改读 `instance.queue.len()`（VendorQueue 已有 `len()`，queue.rs:39-41），删掉 QueueManager。

### #4【高】熔断器默认参数下半开（HalfOpen）无法收敛到 Closed → 一旦熔断过就永久拒绝

- 证据：HalfOpen 入口 `half_open_calls.fetch_add` 后 `calls >= half_open_max_calls(默认1)` 即拒绝（circuit_breaker.rs:131-135）；计数器只在 Open→HalfOpen 转换时清零（107-116）；首个探测成功后 success_count=1 < 3，状态停留 HalfOpen，**后续所有调用永远 Err(Open) 且不再重置任何计数器**（125 行的 maybe_half_open 对 HalfOpen 是 no-op）；非 retryable 失败同样卡死（151 `_ => {}`）。
- 测试掩盖：half_open_three_successes_close 用 `half_open_max_calls: 10` 绕过（tests/circuit_breaker_test.rs:92-97），从未覆盖默认组合 (1,3)。
- 优化切入：探测成功后重置 `half_open_calls`（或每次探测完成即清零，实现"串行探针"语义）；或将默认 `success_threshold` 降为 1；并发探针需处理 `half_open_max_calls>1` 时的计数泄漏。

### #5【高】`VendorState` 全套变更方法零调用 → 可用性状态机制形同虚设

- 证据：`update_status`/`mark_busy`/`mark_available`/`mark_throttled`/`inc_error`（manager.rs:821-855）在 src/ 内除定义外无调用（grep "mark_busy|mark_available|mark_throttled|update_status|inc_error|reset_error" 仅命中定义与 status.rs）；status.rs:62-76 同。
- 后果：`get_status` 恒 Available → router 的可用性过滤（router.rs:105, 181, 207）永不剔除坏厂商；`error_count: AtomicU8` 255 回绕（status.rs:17）；`mark_throttled` 丢弃 backoff、无自动恢复（status.rs:70-72）；`status_changed` 只写不读（status.rs:41-47）。
- 优化切入：在 worker 的请求任务中接线（失败 `inc_error`+按阈值 mark_unavailable、成功 mark_available），backoff 用 `status_changed` 时间戳实现自动恢复；error_count 升 AtomicU64。

### #6【高】流式与透传路径绕过全部韧性设施

- 证据：三条旁路无 CB 包裹、无重试（manager.rs:626-683, 707-776, 779-819；CB 包裹仅存在于队列路径 445-465）；execute_stream/passthrough_stream 不记 metrics（全仓库唯一 record 点 698-701 在 passthrough 非流式包装层）；流式路径从不 `consume_tokens`（Token 窗口对流式流量零计量）；并发 permit 即取即弃（650, 680, 804-805），流式传输期间并发上限不受控。
- 优化切入：至少给透传非流式接 CB + 分类重试（它已具备 metrics/token 钩子，改动最小）；流式的 permit 需移入 stream 响应体生命周期（`OwnedSemaphorePermit` move 进 async stream 闭包）。

### #7【高】worker 空闲烧令牌 + 高级限流 check-then-act 竞态

- 证据：`rate_limiter.acquire()` 在 `queue.recv()` 之前（manager.rs:271-274），空转时 RPS 令牌被无主消耗，突发到达时桶空、每请求吃最多 100ms 轮询延迟；`advanced_limiter.check()`（276）与成功后的 `consume_tokens`（307-310）非原子，并发下 token 预算可被超卖。透传/流式路径同序（723-730, 795-802）。
- 优化切入：循环改为先 `recv()` 再 `acquire()`；Token 窗口改用 `fetch_add`+比较回滚或 CAS 预扣。

### #8【中】配置热更新对 vendor 实例不生效

- 证据：`initialize()` 仅启动时调用一次（main.rs:41-42）；`/admin/config` → `ConfigManager::update_config`（config/manager.rs:30-50）只更新内存 Config + 写 config.toml，不通知 VendorManager；`get_or_create_instance` 命中即返回旧实例（manager.rs:222-224）；`instance.config` 是创建时克隆的快照（99）。
- 后果：热更新新增 vendor 无实例（execute_request 直接 VendorUnavailable）；改 api_base/rate_limit/pricing 对存量实例无效（成本定价表 `CostCalculator::from_config` 也只在构造时建一次，manager.rs:135-139）。
- 优化切入：update_config 后 diff vendor 列表，新建缺失实例 + 更新存量实例 config（ArcSwap<VendorConfig> 可无锁热替换）。

### #9【中】重试实现碎片化、参数硬编码且互相矛盾

- 证据：manager 内联手写循环 max_retries=5（manager.rs:484），但 ErrorCategory 声明 429→5 次、5xx→3 次（utils/retry.rs:73-80）、RetryStrategy/BackoffStrategy（utils/retry.rs:85-140）无人使用；backon 版 `with_retry` 重试所有错误且零调用（resilience/retry.rs:33-41）；CB 包裹时 `request_body.clone()` 每请求多拷贝整个 JSON body（manager.rs:453，closure 为 FnOnce 本可 move）；重试总退避可达 31s+ 但调用方 60s 超时截断后上游请求继续执行、结果被 `let _ = send` 丢弃（manager.rs:330, 619-623）——超卖上游配额。
- 优化切入：统一到一套重试策略（RetryStrategy 数据驱动，从 vendor config 读取）；CB 闭包 move body；oneshot 断开时 abort 上游任务或至少计数。

### #10【中】同步锁用于 async 热路径 + 锁嵌套

- 证据：`instances`/`circuit_breakers` 用 `std::sync::RwLock` + `unwrap()`（manager.rs:117, 125, 143, 150；poison 即 panic 传染）；`get_or_create_instance` 持 instances 写锁期间再取 circuit_breakers 写锁（219-248）；`GlobalMetrics::record_request` 每请求取 per_vendor 写锁（utils/metrics.rs:45-57）。持锁段短、未跨 await（不会死锁），但高并发下是串行化点，且 `get_window_info` 在读锁内做多次 chrono 系统调用（manager.rs:874-912）。
- 优化切入：instances 改 `DashMap` 或 `ArcSwap<HashMap>`（读多写极少）；metrics per_vendor 改 DashMap（CostTracker 已是此模式，utils/cost_tracker.rs:31-33）。

### #11【低】配置硬编码与容量耦合

- 证据：queue_size=100 硬编码（manager.rs:226）且一值三用——队列容量、令牌桶容量（`TokenBucket::new(queue_size, rps)`，102）、并发上限（103）；`ConcurrencyConfig` 只有 rps（config/mod.rs:104-108），突发容量/最大并发均不可配。
- 优化切入：VendorInstance::new 拆分为三个独立配置项（queue_capacity / burst / max_concurrent）。

### #12【低】杂项

- TimeWindow 跨午夜不支持：`now>=start && now<end` 在 start>end 时永假（rate_limiter.rs:130-133）；解析失败静默回退为全日开放 00:00-23:59（97-100），坏配置无告警。
- `Interval` 变体 `check()` 恒 true（rate_limiter.rs:134）——Interval 只延迟 reset，不做准入。
- api_key 每请求 `std::env::var`（manager.rs:409）。
- `VendorQueue::send` 错误消息硬编码 "queue_closed" 丢失 vendor 上下文（queue.rs:30-32）。
- `build_vendor_url` 以 `/v1` 后缀猜测拼接（manager.rs:387-397），非 `/v1` 风格的自定义网关（如 Azure）不可表达。
- 错误分类依赖字符串解析（`extract_status_code` 括号协议 + 默认 500，utils/retry.rs:143-159；`is_circuit_open` 靠子串，fallback.rs:104-106）——脆弱的类型擦除。
- AGENTS.md 声称 "0 编译告警"，但未声明的 `cfg(feature = "resilience")` 理论上应触发 `unexpected_cfgs` 告警（Cargo.toml:28-29 只豁免了 "openapi"）——无法在本环境编译验证，仅记录推断。

## 6. 已核实为零使用/残件的组件（可删除或接线）

| 组件 | 位置 | 状态 |
|---|---|---|
| ConnectionPool / OwnedReqwestClient | pool.rs | 仅 tests/pool_test.rs 引用 |
| QueueManager | queue.rs:48-69 | 无 insert，读取恒 None |
| FallbackChain / FallbackTarget / FallbackTrigger | resilience/fallback.rs | 仅 tests/fallback_test.rs（且编译被 #1 裁剪） |
| with_retry (backon) | resilience/retry.rs:33-41 | 零调用 |
| RetryStrategy / BackoffStrategy | utils/retry.rs:85-140 | 零调用 |
| VendorState 变更方法 | status.rs:62-76 | 零调用 |
| status_changed 字段 | status.rs:41-47 | 只写不读 |
| CostTracker / cost_tracker.rs | utils/cost_tracker.rs | record 点被 cfg 裁剪 |

## 7. 验证状态声明

- 已运行：`grep`/`cat`/`sed` 全部源码阅读；`cargo metadata --no-deps --format-version 1`（成功，features 输出为 §5#1 依据）。
- 未运行（环境缺路径依赖 ../dbnexus 等，`cargo check` 报 "failed to read /home/dev/projects/dbnexus/Cargo.toml"）：`cargo check`、`cargo test`、`cargo clippy`——§5 中"编译应失败/应告警"类结论均为静态推断，未实证。
- 未能核实：limiteron 兄弟库源码（`TokenBucketLimiter::allow`、GCRA）不在本环境，§3.1 对其描述仅基于 NexusFlow 侧调用方式。
