# NexusFlow 算法引入/优化实施交付报告

**日期：** 2026-10-02
**对象：** 基于 `out/nexusflow-algorithm-research-report.md`（19 条算法引入/优化建议）的实施交付
**仓库：** `/home/dev/projects/NexusFlow`（crate 名 `nexusflow`，非 git 仓库，全程未运行任何 git 命令）
**方法来源：** 实施方案（19 条 recId + 波次策略）与各波执行记录，见本报告第 2/5 节；报告中的行号凡标注「本轮核实」者均为撰写时在当前源码上 grep 实测，其余为各波合入时点记录（后续合入可能使行号漂移）。

---

## 1. 执行摘要

### 1.1 做了什么

按评审定稿的实施方案，19 条算法引入/优化建议分 **6 个波次串行合入**（评审可并行、合入必须串行，按文件冲突实配顺序），**全部落地，无顺延**（deferred 为空）：

| 波次 | 内容 | MoSCoW |
|------|------|--------|
| W1 | #1 熔断器慢调用阈值、#5 重试抖动、#13 时段门控跨午夜 | must ×3 |
| W2 | #4 出站 HTTP 超时治理、#3 入站限流落地 | must ×2 |
| W3 | #2 熔断/重试/换路覆盖 4 条执行路径（分两期） | must ×1 |
| W4 | #6 in-flight 负载维度、#8 延迟口径分道 + Peak-EWMA + min+buffer | must + should |
| W5 | #7 失败率软降权、#9 重试放大守门器 + 预算、#19 判定改进包、#10 会话亲和软化 | should + could |
| W6 | #14 SLA 虚拟队列、#15 LinUCB、#16 优先级准入、#17 LPS 延迟前瞻、#11 滑动窗口、#12 缓存 TTL 联动、#18 token 加权票据 | should + could ×7 |

MoSCoW 统计：must 7 条（#1/#5/#13/#4/#3/#2/#6）、should 8 条（#8/#7/#9/#10/#14/#15/#16/#17）、could 4 条（#19/#11/#12/#18）。

### 1.2 门禁结论

- **各波合入门禁（材料记录）：** 每波合入后均运行 `cargo check --tests --all-features` + `cargo test --all-features`，退出码 0；W2/W4/W5/W6 另有复审整改，整改后 check/test 再次通过（第 5 节逐波明细）。
- **本轮收尾抽查实跑（撰写本报告时）：**
  - `cargo check --tests --all-features` → **退出码 0**（存在 8 条 rustc warning，全部位于 `tests/api_test.rs` 的 unused imports/unused variable，属测试目标 lint，不在 `cargo clippy` 生产门禁的 target 范围内，详见 5.3 节）；
  - `cargo clippy --all-features -- -D warnings` → **退出码 0，0 error**（唯一 warning 为第三方依赖 proc-macro-error2 v2.0.1 的 future-incompat 提示，基线即有）；
  - `cargo test --all-features` → **退出码 0，23 个测试目标全部通过，累计 627 通过 / 0 失败**。

### 1.3 默认关闭原则落实

- **修 bug 类行为修复直接生效**（#1/#5/#13/#4），其中 #4 带三参数 0=禁用逃生阀、#5 带 spread_jitter=0 回滚阀；
- **#3 激活键控于新形态**：`rate_limit.rules` 数组为空（含随仓 config.toml 示例）→ 与现状逐位一致；
- **#6 默认档即现状**：`load_signal = "queue"`；
- **其余 13 条全部开关化默认关闭**，采用两层锁定：(a) `tests/config_toml_example_test.rs` 逐项断言随仓示例默认值（配置面结构性锁定）；(b) 每个新算法带「关闭态 == 基线」对照测试（行为面等价锁定）。总表见第 4 节。

---

## 2. 逐波实施明细

> 每条按「改了什么 / 涉及文件 / 默认状态与开关 / 测试」记录。行号除注明外为合入时点记录；关键符号本轮已核实现存。

### W1 — 判定式缺陷修复（must ×3）

#### #1 修复熔断器 slow-call-rate 默认阈值对 LLM 延迟的误触发

- **改了什么：** 提取 `pub(crate) fn nexusflow_circuit_config()`（`src/vendor/manager.rs:154`，本轮核实）：`CircuitBreakerConfig::new(5, 3, 30s)` 后链式 `.slow_call_duration_threshold(60s).slow_call_rate_threshold(1.0)`，生产构造（VendorInstance::new，`manager.rs:197-199`）与测试收敛于此。语义依据（材料核实）：limiteron `types.rs` 的 slow-call 判定为 `elapsed >= duration_threshold` 与 `rate >= rate_threshold`（均 ≥ 比较）。duration=60s 抬高慢调用门槛，rate=1.0 使慢调用路径仅在窗口内 100% 调用皆慢时可能触发；混合流量下慢调用路径失活，熔断由真实失败驱动。与 #4 自洽：#4 落地后非流式/TTFT 默认 60s 超时使成功调用恒 <60s；但 W1 先于 W2 且运维可调大 timeout_ms，rate=1.0 是必要防线。
- **文件：** `src/vendor/manager.rs`、`tests/circuit_breaker_test.rs`（方案文件列表中的 `src/lib.rs` 未改动——helper 为 `pub(crate)` 不跨 crate 边界，无需导出，W1 记录声明）。
- **默认状态：** 行为修复，无开关。生产 CB 慢调用阈值由 limiteron 默认 500ms/0.5 变为 60s/1.0；唯一残余触发场景「窗口内 100% 调用皆慢且全成功」与失败路径条件重合（doc 注释声明）。
- **测试：** 双落点。生产构造证明（crate 内可见性）：`src/vendor/manager.rs:2066` `slow_ok_calls_do_not_open_production_circuit`（3×800ms 真睡眠保持 Closed）+ `manager.rs:2090` `nexusflow_circuit_config_values`（断言 60s/1.0）；limiteron 语义隔离证明（集成侧自建 CB）：`tests/circuit_breaker_test.rs:126` `slow_call_rate_threshold_one_requires_all_slow`（1 快 + 2 慢 → 全程 rate 严格 < 1.0 → Closed）。证明力边界如实声明：集成测试证明 limiteron 参数语义，单测证明生产接线使用修复后参数，两者合一才完整。
- **方案偏差（W1 记录）：** 方案设想的「2 慢 + 1 快 → rate 0.67 < 1.0」序列不可达——limiteron `check_slow_call_rate` 为逐调用判定、无最小样本数，total=1 时 rate 恒 1.0，慢调用先到即 Open；语义隔离测试改为「1 快 + 2 慢」，并声明「首调用即慢的 1/1=1.0 残余触发」边界（生产主防线是 duration=60s）。Red 实测比方案预期更严重：旧默认下单次 800ms 成功调用即 Open。

#### #5 Retry-After 与 5xx 固定间隔分支补全范围抖动

- **改了什么：** `retry_delay_for` 增加 `rng: &mut dyn rand::RngCore` 注入参数（对齐 `src/router/router.rs` 的 `weighted_select_with` 注入模式）；两分支改为 `base + U(0, spread_jitter) × base`（`spread_delay` 纯函数，`src/utils/retry.rs:270-282`，本轮核实；gen_range 半开上界 + saturating_add）：Retry-After 分支 base=ra → 延迟 ∈ [ra, 2×ra)，RetryableFixed（5xx）分支 base=1s → ∈ [1s, 2s)。`RetryDelayPolicy`/`RetrySection` 增 `spread_jitter` 字段（`retry.rs:188`，serde 默认 1.0）；`manager.rs` 两调用点传 `&mut rand::thread_rng()`。429 无头指数分支（走 limiteron `delay_for_attempt` 的 `[retry].jitter=0.2`）与 RetryableShort 零改动。
- **文件：** `src/utils/retry.rs`、`src/vendor/manager.rs`、`src/config/mod.rs`、`config.toml`（`[retry] spread_jitter = 1.0`，`config.toml:112`，本轮核实，注释写明回滚路径）、`tests/retry_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** 行为修复，默认 spread_jitter=1.0（全范围展宽即修复本体），同上游指示下并发/多实例不再同刻唤醒。回滚阀 = `spread_jitter = 0`（Retry-After 原样返回、5xx 固定 1s），由专项测试锁定；`[retry]` 段热读。
- **测试：** `tests/retry_test.rs:344` `retry_after_gets_full_range_spread`、`:370` `retryable_fixed_gets_full_range_spread`、`:394` `spread_jitter_zero_restores_legacy_delay`（回滚语义显式锁定）；固定种子精确值经实跑固化 StdRng(42)→1.526557409s。存量测试 `retry_after_header_takes_precedence`/`fixed_category_uses_constant_one_second` 经 deterministic_policy 的 spread_jitter=0.0 保留旧语义断言。Red 实测：修复前两分支恒等（Retry-After=7s、5xx=1s）。

#### #13 修复 TimeWindow 时段门控的跨午夜判定缺陷

- **改了什么：** `src/vendor/rate_limiter.rs:30`（本轮核实）新增纯函数 `pub fn time_window_active(now, start, end)`：同日 `start <= now < end`；跨午夜 `now >= start || now < end`（左闭右开边界注释声明）。两处调用点统一委托：`check()` 时段门控与 `manager.rs` `get_window_info` 的窗口外分支（后者喂 window_factor 参与路由权重，原判定同样不感知跨午夜）。
- **文件：** `src/vendor/rate_limiter.rs`、`src/vendor/manager.rs`、`tests/vendor_test.rs`。
- **默认状态：** 行为修复。跨午夜配置（如 22:00–06:00）从整段失效变为正常生效。进阶项未做（代码注释注明）：跨午夜窗口内 window_end 的精确次日时刻（当前按同日插值退化，window_factor 保守减半但不整段排除）；per-window 时区字段。
- **测试：** `tests/vendor_test.rs:1729` `time_window_crosses_midnight`（本轮核实）：02:00/23:00 true、12:00 false、now==start true、now==end false；+ 同日语义回归 + `get_window_info` 窗口外 `window_end: None` 回归（时钟无关构造）。Red 为 E0432（函数不存在）。

### W2 — 超时治理与入站限流（must ×2）

#### #4 出站 HTTP 超时治理：零超时 + 60s 硬编码含排队 + 流式无超时

- **改了什么（三层模型，每参数 0=禁用逃生阀）：**
  1. `CONNECT_TIMEOUT = 10s` 常量（`src/vendor/manager.rs:224`，本轮核实），`VendorManager::new` 改 `Client::builder().connect_timeout(...)`（`manager.rs:239`）；
  2. `VendorConfig` 增 `timeout_ms=60000` / `stream_ttft_ms=60000` / `stream_idle_ms=30000`，均 0=不限时（显式跳过包裹，防 `Duration::from_millis(0)` 立即超时语义）；新 helper `apply_attempt_timeout`（`manager.rs:671`）在重试与透传路径的 RequestBuilder 上按 0-约定链 `.timeout()`——per-attempt 从 send 起算，重试各自独立计时；
  3. `execute_request` 外层预算改为 `QUEUE_WAIT_BUDGET`（60s 常量，`manager.rs:230`）+ `timeout_ms`（0 时不加执行段）——排队与执行预算解耦（`manager.rs:1065-1076`）；两条流式路径 send 按 0-约定包 TTFT 超时（头到达前超时 → Err(Timeout)，流对象不产生），两处 unfold 循环对 `bytes.next()` 包 idle 超时，超时产出 Err(Timeout) 终止流（permit 随 unfold 终止释放）。透传流公开签名错误类型由 `reqwest::Error` 改为 `NexusFlowError`（idle 超时的必要改动），下游 `reframe_passthrough_stream` 泛型兼容、调用点零修改。
- **文件：** `src/config/mod.rs`、`src/vendor/manager.rs`、`config.toml`（三参数示例 + 0=禁用注释）、`tests/vendor_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** 行为修复，默认值即修复本体：connect 10s / 非流式 per-attempt 60s / TTFT 60s / idle 30s，总预算 = 排队 60s + timeout_ms。逃生阀：三参数任一置 0 对应层不限时（config.toml 注释声明，防慢生成流被 idle=30s 误杀的部署可关）。timeout_ms/stream_ttft_ms/stream_idle_ms 经 ConfigManager 热读，热更立即生效。
- **测试：** `nonstream_timeout_returns_timeout_error`、`timeout_zero_disables_per_attempt_budget`（mockito chunked-body 延迟桩）、`stream_idle_timeout_terminates_mid_stream`、`stream_ttft_timeout_fails_before_headers`（手写 tokio TcpListener 桩：accept 后延迟 1.2s 才写响应头——mockito 1.x 无「头前延迟」原语，实测 idle 桩量不出 TTFT 口径）；config_toml_example_test 断言三个默认值。
- **方案偏差（W2 记录）：** 任务引用行号已漂移（如 `Client::new()` 实际 :177、外层 60s 实际 :815），按现源码定位；worker 路径（`execute_http_with_retry_inner`）的 per-attempt 超时按现状语义归类 is_transient 参与 RetryableShort 重试，非流式 Timeout 断言由透传路径承载。

#### #3 入站限流落地：[rate_limit] 段被整段丢弃

- **改了什么：**
  1. `Config` 增 `#[serde(default)] rate_limit: RateLimitSection`（`src/config/mod.rs:982`）与 `RateLimitRule`（`mod.rs:989`）：`{ id, path: Option<String>（前缀匹配，None=全局）, per_tenant（默认 true）, window_secs, max_requests }`；serde 无 `deny_unknown_fields`，旧扁平字段继续被忽略（从未被消费，无既有行为可保）；
  2. `create_router` 经 `build_inbound_limits`：rules 为空保持 `Governor::new()` 不变（逐位现状）+ 空端点表；path=None 全局规则转 limiteron Rule（Matcher::User + SlidingWindow + Reject）经 Governor builder 构造期注入；path=Some 规则进中间件端点表 `EndpointLimitTable`（limiteron `ShardedSlidingWindowLimiter` 原语直接实例化），每 (rule_id, tenant) 一只惰性创建，超限 429 + Retry-After（reset_secs）；
  3. 中间件 State 从裸 `Arc<Governor>` 改为专用 `struct InboundLimitState { governor, endpoint_table }`（`src/api/router_builder.rs:32`，本轮核实——#16 挂载基线）；租户摘要经 `cache_api_key_hash` 先例（无 key 归 sha256(b"anonymous") 单桶），RequestContext 注入 `X-User-Id` 供 Matcher::User 匹配；健康检查白名单引用补全（router_builder.rs ↔ middleware/auth.rs）；
  4. config.toml `[rate_limit]` 段替换为新形态（`config.toml:68`，rules 空 + 注释示例 + restart-required 声明）。
- **文件：** `src/config/mod.rs`、`src/api/router_builder.rs`、`src/middleware/auth.rs`、`src/api/response_cache.rs`、`src/config/manager.rs`、`config.toml`、`tests/config_toml_example_test.rs`、`tests/rate_limit_test.rs`（新）。
- **默认状态：** 激活键控于新形态——rules 为空（含随仓示例）→ Governor 默认兜底 + 无端点表，与现状逐位一致，`config_toml_example_test` 的 `rules.is_empty()` 结构性断言锁定；`[rate_limit]` 标注 restart-required（热下发列二期）。
- **测试：** config 解析 fixture（字段与 per_tenant 缺省断言）；`tests/rate_limit_test.rs` 实跑 7 通过：max_requests=2 第 3 同租户请求 429+Retry-After、不同 Bearer key 不受影响、path 前缀只约束匹配端点、健康检查配额打满后仍放行、无 Authorization 全归 anonymous 单桶、path=None 规则经 Governor 生效、空 rules 冒烟。
- **方案偏差（W2 记录，均以源码为准）：** ① 匿名语义精确化：有 key 但 auth 关闭的请求按 key 摘要分桶（与 cache_api_key_hash 先例实现一致）；② path=None 全局规则的 per_tenant 在 Governor 层不生效——limiteron 决策链为规则级单例桶（chain.check() 无 context 参数），需要租户隔离配额的规则须配 path 前缀，已在 config.toml 注释与代码 doc 声明；③ 全部规则带 path 时 Governor 注入 rules 为空而 limiteron validate 要求非空，该情形 Governor 保持默认兜底 + 端点表承接；④ 任务背景的 GCRA 声明与源码不符确认（GcraLimiter 在 nexusflow 零使用），本条以 limiteron 真实原语落地。

### W3 — 韧性覆盖 4 条执行路径（must ×1）

#### #2 熔断/重试/换路覆盖到透传与流式路径

- **改了什么：**
  - **配置面：** 新增 `ResilienceSection`（`cover_all_paths`，serde/derive Default=false）作为 Config 结构体 Section 字段（`src/config/mod.rs:844`，本轮核实）——满足硬约束：`persist_and_swap` 以 `toml::to_string_pretty` 全量写回 config.toml，未进 Config 的段会在首次 `POST /admin/config` 热更时从磁盘消失；config.toml:288-290 新增 `[resilience]` 段（含运维影响面注释）；config_toml_example_test.rs 结构性断言默认 false；config_test.rs 另加「热更写回不抹段且取值不丢」测试。
  - **一期（透传非流式）：** manager 侧 `execute_passthrough_request_at` 改经 CB 包裹路径（与 `execute_http_with_retry` 同款泛型 execute + to_limiteron/from_limiteron）；api 层新增泛型 `execute_dispatch_with_fallback`（`src/api/mod.rs`，镜像既有 `execute_request_with_fallback` 的 excluded 累积 + route_excluding + should_trigger + max_total_attempts 结构），openai/anthropic 透传非流式调用点开关开启时走之。
  - **二期（流式首字节前换路）：** manager 抽出 `dispatch_stream_once`（`manager.rs:1146`，本轮核实；send + check_stream_status 段，开启时 CB 包裹、时长只计到响应头，与首字节边界一致），两条流式路径共用；api 层 4 条流式分支把单次派发包成换路循环——Err（均在响应头前）排除重派，Ok 即交出 body，流中错误仍以 SSE error 事件终止不再换路；「透传无 fallback」注释随之删除。actual_vendor/亲和绑定/缓存键均以换路后最终 decision 记账。
- **文件：** `src/config/mod.rs`、`config.toml`、`tests/config_toml_example_test.rs`、`tests/config_test.rs`、`src/vendor/manager.rs`、`src/api/mod.rs`、`src/api/openai.rs`、`src/api/anthropic.rs`、`src/api/response_cache.rs`、`src/config/manager.rs`、`tests/fallback_test.rs` 等；另随附机械改动：Config 新字段致 20 处测试字面量按 E0063 补 `resilience: ResilienceSection::default()`（纯编译面，无行为变化）。
- **默认状态：** `[resilience] cover_all_paths` 默认 false（Config Section 字段落地，缺段/缺字段 serde default=false 结构上锁定）；关闭时三条裸路径（透传非流式、透传流式、转换流式）与开关前行为逐位一致——manager 侧 CB 包裹与 api 层换路循环全部被运行时开关短路；转换非流式的既有换路不受影响。
- **测试：** `tests/fallback_test.rs:346`（本轮核实）新建 cover_all_paths 模块，从零搭 mockito 双 server 基建，7 用例：passthrough_nonstream_fails_over（A 500→B 成功且 request_logs.actual_vendor==fb-b）、stream_fails_over_before_first_byte（A 503→重派 B 拿到 SSE）、mid_stream_error_does_not_failover（B 流中 error 事件→终止不重派，A mock expect(1)）、cb_open_triggers_fallback（5 次烧熔 A→Open→后续请求 A 零新增触达）、cb_open_rejects_passthrough_http_before_dispatch（direct 路由绕过路由层预过滤→派发层 CB 拒绝、零 HTTP）、cover_off_no_failover_nonstream/stream（开关=false 对照：健康 B expect(0) 零调用，防门控失效）。
- **关键决策与偏差（W3 记录）：** ① 换路实现为泛型闭包形态而非方案点名的独立函数——稳定 Rust 下 Fn 返回的固定 Future 无法表达借用闭包参数的高阶生命周期（HRTB+BoxFuture 亦编译失败，实际试过两条路），owned captures 的 clone 成本相对 HTTP 往返可忽略；② **same_protocol_only 协议过滤**（方案未提，源码事实驱动）：透传载荷不可跨协议，route_excluding 不按 vendor type 过滤，跨型候选会收到错误协议 body，故 type 不符的候选计入排除集跳过不发起 HTTP；③ reqwest 0.13 RequestBuilder 无 Clone，`dispatch_stream_once` 内 per-attempt try_clone（None 映射 Config 错误）；④ embeddings 端点经同一入口，开关开启时自然获得 CB 包裹；⑤ tests/api_test.rs 最终无改动（既有流式测试作为回归全数通过）。

### W4 — 负载与延迟信号（must + should）

#### #6 路由负载维度改用在途请求数（in-flight）

- **改了什么：** 取报告实现①（零新增状态）：`ConcurrencyLimiter` 构造时保存入参为新增字段 `max_concurrency`（`src/vendor/rate_limiter.rs:67/:71-76`，本轮核实），新增 `pub fn in_flight() = max.saturating_sub(semaphore.available_permits())`（`rate_limiter.rs:85-92`）；`VendorManager` 镜像 get_queue_length 锁模式新增 `get_in_flight`（`manager.rs:982`）/`fleet_mean_in_flight`；`Router::calculate_weight` 按 `[routing].load_signal` 单选负载输入——"queue"（默认）→ get_queue_length 现状、"in_flight" → get_in_flight、"in_flight_relative" → 纯函数 `relative_in_flight_to_len`（`src/router/router.rs:1393`，(x−mean)/max(1,mean)，线性映射回 0..100 输入域，clamp 后传 compute_weight，签名未动，门控全在调用侧，`router.rs:1068-1070`）。决策已写入字段文档与 config.toml：排队深度不叠加为次级加权项（负载维输入是单选——叠加会双计同一请求且系数不可分）。
- **文件：** `src/vendor/rate_limiter.rs`、`src/vendor/manager.rs`、`src/router/router.rs`、`src/router/mod.rs`（relative_in_flight_to_len 再导出，对齐 compute_weight 先例）、`src/config/mod.rs`、`config.toml`（`load_signal = "queue"`，`config.toml:170`）、`tests/router_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** `load_signal` 默认 "queue"（与历史 3D 权重逐位一致）；config_toml_example_test 断言；未知档位值按 "queue" 处理（回滚阀）。
- **测试：** rate_limiter 单测 ×2（acquire n→in_flight==n、Drop 归零、max 字段直接断言）；router 纯函数单测（8 组边界含 mean=0 钳制）；mockito 集成 `in_flight_load_signal_lowers_weight_only_when_selected`——max_concurrent_requests=2、两条慢 chunked 响应占满 permit→get_in_flight==2 且队列深 0，热更逐档断言：in_flight 档 busy 严格低于 idle、relative 档饱和触底差距更大、queue 档两侧权重相等（现状回归）。
- **清单外最小改动（W4 记录）：** `tests/vendor_test.rs` sample_pending() 补 `dequeued_at: None`（#8 新字段的强制编译修复）。

#### #8 延迟信号口径分道（TTFT/端到端/排队）+ Peak-EWMA + min+buffer 候选集

- **改了什么：**
  1. **口径分道（纯观测）：** `VendorMetrics` 拆 ewma_ttft_ms / ewma_e2e_ms / ewma_queue_ms 三组独立 EWMA + 各道样本数（ewma_error_rate 不动）；`GlobalMetrics` 增 record_ttft/record_e2e/record_queue（带 now 注入）；`record_request` 不再喂延迟 EWMA（分道本体，单测锁定）。四个喂入点改道：worker 完成分支拆 queue_ms=dequeued_at−enqueued_at + e2e=完成−enqueued_at（成败都记）；两条流式加记 TTFT（口径=send→上游响应头，严格首 token 网关侧拿不到，如实标注近似）；透传非流式记 e2e。`PendingRequest` 增 `dequeued_at`（定义处 `src/vendor/queue.rs`），`VendorQueue::recv` 单点盖戳——结构上保证 recv 返回必有戳且 ≥ enqueued_at。`ewma_latency_ms` 保留为 e2e 别名（语义变化已在字段文档 + config.toml 声明，影响 `/admin/metrics` 外部消费方）。
  2. **Peak-EWMA：** α_dyn = 1−exp(−Δt/τ)，τ=`[routing].ewma_tau_ms` 默认 10000（`config.toml:175`）；GlobalMetrics::set_ewma_tau_ms 于每次落账前从热配置同步。方案微调（正确性必需偏差）：时间锚按道独立 3 字段而非单 last_sample_at——worker 同刻落 queue+e2e 两笔，共享锚会使第二笔 Δt=0→α=0 整笔漏记。
  3. **min+buffer：** `weighted_select_with` 平滑前可选阶段（`router.rs:912-914`，本轮核实）：latency_buffer>0 且有延迟数据候选 ≥2 时取 ewma ≤ 最低×(1+buffer) 集合内均匀随机；buffer=0 完全跳过、rng 零消耗，Po2C/轮盘赌回落与历史逐位一致。
- **文件：** `src/utils/metrics.rs`、`src/vendor/queue.rs`、`src/vendor/manager.rs`、`src/router/router.rs`、`src/config/mod.rs`、`config.toml`（`ewma_tau_ms=10000`、`latency_buffer=0.0`，`config.toml:175/:184`）、`tests/router_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** 分道无开关（/admin/metrics 自动带出三口径）；Peak-EWMA 改变 ewma 数值但默认消费方全关（w_latency=0、latency_buffer=0.0、Po2C 非默认）故默认路由路径逐位不变；w_latency 调非零的顺序依赖写入 config.toml 注释。
- **测试：** metrics 单测 ×5（三道互不混流、60s 间隔新样本权重趋 1 与密集采样平滑的注入 now 精确断言、τ 热同步、record_request 分道锁定、别名逐位一致）；router 单测 ×3（buffer=0.5 候选 [100,120,500]ms 时 500ms 者零入选且集合内两者都出现、buffer=0 纯轮盘赌回归、scored<2 回落）；存量测试迁移 ×2（非删：metrics 延迟 EWMA 测试改 record_e2e 驱动 + 注入 now 反解 α、router Po2C 测试改 record_e2e 喂数——record_request 分道后原喂数路径失效，已注释说明）。
- **未实施项（W4 记录，非独立 recId）：** 「latency_factor 按请求类型取对应口径」需改 compute_weight 签名与调用链，超出本波评审方案文本，未实施、未记 deferred。`config.example.toml` 未动（无测试/代码消费，新字段全 serde 缺省）。GitNexus MCP 工具在本环境不可用（AGENTS.md 引用其规则），改手工 grep 影响面核查（详见 W4 notes）。

### W5 — 软降权、重试守门、判定改进（should ×3 + could ×1）

#### #7 熔断触发式升级为「失败率比例 + 最小样本数」，并补 429 感知

- **改了什么：** (a) 窗口计数层：GlobalMetrics 每 vendor 双 SlidingCounter（total/failures）+ `record_vendor_outcome_at`（`src/utils/metrics.rs:343`）+ `vendor_failure_window`（`metrics.rs:362`，本轮核实）；四处记账点同步双记且失败分支把 RateLimited 一并记 false（worker/`check_stream_status`/透传/两条流式成功点；429 感知独立于 CB 分类器 error_map，只影响失败率窗口不影响 CB 自身）。(b) 软降权：`[resilience] failure_rate_gate` 门控，开启时 calculate_weight 末尾乘 penalty（`src/router/router.rs:1100-1107`，本轮核实）：total≥min_samples(5) 且 fail_pct>threshold(0.5) → ×0.1；三参数入 [resilience] 热读段；路由层 ==Available 过滤未动（软降权只作用于仍 Available 的劣化 vendor）。
- **文件：** `src/utils/metrics.rs`、`src/vendor/manager.rs`、`src/router/router.rs`、`src/config/mod.rs`、`config.toml`（`config.toml:288-290`）。
- **默认状态：** failure_rate_gate 默认 false（关闭时路由权重与现状逐位一致）；失败率窗口计数常开（纯观测，不喂任何决策）；config_toml_example_test 断言默认值。
- **测试（7 项全部在位并点名跑过，W5 记录）：** 窗口 (6,3)、429 入窗而 CB 无感对照、gate=true 6/6 恰 ×0.1、样本 <5 不罚、gate=false 权重不变、config_toml_example 断言。门禁实跑：test 22 组 530 通过。
- **接手实况（W5 记录）：** 接手时发现上一波中断残留已落成 #7 代码面且与方案一致，本次实际改动仅一处：补 `#[allow(clippy::too_many_arguments)]` 于 `perform_http_request`（`manager.rs:489` 附近，`--all-features` 下 cfg(resilience) 门控参致 8 参超 clippy 阈值 7，沿用项目既有先例，零行为变更）。方案要点提示中「唯一 vendor 组 total>=1000 100% 门槛」未实现：评审方案与测试规格均无此项，且软降权是权重乘子非状态位，误杀（剔除）结构上不可能。

#### #9 重试放大守门器（λ_eff/f_c 亚稳态防护）与重试总时间预算

- **改了什么：** (a) 预算：`[retry] retry_budget_secs` 默认 0=不限时；>0 时单 vendor 重试循环（manager 侧）与两个跨 vendor fallback 循环（api 侧）以 tokio::time::Instant 锚点检查，超限返回最后错误（fallback 循环重派前 `budget_exceeded` 判定）；(b) 守门器：GlobalMetrics 增 retried_attempts/failed_requests_before_retry 计数；纯函数 `amplification`（`src/utils/retry.rs:290`）/`critical_failure_fraction`（`retry.rs:302`，本轮核实；λ_eff=λ/(1−r·f)、f_c=(C−λ)/(r·C) 直译）+ 判定纯函数 `amplification_guard`（f>0.8·f_c 收紧、r·f>0.95 发散告警、f_c 无解 fail-open）+ `GlobalMetrics::amplification_view`；消费点在两个 fallback 入口热读评估（`[retry] amplification_gate` 门控），f>0.8·f_c 时该请求 max_total_attempts 临时收紧到 1 + tracing::warn。C=Σ enabled vendor 显式并发上限、全不限时 C=0 不收紧只告警（plan 未定义 None 上限语义，按 fail-open 补全）。
- **文件：** `src/utils/metrics.rs`、`src/utils/retry.rs`、`src/api/mod.rs`、`src/vendor/manager.rs`、`config.toml`（`retry_budget_secs = 0`、`amplification_gate = false`，`config.toml:114-115`，本轮核实）。
- **默认状态：** 双开关默认 0/false——单 vendor 重试与跨 vendor fallback 行为与现状逐位一致（defaults_keep_current_behavior_two_dispatches 结构性回归锁定）；守门器输入计数常开纯观测；`[retry]` 段热读。
- **测试（含新增 8 项，W5 记录）：** guard 纯函数 ×3、amplification_view、计数 ×1、fallback 集成 ×3。TDD 先 Red（guard/view 编译缺失 + gate/budget 行为失败）再 Green。**与 plan 偏差：** fallback 集成测试无法用 `start_paused`——实测 paused clock 与测试接线的 sqlite 连接池冲突（PoolTimedOut），改真实时钟；tokio Instant 随 paused clock 推进的语义由 retry.rs 既有纯函数测试（tokio::time::advance）锁定。门禁实跑：22 组 538 通过。
- **行号注记：** W5 记录引 `r 分母 record_failed_request_before_retry 记在 worker 失败分支（仅转换路径有 per-vendor 重试，与分子同路径口径）`。

#### #19 冷却与候选集的低成本判定改进包

- **改了什么：** 三小项独立落地。(a) 请求级权重：RoutingHints 增 preferred_weight（X-Weight 解析，非数值/非有限/非正一律忽略，读取点 `src/api/routing_hints.rs`）+ `RoutingSection.allow_request_weight` 门控在 Router 侧热读（`router.rs:151`，本轮核实）+ route_weighted/route_with_hints 新变体增权入参（route 原签名不动）+ 四级权重链：请求权重 > model.weight(>0) > 容量自动权重（weight<=0 时 window limit 归一，缺省 1.0 使该分支默认不触发）> 均匀 1.0。(b) fail-open：`fail_open_when_all_cooling` 门控（`router.rs:456/:591`），开启且全候选被 CB/Throttled 剔除时回落忽略状态的候选集加权选择（注释如实注明：无主动健康检查，覆盖的是 CB Open 超时未过窗口/限流窗口未重置，仅建议有兜底容量的部署开启）。(c) Retry-After 提示：excluded 升级为 `Vec<(String, ExclusionReason)>`（RateLimited 或 get_status==Throttled → Throttled），两处耗尽分支统一收敛 `finalize_all_throttled`（`src/api/mod.rs:260`，本轮核实）：hint 开且全 Throttled → 取各 vendor 窗口重置剩余 min 抛新变体 `AllVendorsThrottled`（`src/error.rs` IntoResponse 429 + Retry-After 头）；否则原错误冒泡。
- **文件：** `src/router/router.rs`、`src/api/mod.rs`、`src/api/openai.rs`、`src/api/anthropic.rs`、`src/api/routing_hints.rs`、`src/vendor/manager.rs`、`src/vendor/resilience/error_map.rs`、`src/error.rs`、`src/config/mod.rs`、`config.toml`（`config.toml:190/:198/:204` 三开关 false，本轮核实）。
- **默认状态：** 三开关均默认 false——X-Weight 头被忽略、全剔除保持 Validation 错误、耗尽原错误冒泡（503 语义不变），行为与现状逐位一致；三参数入 [routing] 热读段。
- **测试（含新增 12 项，W5 记录）：** X-Weight 解析、error 429+Retry-After 头、router 门控/覆盖数值 1.65/容量归一/均匀兜底/fail-open 双向、api 收尾纯判定 min(30,60)=30 与两耗尽分支 E2E 429+Retry-After 头、混合原因保持 503、默认关回归。门禁实跑：22 组 550 通过。
- **与 plan 的偏差（W5 记录，以源码为准）：** ① WindowInfo 无 reset_secs 字段——get_window_info 把 limiteron window_snapshot 的 reset_secs 折算成 window_end，故剩余秒从 window_end 与当前差向上取整派生；② E2E 全 Throttled 场景用 Token max_tokens=0 配置注入 + direct 形式初始决策驱动循环——首路由 ==Available 过滤使循环不可达的入口边界即 plan 列为后续条目的场景，文档/测试注释如实注明；③「原错误冒泡」实现为最后一次失败的错误；④ calculate_weight 移除被 effective weight 取代的 model 参数（编译器驱动）；⑤ (c) 覆盖面限于 execute_request_with_fallback（透传/流式 dispatch 变体 excluded 未升级）。

#### #10 会话亲和从硬 return 软化为评分项 + 键租户隔离 + SetNX 首绑定收敛

- **改了什么：** TDD 按 plan 顺序三步。(2) 键隔离（Red 先行）：`AffinityTable::key` 增 tenant 维度（Some(hash) → `affinity:{api_key_hash}:{session_key}`，None/空 = legacy 键），bind/lookup/unbind/bind_if_absent 全部增 tenant 参数，API 层 bind/clear helper 从已验证 key 经 cache_api_key_hash 派生传入，路由侧 route_weighted/route_with_hints 增 tenant 入参并按 `affinity_tenant_scoped` 门控（关闭一律 legacy 键）。(1) 软化：`affinity_mode`（`router.rs:483/:615`，本轮核实）——hard（默认）命中即返回现状不变；soft 走 `apply_affinity_boost`（`router.rs:1404`，pub 纯函数）权重 ×(1+affinity_boost_factor) 后与其余候选竞争，factor<=0 无操作。(3) 首绑定收敛：`bind_if_absent`（get+set 模拟 SetNX，命中已有绑定时照常续期 TTL）用于流式「派发成功即回写」四个首绑定点（tenant_scoped 开启时），「失败迁移绑定」四个点改 rebind_session_affinity 无条件 set，clear 22 处同步 tenant。
- **文件：** `src/router/router.rs`、`src/router/affinity.rs`、`src/router/mod.rs`、`src/api/mod.rs`、`src/api/openai.rs`、`src/api/anthropic.rs`、`src/config/mod.rs`、`config.toml`（`affinity_mode = "hard"` `config.toml:210`、`affinity_tenant_scoped = false` `:220`）。
- **默认状态：** affinity_mode 默认 "hard"、affinity_tenant_scoped 默认 false（boost_factor 默认 1.0 仅 soft 模式生效）——关闭时命中即返回、legacy 键、无条件 set，行为与现状逐位一致（session_affinity 既有 5 测试全绿 + unknown_mode 按 hard 回滚 + tenant 参数被忽略回归锁定）；三参数入 [routing] 热读段。
- **测试（含新增 7 项，W5 记录）：** 租户隔离 Red→Green（tenant-A 绑定后同 session 的 tenant-B lookup 必须 None）、bind_if_absent 收敛、soft boost 固定种子（StdRng 42 + weighted_select_with）/boost=0 回归/soft E2E（factor=10000 使其余候选胜出率 <0.01%）/未知模式回滚/租户隔离 E2E。门禁实跑：22 组 557 通过。
- **与 plan 的偏差（W5 记录）：** ① boost 默认值 plan 写 1.0、要点提示写 0——取 plan 的 1.0，行为保持由 affinity_mode 默认 "hard" 保证；② router 侧 lookup 也需 tenant（scoped 键下 bind/lookup 必须同键，否则亲和整体失效）——plan 只提 bind/lookup/unbind 增参，路由入参为必要延伸。

### W6 — 打分项与准入、窗口、缓存、票据（should ×4 + could ×3）

> 本波五条算法项（#14/#15/#17 及 #7/#10 前波乘子）同插入点 calculate_weight 权重链末端/weighted_select 分支，串行叠加且每步带关闭态等价断言；#16 以 #3 的 InboundLimitState 为基线；#11/#18 共享 rate_limiter.rs；#12 依赖 #8 的 ewma_e2e_ms。

#### #14 SLA/满意度虚拟队列打分项（SLARouter 零模型版）

- **改了什么：** TDD（先写纯函数桩与 11 测试，Red 实跑 4 单元 + 5 集成失败，再转 Green）。ŝ_m = 1−EWMA 错误率（复用 `GlobalMetrics::vendor_ewma`，无 e2e 观测时 ŝ=1 冷启动乐观）；Ĉ_m 查热配置 [pricing] 的 (input+output)/2 每 1k 单价；缺价候选记候选集已知单价中位数（`sla_median_cost`，`src/router/router.rs:1479`，本轮核实），候选集全无定价时成本项整体置 0 退化为纯 Q 项并 warn 一次；惩罚因子 1/(1+V·Ĉ+max(0,Q·(α−ŝ))) 乘在权重链末端（calculate_weight 单 vendor 视野拿不到候选集中位数与租户 Q，按权重链末端落位，与失败率软降权/亲和 boost 独立乘法）；Router 增 `sla_queues: Arc<RwLock<HashMap<String,f64>>>`，选择完成后在 weighted_select 做对偶更新 Q←max(0,Q+α−ŝ_selected)。
- **文件：** `src/router/router.rs`、`src/router/mod.rs`、`src/config/mod.rs`、`config.toml`（`sla_v = 0.0` `config.toml:235`、`sla_alpha = 0.95` `:239`，本轮核实）、`tests/router_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** sla_v 默认 0.0 = 完全关闭：惩罚不参与权重、Q 不读不写、路由行为与现状逐位一致（sla_off_keeps_weights_bit_identical_and_queue_untouched 断言逐位相等 + Q 表恒空；config_toml_example_test 断言默认值且示例键显式存在）。sla_alpha 默认 0.95（非有限/越界钳 [0,1]，NaN 按 0）；负/非有限 sla_v 一律视为关闭；sla_v>0 才启用惩罚与 Q 更新，Q 重启归零。V/α 进 [routing] 热更段（热更立即生效有专项测试）。
- **测试：** 11 项（单元 4：Q 更新钳位/惩罚单调性/缺价方向性/关闭逐位等价 + 集成 7，含 sla_queue_is_scoped_per_tenant 锁定租户独立累积）。
- **与源码现状的偏差（W6 记录）：** ① plan 引 metrics.rs:164-166 实为 vendor_windows 注释，ŝ 实际复用 vendor_ewma，`src/utils/metrics.rs` 零改动；② route_inner 的 affinity_tenant_scoped 门控（#10）会在入口把 tenant 吞为 None 导致 Q 恒落 global 桶——SLA 链路改传原始租户（新增 sla_tenant 参数穿 try_abstract_route/try_global_route），亲和门控原位未动；③ direct/亲和 hard 等单候选「命中即返回」路径不经 weighted_select，无选择即无对偶更新（已注释说明）。gitnexus 影响分析工具不可用，以 grep 手工核对全部调用点代替。

#### #15 LinUCB 打分项：人工特征起步，闭式更新无新依赖

- **改了什么：** TDD（先写 `src/router/bandit.rs` 桩与 8 单元测试，Red 实跑 5 失败，实现后全绿；Router 集成 6 测试一次全绿）。`BanditState`（`src/router/bandit.rs:228`，本轮核实）臂表键 (abstract_model, vendor_id)，臂 Arc<Mutex<Arm>> 惰性创建；A₀=max(α²,1)·I（α≤0/非有限取 1 保正定可逆）、b₀=0；`ucb`（`bandit.rs:271`）=⟨x,A⁻¹b⟩+α√(xᵀA⁻¹x)，手写部分主元高斯-约当求逆（d=5，无新依赖），奇异/非有限退化对角近似并 warn 一次；`update`（`bandit.rs:296`）只动被选臂 A+=xxᵀ、b+=r·x（r=成功1/失败0），脏样本丢弃；平局 |Δ|≤1e-9 由注入 rng 决断。特征 = `linucb_features`（`bandit.rs:75/:92`）5 维 [1, ln(1+消息数), ln(1+body字符数), 含tools, 流式]。选择分叉在 try_abstract_route/try_global_route 的普通路径（`linucb_route`，`src/router/router.rs:721`，本轮核实；热读 selection+ucb_alpha）：亲和 hard 命中仍优先、soft/fail-open 路径保持 weighted；global 路径臂键 "__global__"。奖励回传收敛在两 handler 的落库函数（response:None=失败，与落库记账同点），经 ApiState::record_linucb_reward_from_body（接线+selection 双重 gate，默认档不计算特征、无状态更新）。接线：Router.with_bandit（affinity 同款 builder）、`src/main.rs:130`（本轮核实）构造 BanditState 并注入 Router+ApiState。
- **文件：** `src/router/bandit.rs`（新）、`src/router/mod.rs`、`src/router/router.rs`、`src/api/state.rs`、`src/api/openai.rs`、`src/api/anthropic.rs`、`src/main.rs`、`src/config/mod.rs`、`config.toml`（`selection = "weighted"` `config.toml:154`、`ucb_alpha = 1.0` `:161`，本轮核实）及 6 个测试/构造点适配文件。
- **默认状态：** selection 默认 "weighted"：LinUCB 档不被咨询、特征不计算、BanditState 不读不写（测试侧 Arc 断言 arm_count 不增长、200 次路由两 vendor 均出现），行为与现状逐位一致；config_toml_example_test 断言 selection=weighted、ucb_alpha=1.0 且示例键显式存在。selection="linucb" 时：未接线 BanditState 或特征缺失均回退 weighted（安全语义）；ucb_alpha 热更只影响探索半径，臂 A/b 重启归零且特征集变更不可迁移。
- **与 plan 的偏差（W6 记录）：** ① LinUCB 分支未放 weighted_select_with 而在 try_abstract_route/try_global_route——臂键需要 abstract_model 且不应破坏 weighted_select_with 的 pub 确定性测试入口，语义等价；② ucb_alpha 默认 1.0 而非要点提示的 0——以已评审 plan JSON 为准（关闭由 selection 档位承担，0/负值语义=纯贪心而非关闭）；③ plan files 未列 openai.rs/anthropic.rs 与 4 个测试文件，但 approach 明确要求 handler 回传且 ApiState 加字段必须适配全部 8 处构造点；④ 预算感知变体（UCB/max(ĉ−β,ε)）按 plan 列为后续未实现。

#### #16 拥塞感知优先级准入

- **改了什么：** TDD（先写 `src/middleware/admission.rs` 桩与 12 测试，Red 实跑 10 失败，实现后单元全绿；全链路集成 15/15）。(a) 身份：AuthSection 增可选 `[[auth.keys]] { hash, tier }`（AuthTier 严格 serde 枚举 dedicated/guaranteed/elastic/spot 权重 1000/100/1/0.1，未知值使加载失败——配置错误早暴露）；api_keys 兼容不动（缺省档 elastic，#[default] 派生）；`tier_for_key`（`src/middleware/auth.rs:111`，本轮核实；validate_key bool 签名不动，复用同一 sha256+constant_time_eq 摘要比较）。(b) 优先级：`admission_priority`（`src/middleware/admission.rs:73`）= w_tier·(1+α_slo·ℓ*)⁻¹·(1+α_burst·b)⁻¹·(1+α_debt·d)⁻¹（α 2.0/1.0/4.0 进 [admission] 热更段；ℓ* 无数据源恒 0；burst=到达速率 req/s EWMA、debt=被拒频度 EWMA，AtomicU64 ×2¹⁶ 定点，per-key 表带 1024 条硬上限+溢出共享桶防伪造 key 内存放大）。(c) 挂载：核实 #3 已交付 InboundLimitState（`router_builder.rs:32`），扩展 admission: Arc<AdmissionState>（`admission.rs:113`，常驻构造保证 enabled 热开立即生效）；准入分支在 rate_limit_middleware 健康白名单与限流判定之后、handler 之前；拥塞信号 = `fleet_concurrency()`（`manager.rs:994`，Σ in_flight/Σ max_concurrency，复用 #6）≥ `congestion_threshold`（默认 0.9）；比较语义 = 新请求 priority < 在途最小档 tier 权重 → 429+Retry-After(1s)，同档/高档放行、在途计数空时保守放行；per-tier AtomicU64 计数 guard（Drop 含 panic 展开释放，专项测试）；健康检查永不 429（沿用白名单）。
- **文件：** `src/middleware/admission.rs`（新）、`src/middleware/mod.rs`、`src/middleware/auth.rs`、`src/config/mod.rs`（`AdmissionSection` `config/mod.rs:913`，本轮核实）、`src/api/router_builder.rs`、`src/vendor/rate_limiter.rs`、`src/vendor/manager.rs`、`config.toml`（`[admission]` `config.toml:307-310`，enabled=false）及大量构造点适配文件（Config/AuthSection 加字段与 fleet 信号的必然适配）。
- **默认状态：** `[admission].enabled` 默认 false = 完全关闭：准入判定旁路、EWMA 不记账、per-tier 在途计数不参与任何决策，行为与现状逐位一致（config_toml_example_test 断言 enabled=false + 阈值 0.9/系数 2.0/1.0/4.0 + 示例键显式存在，且断言示例 [auth].keys 缺省为空）；auth.keys 缺省 → 全部 elastic（tier_for_key 未命中恒 elastic）。开启后全部参数热更。
- **测试（含 15 项集成）：** 纯函数四档数值断言、burst EWMA 独占抑制；全链路 shed 构造：vendor 指向不响应的本地 TCP 监听 + timeout 1.5s，两个挂住的 dedicated 请求把并发推到 2/2 → spot 429+Retry-After、dedicated 放行；enabled=false → 同状态两档都放行（默认回归）；健康检查永不 429（断言用 !=429——/api/v1/health 实际路由由 sdforge 在 main 挂载，测试 app 无该路由，白名单放行后 404，注释说明）；guard Drop 语义（panic 路径计数不泄漏）。
- **自洽定义与偏差（W6 记录）：** ① burst/debt 的样本定义 plan 未锁定，采用网关可达信号的自洽近似（到达速率/被拒频度，EWMA 系数 0.5/0.25 固定常量），报告已限定「粗分类 shed 顺序」定位；② Retry-After 固定 1s（plan 未指定秒数，未加配置面）；③ guard 生命周期为中间件作用域、流式 body 传输期不在途计数、容量只计已初始化实例——两处网关侧近似已注释。

#### #17 排队论延迟前瞻估计（LPS 平均情形模型）

- **改了什么：** TDD（lps_wait 纯函数桩+4 单测先 Red 实跑 3 失败 → Green；metrics 窗口访问器单测 + router 集成 6 测全绿）。(a) 输入可得化：核实 SlidingCounter::window_total 实际已是 pub(crate)（plan 标「私有」未单独复核，以源码为准无需改），仅新增 `GlobalMetrics::vendor_window_total`（`src/utils/metrics.rs:569`，本轮核实）；μ=1000/ewma_e2e_ms（无 e2e 数据 → Ŵ=0 中性不惩罚冷启动；口径含排队会高估 Ŵ——注释声明粗分类定位）；k=max_concurrent_requests 缺省 100。(b) 纯函数 `lps_wait`（`src/router/router.rs:1452`，本轮核实）：(α/μ)^k/(μ−α)，ρ≥1/μ 非法→CAP(1e6)、α≤0→0、k<1 鉗 1；d=1 退化 M/M/1 数值对照精确。(c) 接入（按评审 issue#17 采纳的有意偏差）：乘法 1/(1+δ·Ŵ/Ŵ_ref) 而非报告线性 −δ·Ŵ——线性负项会触发 compute_weight 非正归零保护造成权重归零被剔除的语义污染（注释+文档双注明）；Ŵ_ref=当次候选集内 Ŵ 中位数（全候选发散时中位数=CAP→因子均匀不扭曲选择；全候选无数据→因子恒 1）；乘点在权重链最末端（#14 SLA 之后，独立乘法），lps_delta=0 零开销旁路（`router.rs:1183-1194`，本轮核实）。硬约束版：`lps_deadline_gate`（`router.rs:1217-1231`）+ x-deadline-ms 头（两 handler 读取，非法值忽略）→ 候选过滤先按 Ŵ·1000≤deadline 筛可行集，空集时改选 Ŵ 最小者（JITServe 软信号语义）；deadline 非法（≤0/非有限）视为不设限而非全超时；亲和 hard/soft 优先于筛选；global 路径对称实现（plan 字面只提 abstract，gate 默认关不影响 defaultOff）。
- **文件：** `src/utils/metrics.rs`、`src/router/router.rs`、`src/config/mod.rs`、`src/api/openai.rs`、`src/api/anthropic.rs`、`config.toml`（`lps_delta = 0.0` `config.toml:250`、`lps_deadline_gate = false` `:256`、`default_deadline_ms = 10000` `:260`，本轮核实）、`tests/router_test.rs`、`tests/config_toml_example_test.rs`。
- **默认状态：** lps_delta 默认 0.0、lps_deadline_gate 默认 false：Ŵ 不参与权重、deadline 头不被消费（handler 只读头不解析成本），行为与现状逐位一致——δ=0 逐位基线测试 + config_toml_example_test 断言三键默认值且示例键显式存在锁定。开启后三参数均进 [routing] 热更段（δ 热更立即生效有专项测试）。
- **plan files 偏差：** 未列 openai.rs/anthropic.rs——硬约束「接受 x-deadline-ms 头」必然动 handler，以 approach 为准。

#### #11 出站 Token/Interval 窗口滑动窗口化

- **改了什么：** TDD（先写 MockClock::with_instant（固定 UNIX 锚，远离分片判定的 0 饱和边界）驱动的 4 个滑窗测试，Red 实跑确认核心断言失败——sliding=true 当时仍走 fixed → 翻越点后双花放行；实现后 4/4 Green、vendor_test 全绿含既有 fixed 回归）。`WindowInner` 枚举 `{ Fixed(Arc<FixedWindowLimiter>), Sliding(Arc<ShardedSlidingWindowLimiter>) }`（`src/vendor/rate_limiter.rs:140`，本轮核实；枚举分派而非 dyn Limiter 避免对象安全不确定性）；`RateLimitConfig::Token/Interval` 各增 `#[serde(default)] sliding: bool`（缺省 false=Fixed 现状）；with_clock 按 sliding 经 build_window 选实现（limiteron `ShardedSlidingWindowLimiter::with_clock` 时钟注入、`allow(cost)` token 计量消费、peek 同构 RateLimitSnapshot 均已核实）；check/consume_tokens/window_snapshot 三处 match 分派，接口签名不变；明确不用 SlidingWindowLimiter（deprecated + 硬编码 Instant::now 破坏时钟注入）。
- **文件：** `src/vendor/rate_limiter.rs`、`src/config/mod.rs`、`config.toml`、`tests/vendor_test.rs`、`tests/config_toml_example_test.rs` 及 12 个构造点适配文件（RateLimitConfig 加字段致 47 处构造/模式点补默认值）。
- **默认状态：** sliding 默认 false → inner 仍是 FixedWindowLimiter，行为与现状逐位一致（既有 Token/Interval 窗口测试全绿回归 + fixed 翻越点双花语义对照测试锁定）；滑动窗口为显式 opt-in（sliding = true）。config_toml_example_test 断言随仓 vendor 配置 Token.sliding=false（解析级断言，覆盖 serde default 静默缺席场景）且 Interval 若出现同样锁定 false。sliding 消费面 restart-required（vendor 实例构造期选定，与既有 rate_limit 语义一致）。
- **测试要点：** 翻越点两侧各消费 60%（t=T0+3300s 消 6 → 跨 fixed 翻越点 T0+3600）→ sliding 拒第二笔且剩 4 可用（Cloudflare 12:09/12:11 例等比构造），fixed 对照锁定现状双花语义；快照形状测试如实锁定两实现差异点——sharded 空窗 reset_secs=0、有消耗后以窗口全长为近似上界（window_factor 仅 is_ending_soon(<300s) 惩罚，注释注明影响可忽略）。
- **plan files 偏差：** 未列构造点适配文件；`src/vendor/mod.rs` 最终未动（ConcurrencyPermit 此前已导出）。

#### #12 精确响应缓存 TTL 负载联动 + 按类别差异化

- **改了什么：** 依赖注入补齐（评审 issue#6 采纳）：ResponseCache 增 `metrics: Arc<GlobalMetrics>` 字段（L_p=vendor_ewma 的 ewma_e2e_ms、Q=traffic_shares 份额，均从 GlobalMetrics 取、不引入 VendorManager），构造签名三处适配（`src/main.rs:110` 生产接线本轮核实；response_cache.rs 内两处 #[cfg(test)] 补 GlobalMetrics::new()）；`ttl(vendor_id, model_id)` 三段式（`src/api/response_cache.rs:188-192`，本轮核实）：base=ttl_secs×类别乘数（[[cache.categories]] 按 model_id 前缀首条命中，无命中 1.0，负/非有限防御为 1.0）→ 负载联动（ttl_load_linked 门控）λ=min(1,(L_p/L_target)·w_L+(Q/Q_target)·w_Q) clamp[0,1] → 实际=base×(1+λ(β_max−1))，β_max<1 鉗 1（只放大不缩短）。口径降级如实声明（ttl 注释 + config.toml + 配置字段 doc 三处）：论文 L_p 为 P95 分位，本实现用 ewma_e2e_ms 均值替代——高估方向比低估安全。
- **文件：** `src/api/response_cache.rs`、`src/main.rs`、`src/config/mod.rs`、`config.toml`（`[cache]` 段 `config.toml:330` 起：`ttl_load_linked = false` `:335`、w_l/w_q=0、beta_max=1.5、l_target=1000、q_target=0.5、categories 表缺省空，全部热读，本轮核实）、`tests/config_toml_example_test.rs`、`tests/api_test.rs`（CacheSection 字面构造点 3 处补 ..Default::default()，加字段必然适配）。
- **默认状态：** ttl_load_linked 默认 false（纯静态 TTL）、w_l/w_q 默认 0、categories 表缺省空（乘数全 1.0）→ ttl() 返回值与现状逐位相等（开关等价测试断言恰等于配置值 300s）。类别乘数表独立于联动开关生效（配了即乘）；冷 vendor（无观测）λ=0 不惩罚冷启动。语义缓存维持暂缓不动（papers-caching 结论：无日志数据不得开启——本条不改变该决定）。
- **测试（9/9 全绿含 5 存量）：** 开关等价（ttl_load_linked=false → ttl 恒等于配置 300s）、λ 鉗位（灌 vendor ewma 5000ms、w_l=1、l_target=1000 → λ=1 → TTL=450s 精确）、类别乘数命中（gpt-4 前缀 ×2 → 600s）/缺省（300s）、冷 vendor λ=0（TTL=base）。

#### #18 agentic 负载的 token 加权并发票据

- **改了什么：** (a) 估算双形态：`TicketPolicy{min,max,default}`（`src/vendor/manager.rs:84`）+ `estimate_cost_unified`（`manager.rs:95`）/`estimate_cost_json`（`manager.rs:109`，本轮核实）：cost = clamp(messages 序列化字节/4 + max_tokens, min, max)，max_tokens 缺省用 default=4096 兜底；估算口径声明（序列化字节/4 对中文/多模态偏差约 1.5-3×，建议保守调低 token_ticket_max）写入函数 doc。(b) 票据：`ConcurrencyLimiter::acquire_weighted(cost)`（`src/vendor/rate_limiter.rs:112`，本轮核实）——`acquire_many_owned`（OwnedSemaphorePermit 与现有 acquire 同源；cost 鉗 [1, 容量]，超容量按容量截断防 acquire_many 永久等待死锁）。(c) 门控：ConcurrencyConfig 增 token_weighted=false / token_ticket_min=1 / token_ticket_max=16384 / token_ticket_default=4096（全部 serde default，vendor 实例构造期消费 restart-required，与 max_concurrent_requests 同路径）；VendorInstance 增 ticket: Option<TicketPolicy>，四处 acquire 接线（本轮核实现行行号 `manager.rs:410/:1251/:1564/:1669`；W6 记录中的 :357/:1191/:1497/:1595 为合入时点行号）：worker 循环与转换流（UnifiedRequest）走 unified 版、透传非流式与透传流式（Value）走 json 版，None=未开启单票据现状；#6 的 in_flight 自动变 token 加权口径（plan 预期）。
- **文件：** `src/vendor/rate_limiter.rs`、`src/vendor/manager.rs`、`src/config/mod.rs`、`config.toml`、`tests/vendor_test.rs`、`tests/config_toml_example_test.rs` 及 12 个构造点适配文件。
- **默认状态：** token_weighted 默认 false：四处 acquire 仍为单票据、估算函数不被调用，行为与现状逐位一致（unweighted 对照测试锁定同 body in_flight==1；既有并发/窗口测试全绿回归）；config_toml_example_test 断言随仓配置默认值且示例注释显式存在。
- **测试（5/5 全绿）：** 估算 clamp 三边界（缺 max_tokens→default、超长→max、零成本→min）、两形态同请求量级一致（ratio<2）、acquire_many 等待语义（容量 100 两张 cost=50 占满→第三张 60 等待、释放后获得且 in_flight==cost）、全链路挤压（挂起 listener + 32k 长会话→fleet_concurrency==(100,100) 独占、短请求 300ms 内 pending 被挤压）、unweighted 对照。vendor_test 实跑 79/79。
- **TDD 顺序偏差（如实声明）：** 本条为纯新增能力（无存量行为可断言失败），实现先行、weighted=false 的现状行为由 78 条既有测试回归锁定，weighted=true 行为测试与实现同期完成。其余偏差：① 估算签名用 TicketPolicy 结构体三元（default 兜底值需第三参数）；② acquire_many_owned 对 plan 的 acquire_many 语义修正（tokio 借用/Owned 两形态，项目 ConcurrencyPermit 要求 Owned）。

---

## 3. 缺陷修复专节（P0 七条前后对比）

### 3.1 #1 熔断器慢调用阈值误触发

| | 内容 |
|---|---|
| **修复前行为** | 生产 CB 使用 limiteron 默认 500ms duration_threshold / 0.5 rate_threshold（limiteron constants.rs:106/111，材料核实）。LLM 正常延迟动辄数秒，任意一次 >500ms 的**成功**调用即被计为慢调用；因 limiteron `check_slow_call_rate` 为逐调用判定、无最小样本数，首调用即慢时 rate=1/1=1.0 ≥ 0.5 → **单次 800ms 成功调用即熔断 Open**（Red 实测，比方案预期「3 次慢触发」更严重）。 |
| **修复后行为** | 阈值抬至 60s/1.0（`src/vendor/manager.rs:154`）：混合快慢流量下慢调用路径失活，熔断由真实失败驱动（failure_threshold=5 路径）；3×800ms 真睡眠成功调用保持 Closed。唯一残余触发场景「窗口内 100% 调用皆慢且全成功」与失败路径条件在 #4 默认超时下重合，行为一致（doc 注释声明）。 |
| **判定依据** | ① limiteron types.rs slow-call 双阈值为 ≥ 比较（材料核实，types.rs:580/:603）；② 生产构造证明：`manager.rs:2066` 单测（经生产同款 helper 建 CB，3×800ms 后 get_state()==Closed）；参数断言 `manager.rs:2090`；③ limiteron 语义隔离证明：`tests/circuit_breaker_test.rs:126` 自建 CB（100ms/1.0），1 快+2 慢 → 全程 rate<1.0 → Closed（旧默认 0.5 下 2 慢触发会 Open）。证明力边界如实声明：集成测试证明参数语义，单测证明生产接线，两者合一才完整。 |

### 3.2 #5 重试延迟同步惊群

| | 内容 |
|---|---|
| **修复前行为** | Retry-After 分支延迟恒等于上游指示值（`src/utils/retry.rs` 旧 :231-233）、5xx 固定间隔分支恒 1s（旧 :242）——Red 实测 delay==7s/1s。同一 Retry-After 下并发请求/多实例在同一时刻唤醒重试，形成同步惊群。 |
| **修复后行为** | 两分支全范围抖动：Retry-After ∈ [ra, 2×ra)、5xx ∈ [1s, 2s)（`spread_delay`，`src/utils/retry.rs:270-282`，gen_range 半开上界）；同上游指示下并发唤醒从同刻变为 U(0,base) 展宽。429 无头指数分支零改动（仍走 limiteron delay_for_attempt 的 [retry].jitter=0.2）。回滚阀：`spread_jitter = 0` 恢复旧行为（`config.toml:112` 注释写明回滚路径）。 |
| **判定依据** | `tests/retry_test.rs:344`（Retry-After 展宽）、`:370`（5xx 展宽 ∈ (1s,2s]）、`:394`（spread_jitter=0 时固定种子下恰等于 ra/1s——回滚语义显式锁定）；固定种子精确值 StdRng(42)→1.526557409s 经实跑固化（非占位）；存量两测试经 deterministic_policy 的 spread_jitter=0.0 保留旧语义断言。 |

### 3.3 #13 TimeWindow 跨午夜时段门控失效

| | 内容 |
|---|---|
| **修复前行为** | `check()` 与 `get_window_info` 的窗口判定（`now >= start && now < end` 及其取反）不感知跨午夜——22:00–06:00 配置在 02:00/23:00 等时段恒判「窗口外」，整段失效：check() 拒绝放行、window_factor 恒按窗口外惩罚路由权重。跨午夜 case 恒 false 可由代码直读证明；Red 为 E0432（修复函数不存在）。 |
| **修复后行为** | 纯函数 `time_window_active`（`src/vendor/rate_limiter.rs:30`，本轮核实）：同日 `start <= now < end`；跨午夜 `now >= start || now < end`。两处调用点统一委托（DRY），跨午夜配置正常生效。进阶项未做（注释注明）：跨午夜窗口内 window_end 精确次日时刻（当前退化为 Instant::now()，window_factor 保守减半但不整段排除）；per-window 时区字段。 |
| **判定依据** | `tests/vendor_test.rs:1729`（本轮核实）：start=22:00/end=06:00 下 now=02:00 true、now=23:00 true、now=12:00 false；同日边界 now==start true、now==end false（左闭右开）；`get_window_info` 窗口外 window_end:None 回归（时钟无关构造：end=now−1min/start=now−2min，同日与跨午夜两种时钟状态下均稳定窗口外，既有语义不回退）。 |

### 3.4 #4 出站 HTTP 零超时/超时含排队/流式无超时

| | 内容 |
|---|---|
| **修复前行为** | ① `reqwest::Client::new()` 无连接超时（W2 记录：现源码 :177）——连接挂起可无限等待；② 非流式仅外层 60s 硬编码预算且**含本地排队**（W2 记录：实际 :815）——排队久时执行段被挤占，per-attempt 无上限；③ 两条流式路径 send 与 chunk 读取均无任何超时——上游无响应或流中断时请求永久挂起、permit 不释放。 |
| **修复后行为** | 三层模型：connect 10s（`manager.rs:224/:239`）；非流式 per-attempt 60s（`timeout_ms`，从 send 起算，重试各自独立计时，`apply_attempt_timeout` `manager.rs:671`）；流式 TTFT 60s（响应头到达前超时→Err，流对象不产生）+ chunk 间 idle 30s（超时产出 Err(Timeout) 终止流，permit 随 unfold 终止释放）；外层总预算 = 排队 60s + timeout_ms（`manager.rs:1065-1076`），排队与执行预算解耦。逃生阀：三参数任一置 0 = 对应层不限时。 |
| **判定依据** | `tests/vendor_test.rs`：nonstream_timeout_returns_timeout_error（mockito 延迟桩）、timeout_zero_disables_per_attempt_budget（桩 sleep 超过默认值仍成功——锁定 0=禁用约定）、stream_idle_timeout_terminates_mid_stream、stream_ttft_timeout_fails_before_headers（手写 tokio TcpListener 桩 accept 后延迟 1.2s 才写响应头——mockito 1.x 无头前延迟原语，评审 issue#12 采纳）；config_toml_example_test 断言三默认值。行为面变化如实声明：原本可无限等待的请求现在会超时失败，长思考型模型需调大 timeout_ms/stream_ttft_ms 或以 0 关闭。 |

### 3.5 #3 入站 [rate_limit] 段被整段丢弃

| | 内容 |
|---|---|
| **修复前行为** | config.toml `[rate_limit]` 段（旧扁平字段）被 serde 整段丢弃（Config 无对应字段），运行中的 Governor 一直是 `Governor::new()` 的**库内置默认规则**——运维写进配置的任何限流策略从未生效。旧字段从未被消费，无既有行为可保。 |
| **修复后行为** | `RateLimitSection`（`src/config/mod.rs:982`）落地：rules 为空 → `Governor::new()` 默认兜底 + 无端点表（逐位现状）；非空时 path=None 全局规则注入 Governor（Matcher::User + SlidingWindow + Reject）、path=Some 规则走中间件端点表（每 rule×tenant 一只 limiter 惰性创建），超限 429 + Retry-After；租户摘要经 cache_api_key_hash（无 key 归 anonymous 单桶）；健康检查白名单放行。中间件 State 改 `InboundLimitState`（`router_builder.rs:32`）为 #16 建立挂载基线。 |
| **判定依据** | Red：新 rate_limit_test.rs 引用不存在的 RateLimitSection/Config.rate_limit，cargo check 报 E0432/E0609/E0560。Green：`tests/rate_limit_test.rs` 实跑 7 通过（同租户第 3 请求 429+Retry-After、不同 Bearer key 不受影响、path 前缀只约束匹配端点、健康检查打满仍放行、无 Authorization 全归 anonymous 单桶、path=None 经 Governor 生效、空 rules 冒烟）；config_toml_example_test 断言示例 rules 为空（结构性锁定）。语义边界如实声明：path=None 全局规则的 per_tenant 在 Governor 层不生效（limiteron 决策链为规则级单例桶），需租户隔离配额须配 path 前缀。 |

### 3.6 #2 韧性机制只覆盖 1/4 执行路径

| | 内容 |
|---|---|
| **修复前行为** | 熔断/重试/换路仅覆盖转换非流式路径；透传非流式、透传流式、转换流式三条路径裸奔——透传请求无 CB 保护、无 fallback（openai.rs:192-193「透传无 fallback」注释），流式上游不可用时直接报错不换路。 |
| **修复后行为** | `[resilience] cover_all_paths` 开启时 4/4 覆盖：一期透传非流式获得 per-vendor CB 包裹 + 跨 vendor 换路（same_protocol_only 过滤防跨协议误派）；二期三条流式路径获得「首字节前换路」（Err 均在响应头前，重派排除已失败 vendor；Ok 即交出 body，流中错误仍以 SSE error 事件终止不再换路）；CB 时长只计到响应头，与首字节边界一致。actual_vendor/亲和绑定/缓存键以最终 decision 记账。关闭时三条裸路径与开关前逐位一致。 |
| **判定依据** | `tests/fallback_test.rs:346` cover_all_paths 模块 7 用例：换路成功且 actual_vendor 记 B、流式 503 重派拿到 SSE、流中 error 不重派（A expect(1) 精确）、CB Open 后零新增触达 + B 接管、direct 路由绕过路由层预过滤时派发层 CB 拒绝且零 HTTP、开关=false 时健康 B expect(0)（防门控失效）；config_test.rs 热更写回不抹段。风险如实声明：本条为 19 条中行为面风险最高——返回类型异构需平行实现；CB 包裹后 Open 状态会拒绝透传流（开启时），运维文档须写明开关影响面。 |

### 3.7 #6 负载信号只有队列深度

| | 内容 |
|---|---|
| **修复前行为** | 路由 3D 权重的负载维只有本地排队深度（get_queue_length）。流式请求的 permit 随流存活但不在队列里——长流占用上游真实并发时，队列深度看不到，路由继续向已饱和 vendor 派发。 |
| **修复后行为** | `[routing].load_signal` 三档：queue（默认，现状）、in_flight（`rate_limiter.rs:85` = max − available_permits）、in_flight_relative（`router.rs:1393` 相对化）。流式 permit 随流存活故 in_flight 对长流真实生效——恰是修复点。排队深度不叠加为次级加权项（负载维输入是单选，叠加会双计同一请求且系数不可分——已写入字段文档与 config.toml）。 |
| **判定依据** | rate_limiter 单测（acquire n→in_flight==n、Drop 归零、max 字段断言）；router 纯函数 8 组边界；mockito 集成：max_concurrent_requests=2 两条慢 chunked 响应占满 permit → in_flight==2 且队列深 0，热更逐档断言 in_flight/relative 档 busy 严格低于 idle、queue 档两侧权重相等（现状回归）。默认档 queue 与历史 3D 权重逐位一致。 |

---

## 4. 新算法默认关闭总表

> 修 bug 类（#1/#5/#13）与 #4 默认值即修复本体、无功能开关；#3 激活键控于新形态；#6 默认档即现状。下表覆盖全部 16 个开关化/默认关闭配置面。所有默认值均由 `tests/config_toml_example_test.rs` 结构性断言 + 各自的「关闭态==基线」行为对照测试双重锁定。

| 建议 | 开关/参数 | 配置路径（config.toml 行号，本轮核实） | 默认值 | 启用指引 |
|------|-----------|----------------------------------------|--------|----------|
| #2 换路覆盖 | `cover_all_paths` | `[resilience]`（:288-289） | `false` | 置 true：透传/流式获得 CB 包裹与首字节前换路；注意 Open 状态将拒绝透传流，需评估影响面后开启 |
| #7 失败率软降权 | `failure_rate_gate`（+threshold/min_samples/penalty） | `[resilience]`（:290） | `false`（0.5 / 5 / 0.1） | 置 true：劣化 vendor（≥5 样本且失败率>0.5）权重 ×0.1；热读即时生效 |
| #9 重试预算/守门 | `retry_budget_secs`、`amplification_gate` | `[retry]`（:114-115） | `0`（不限时）、`false` | budget_secs>0 启用总时间预算；gate=true 启用亚稳态收紧（f>0.8·f_c 时 max_attempts 收紧到 1+warn）；热读 |
| #5 抖动回滚阀 | `spread_jitter` | `[retry]`（:112） | `1.0`（行为修复本体） | 置 0 恢复旧行为（Retry-After 原样、5xx 固定 1s）——回滚阀，非功能开关 |
| #4 超时逃生阀 | `timeout_ms` / `stream_ttft_ms` / `stream_idle_ms` | `[[vendors]]` 段 | 60000 / 60000 / 30000（行为修复本体） | 任一置 0 = 对应层不限时；长思考模型调大或置 0；热读 |
| #6 负载信号 | `load_signal` | `[routing]`（:170） | `"queue"` | 置 `"in_flight"` 或 `"in_flight_relative"`；未知值回退 queue |
| #8 延迟候选集 | `latency_buffer`、`ewma_tau_ms` | `[routing]`（:184/:175） | `0.0`、`10000` | buffer>0 启用 min+buffer 候选集（建议先确认 ewma 口径数据再调 w_latency 非零，顺序依赖见 config.toml 注释） |
| #10 会话亲和 | `affinity_mode`、`affinity_tenant_scoped`、`affinity_boost_factor` | `[routing]`（:210/:220） | `"hard"`、`false`、`1.0` | mode="soft" 软化评分；tenant_scoped=true 租户键隔离（实为租户劫持修复，**建议上线开启**）；注意键格式变更使存量绑定失效（TTL 1h 内自然过渡） |
| #19 请求级权重 | `allow_request_weight` | `[routing]`（:190） | `false` | 置 true 后 X-Weight 头参与路由（四级链最高优先级）；热读 |
| #19 fail-open | `fail_open_when_all_cooling` | `[routing]`（:198） | `false` | 置 true：全候选被剔除时回落忽略 CB 状态的候选集；**仅建议有兜底容量的部署开启**（覆盖 CB Open 超时未过窗口，语义弱于 LiteLLM health-check 限定） |
| #19 Retry-After 提示 | `retry_after_hint` | `[routing]`（:204） | `false` | 置 true：全候选 Throttled 时返回 429 + Retry-After（min reset_secs）而非 503；热读 |
| #14 SLA 打分项 | `sla_v`、`sla_alpha` | `[routing]`（:235/:239） | `0.0`、`0.95` | sla_v>0 启用惩罚因子与虚拟队列 Q（Q 重启归零，Lyapunov 收敛重建）；需 [[pricing]] 定价数据，缺价 vendor 记候选集中位数 |
| #15 LinUCB | `selection`、`ucb_alpha` | `[routing]`（:154/:161） | `"weighted"`、`1.0` | selection="linucb" 启用（未接线 BanditState 自动回退 weighted）；A/b 重启归零，特征集变更不可迁移 |
| #16 优先级准入 | `enabled`（+congestion_threshold/系数） | `[admission]`（:307-310） | `false`（0.9 / 2.0/1.0/4.0） | 置 true + `[[auth.keys]]` 配 tier（缺省 elastic）；拥塞时低档（spot）先被 shed；健康检查永不 429；全参数热更 |
| #11 滑动窗口 | `sliding`（Token/Interval 各一） | `[[vendors]]` rate_limit 段 | `false`（Fixed 现状） | 置 true 换 ShardedSlidingWindowLimiter（消除翻越点双花）；vendor 构造期消费，restart-required |
| #12 缓存 TTL 联动 | `ttl_load_linked`（+w_l/w_q/beta_max/l_target/q_target/categories） | `[cache]`（:335 起） | `false`（0/0/1.5/1000/0.5/空） | 前提：`[cache].enabled=true`（当前 :327-328 为 false，缓存整体关闭）；联动需 true 且权重非零；类别乘数表配了即乘、独立于联动开关 |
| #18 token 票据 | `token_weighted`（+token_ticket_min/max/default） | `[[vendors]]` concurrency 段 | `false`（1/16384/4096） | 置 true 后并发票据按预估 token 计费；in_flight 随之变 token 加权口径；中文/多模态载荷建议保守调低 token_ticket_max；restart-required |
| #3 入站限流 | `rate_limit.rules` | `[rate_limit]`（:68） | `[]`（空=现状） | 填 `[[rate_limit.rules]]`：path=None 注入 Governor（注意 per_tenant 不生效）、path=Some 走端点表（per-tenant 生效）；restart-required（热下发列二期） |

---

## 5. 门禁与验证

### 5.1 各波合入门禁（材料记录）

| 波次 | check | test | 复审整改后 |
|------|-------|------|-----------|
| W1（#1/#5/#13） | 通过（整改 0 轮） | 退出码 0 | — |
| W2（#4/#3） | 通过（整改 0 轮） | 退出码 0 | check 0 / test 0（整改 2 处） |
| W3（#2） | 通过（整改 0 轮） | 退出码 0 | — |
| W4（#6/#8） | 通过（整改 0 轮） | 退出码 0 | check 0 / test 0（整改 1 处） |
| W5（#7/#9/#19/#10） | 通过（整改 1 轮，遗留后修复） | 退出码 0 | check 0 / test 0（复审整改 1 处） |
| W6（#14/#15/#16/#17/#11/#12/#18） | 通过（整改 0 轮） | 退出码 0 | check 0 / test 0（复审整改 2 处） |

W5 波内各条门禁实跑记录（材料）：#7 后 22 组 530 通过 → #9 后 538 → #19 后 550 → #10 后 557，clippy 逐条 0 告警。W6 波内：#14/#15 后 22 目标全过；#16 起新增 admission_test 后 23 目标全过；clippy 逐条 0 告警（修掉 type_complexity/too_many_arguments/derivable_impls/await_holding_lock/private_interfaces 等若干处）。

### 5.2 收尾验证（材料记录）

clippy 退出码 0、全量测试退出码 0。

### 5.3 本轮撰写报告时的抽查实跑（本报告作者执行）

| 命令 | 结果 |
|------|------|
| `cargo check --tests --all-features` | **退出码 0**（Finished dev profile）。有 8 条 rustc warning：`tests/api_test.rs:13/:397/:908/:1295/:1717/:2129/:2429` 的 unused imports（DbConfig、DbPool）与 `:2976` 的 unused variable（app）。属测试目标 lint；`cargo clippy`（未带 `--tests`，生产门禁 target 为 lib/bins）不覆盖它们，与「clippy 0 告警」声明不矛盾，但如实记录在此。 |
| `cargo clippy --all-features -- -D warnings` | **退出码 0，error 计数 0**（grep "^error" 计 0）。唯一 warning 为第三方依赖 proc-macro-error2 v2.0.1 的 future-incompat 提示（基线即有，非本项目代码）。 |
| `cargo test --all-features` | **退出码 0，23 个测试目标、累计 627 通过 / 0 失败**（awk 汇总全部 "test result" 行；抽见 router_test 63、vendor_test 79、retry_test 26、rate_limit_test 7、sse_test 24、protocol_test 71 等）。 |

### 5.4 行号与符号抽查（本轮 grep 实测命中）

`nexusflow_circuit_config`（manager.rs:154）、`CONNECT_TIMEOUT`（:224）、`QUEUE_WAIT_BUDGET`（:230）、`apply_attempt_timeout`（:671）、`get_in_flight`（:982）、`fleet_concurrency`（:994）、`dispatch_stream_once`（:1146）、`TicketPolicy/estimate_cost_unified/estimate_cost_json`（:84/:95/:109）、四处 `acquire_weighted`（:410/:1251/:1564/:1669）、`time_window_active`（rate_limiter.rs:30）、`in_flight`（:85）、`WindowInner`（:140）、`acquire_weighted`（:112）、`spread_jitter/spread_delay/amplification/critical_failure_fraction`（retry.rs:188/:274/:290/:302）、`allow_request_weight`（router.rs:151）、`fail_open_when_all_cooling`（:456/:591）、`affinity_mode`（:483/:615）、`linucb_route`（:721）、`min_buffer_pick` 调用（:912-914）、`load_signal`（:1068-1070）、`failure_rate_gate`（:1100-1107）、`sla_v` 热读（:1125-1132）、`lps_waits`（:1161）、`lps_delta`（:1183-1194）、`lps_deadline_gate`（:1217-1231）、`relative_in_flight_to_len`（:1393）、`apply_affinity_boost`（:1404）、`lps_wait`（:1452）、`sla_median_cost`（:1479）、`finalize_all_throttled`（api/mod.rs:260）、`record_vendor_outcome_at/vendor_failure_window/vendor_window_total`（metrics.rs:343/:362/:569）、`ResilienceSection/AdmissionSection/RateLimitSection/RateLimitRule`（config/mod.rs:844/:913/:982/:989）、`InboundLimitState`（router_builder.rs:32）、`ttl/ttl_load_linked`（response_cache.rs:188/:192）、`admission_priority/AdmissionState`（admission.rs:73/:113）、`tier_for_key`（auth.rs:111）、`linucb_features/BanditState/ucb/update`（bandit.rs:75/:228/:271/:296）、`with_bandit/ResponseCache::new`（main.rs:130/:110）、config.toml 各段键（:68/:112-115/:154-161/:170-184/:190-220/:235-260/:288-290/:307-310/:330-335）、测试落点（fallback_test.rs:346、circuit_breaker_test.rs:126、retry_test.rs:344/370/394、vendor_test.rs:1729、manager.rs:2066/:2090）。

---

## 6. 顺延清单、遗留风险与建议后续

### 6.1 顺延清单

**为空。** 19 条建议全部落地，无顺延项（各波 deferred 均为空）。

各条**内部列为后续**的子项（非独立 recId，如实列出）：

| 来源 | 后续项 | 理由 |
|------|--------|------|
| #13 | 跨午夜窗口 window_end 的精确次日时刻；per-window 时区字段（LiteLLM ZoneInfo 对照） | 进阶项；当前退化为保守减半，不阻塞正确性 |
| #3 | [rate_limit] 热下发（apply_config） | 一期标注 restart-required（与 [server]/[cors] 先例一致） |
| #15 | 预算感知变体 UCB/max(ĉ−β,ε) | plan 明示列为后续 |
| #19 | 首次路由即全 Throttled 的入口场景（handler 首路由早退、Validation 错误丢失原因） | plan 明示列为后续小条目；(c) 覆盖面限于 execute_request_with_fallback |
| #12 | 语义缓存 | papers-caching 结论：无日志数据不得开启，维持暂缓 |
| #8 | latency_factor 按请求类型取对应口径 | 需改 compute_weight 签名与调用链，超出评审方案文本，未实施、未记 deferred |
| #16 | α_slo 的 ℓ* 项 | 无数据源，恒 0、该项恒 1（预留） |

### 6.2 遗留风险（按波次/条目，均已在代码注释/配置注释/测试注释声明）

1. **#2（19 条中行为面风险最高）：** 开启 cover_all_paths 后，CB Open 状态会拒绝透传流（开启时）；返回类型异构需平行 fallback 实现而非强泛型统一；流式重派在首字节前无计费风险，但日志 actual_vendor 语义已与记账口径对齐。**运维开启前必须评估影响面。**
2. **#4（行为面变化最大）：** 原本可无限等待的请求现在会超时失败——长思考型模型需调大 timeout_ms/stream_ttft_ms 或以 0 关闭；外层预算 60s→(60s+timeout_ms) 使排队饥饿请求的最坏等待变长（有意取舍）。
3. **#3：** per-endpoint 走中间件自筛而非 Governor 规则是 Matcher 能力边界的显式妥协；path=None 全局规则 per_tenant 不生效；热更一期不做（restart-required）。
4. **#1：** rate=1.0 在「窗口内 100% 调用皆 >60s 且全成功」病态场景仍可触发（与失败路径条件重合，行为一致）；新增测试墙钟约 +4s。
5. **#10：** soft 模式弱粘性可能提升缓存/前缀复用失配率（affinity_hit_rate 指标已有，观察窗口需定义）；键格式变更换代使存量绑定全部失效（一次性，TTL 1h 内自然过渡）；bind_if_absent 的 get+set 竞态在并行首请求下仍可能双写（可接受）。
6. **#7：** 429 计入失败窗口后上游正常限流也可能触发降权——penalty=0.1 而非剔除，风险有界。
7. **#9：** max_total_attempts 收紧是跨 vendor 语义；误触发时代价是退化为单尝试（fail-safe 方向正确）；C/λ 是全网估计值。
8. **#14/#15/#17 打分项：** ŝ 用错误率代理满意度是弱信号；Q 与 A/b 状态重启归零；LinUCB 奖励是成败 0/1（稀疏反馈问题未解）；三者同插入点独立乘法叠加，串行合入已保证顺序可追溯。
9. **#17：** 乘法软约束 1/(1+δ·Ŵ/Ŵ_ref) 是对报告线性 −δ·Ŵ 的**有意偏差**（线性负项触发 compute_weight 非正归零保护造成权重归零剔除）；若后续需严格线性语义需先改归零保护。Ŵ 对 ρ 敏感，CAP 截断必须有。
10. **#11：** reset_secs 的滑动近似（窗口全长为上界）使 get_window_info 的 window_end 精度略降——window_factor 仅 is_ending_soon(<300s) 时惩罚，影响可忽略但已注释。
11. **#12：** EWMA 均值替代 P95 是声明过的口径降级（高估方向安全）；缓存默认关闭使影响面天然受限。
12. **#18：** 粗估（字符数/4）对中文/多模态载荷偏差 1.5-3×；票据饥饿（长会话占满）是机制内生风险，token_ticket_max 封顶为逃逸阀；in_flight 含义随开关变化已文档注明。
13. **#16：** 拥塞信号是网关侧近似（不含上游自身限流；guard 生命周期为中间件作用域、流式 body 传输期不在途计数）；定位为「粗分类 shed 顺序」，不承诺论文的 TTFT 数字。
14. **#8：** ewma_latency_ms 语义变化（四路混流→e2e 别名）影响 /admin/metrics 外部消费方（字段保留，文档+config.toml 已声明；仓库无 CHANGELOG 文件，声明落在字段 doc 与本报告）。
15. **测试面存量瑕疵（本轮实跑发现）：** `tests/api_test.rs` 8 条 rustc warning（unused imports ×7 @ :13/:397/:908/:1295/:1717/:2129/:2429、unused variable ×1 @ :2976）——不影响 clippy 生产门禁与测试正确性，建议下次触碰该文件时顺手清理。
16. **环境事实：** AGENTS.md 要求的 GitNexus MCP 工具（gitnexus_impact/detect_changes）在本环境不可用，W4-W6 以 grep 手工核对全部受改符号调用面代替（各波 notes 已记录核对范围）。

### 6.3 建议后续

1. **启用顺序建议**（依赖顺序，写入 config.toml 注释的原则）：先让观测面跑起来（#8 分道为纯观测，自动生效）→ 再开行为开关（#7 failure_rate_gate → #6 load_signal → #19 三开关）→ 最后开打分项（#14 sla_v → #17 lps_delta → #15 selection="linucb"），每步观察权重分布与路由质量。
2. **上线前建议开启** `affinity_tenant_scoped = true`（#10 键隔离实为租户劫持修复，方案已明示建议）。
3. **运维文档补齐：** cover_all_paths 影响面（Open 拒绝透传流）、超时三参数对长思考模型的影响、token_ticket_max 对中文/多模态的保守取值建议——均为已声明但散落在注释中的运维要点，建议汇入部署文档。
4. **测试基建：** 修复 tests/api_test.rs 的 8 条 unused lint；mockito TTFT 手写桩（W2 引入的 tokio TcpListener 桩）可沉淀为共享测试工具。
5. **后续迭代候选：** 第 6.1 节所列 7 项内部后续子项，优先级最高者为 #3 的 [rate_limit] 热下发（一期 restart-required 对运维不友好）与 #19 的首路由全 Throttled 入口场景。

---

## 附：写实性声明

- 本报告各波实施内容、测试清单、偏差记录均来自各波执行记录（材料给定，逐条对应 recId）；门禁结论中「材料记录」部分为本轮未重放的波次时点结果，「本轮实跑」部分为报告撰写时在当前工作区执行的命令与输出（第 5.3 节）。
- 第 5.4 节所列行号为本轮在当前源码上 grep 实测；各波记录中的行号（如 W6 #18 的四处 acquire :357/:1191/:1497/:1595）为合入时点快照，后续合入会使行号漂移，引用时以本轮实测行为准。
- limiteron 库侧行号（如 types.rs:580/:603、constants.rs:106/111、sharded_sliding_window.rs 等）依据调研产物核实声明引用，本报告作者未直接重读库源码，已标注「材料核实」。
- 全程未运行任何 git 命令（非 git 仓库）。
