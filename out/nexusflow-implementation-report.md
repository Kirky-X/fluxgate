# NexusFlow 优化建议实施交付报告

**日期**：2026-10-01
**工作目录**：`/home/dev/projects/NexusFlow`（crate：nexusflow，Rust 2021，非 git 仓库）
**报告依据**：16 条优化建议实施方案（recId 1–16，MoSCoW：must 10 / should 5 / could 1）、5 个波次的实施结果与门禁记录、本报告撰写会话的收尾复核实跑结果。调研底稿见 `out/nexusflow-optimization-report.md` 与 `out/research/`（14 份笔记）。

---

## 1. 执行摘要

**范围与完成度**：16 条优化建议分 5 个波次全部实施落地（16/16），其中 must 10 条、should 5 条、could 1 条。顺延项均为计划内 deferred（4 项：#13 可选快照备份、#6 release 实机验证、#12 语义缓存二期、#16 第①③步），另有 1 项跨波次约定项（#5⑥ 流式 token 记账）未随波次交付，见 §5.2。

**基线**：实施前已完成的版本对齐工作将 `cargo check --tests --all-features` 从 5 个库侧错误 + 11 个测试侧错误修复至 exit 0（双会话实跑 5.04s / 4.44s），残余 20 个 lib 警告与 cost-tracking 未声明 feature 的 check-cfg 警告（tests/cost_tracker_test.rs:14、tests/vendor_cost_integration_test.rs:13）——后者即波 1 #1 的范围，已消除。

**主要成果**（按域）：

| 域 | 修复/落地内容 |
|----|--------------|
| 生产装配 | `/v1`、`/admin` 从必 404 恢复可达（main.rs 补 merge create_router）；认证升级 SHA256 摘要存储 + 常量时间比较；健康路径实为 `/api/v1/health` |
| 熔断 | 手写三态机（HalfOpen 死锁：熔断一次即永久拒绝）整体删除，迁移 `limiteron::circuit`，熔断状态接进路由过滤 |
| 限流 | Token 窗口永不重置导致 vendor 被永久排除、check/consume 分离超卖两缺陷由 `FixedWindowLimiter` 根除；流式请求并发上限真实生效 |
| 重试 | 首试 2s 起步偏移修复（1s 起步）；jitter、Retry-After、耗尽/429/5xx 换 vendor（存量 FallbackChain 复活） |
| 流式 | SSE 增量分帧器（修 UTF-8 多字节劈裂、嵌套 SSE 重包装）；跨协议流式转换接通 IR；上游错误不再伪装成 200 正文 |
| 协议 | tool_use/tool_result/system/stop_reason 四处转换断点修复（protocol_test 60→72） |
| 路由 | queue_factor 接真实队列（3D 权重从退化 2D 恢复）；EWMA 延迟/错误率第 4 维；系数热更 + 预设 + Po2C 实验开关；会话/前缀亲和 |
| 可观测 | metrics 覆盖全部执行路径（流式此前零指标）；top-1 占比与流量份额熵 + 路由崩溃告警钩子 |
| 数据 | 日志管道真正接线（AsyncLogger/PreAggregator 此前零调用方）：单事务批量写、背压计数、失败落库、prompt_tokens/status_code 语义修复 |
| 治理 | embeddings 从每请求裸 `reqwest::Client::new()` 回归 VendorManager 治理管道 |
| 缓存 | 精确响应缓存落地 oxcache（租户隔离键 + 确定性请求门控，默认关闭）；会话亲和表同用 oxcache |

**门禁终态**：各波实施轮 check 均一次通过（0 整改轮）、test 退出码 0；各波复审整改后（共 11 处）波 2–5 check/test 均 0/0。本报告撰写会话收尾复核实跑：`cargo check --tests --all-features` exit 0、`cargo clippy --all-features -- -D warnings` exit 0（0 代码告警，符合项目生产标准）、`cargo test --all-features` exit 0（**463 passed / 0 failed / 0 ignored**）。详见 §4。

**自研库迁移**：limiteron（熔断/窗口限流/重试延迟）、confers（配置加载/热读，正式入列依赖）、oxcache（亲和表/响应缓存）、dbnexus（参数化事务批量写）、sdforge（建站返回原生 Router 后 merge）五库实际承接迁移；limiteron fallback/quota/telemetry、confers validator/持久化、inklog 等 8 项能力经评估后明确不采用（原因逐条记录，见 §3）。

---

## 2. 逐波实施明细

> 每条按「改动 / 文件 / 库映射 / 测试 / 与方案的偏差」记录。除特别注明外，`file:line` 引用来自实施会话记录；本会话核实过的现状以"（本会话核实）"标注。

### 第 1 波（must：#1、#6、#13）

#### recId 1 — 声明 cost-tracking feature 并入 full（must）

- **改动**：`Cargo.toml` [features] 新增 `cost-tracking = ["dep:dashmap"]`（Cargo.toml:28）并追加进 full（Cargo.toml:18）；[dependencies] 新增可选 dashmap（Cargo.toml:94）。
- **文件**：Cargo.toml。
- **库映射**：dashmap 6（第三方通用并发 map，CostTracker by_vendor/by_model 容器）。oxcache 评估结论：`DashMapMemoryBackend` 是带 TTL 语义的缓存后端封装，非裸并发 map 能力暴露，不适合当 CostTracker 容器，维持 dashmap。resilience 未动（arc-swap/backon 移除归 #2/#7）。
- **测试**：`cargo check --all-features` 0 error；cost_tracker_test 7 过、vendor_cost_integration_test 4 过——两个 `#![cfg(feature = "cost-tracking")]` 门控测试目标从声明前静默跳过变为真实编译+运行；`cargo check --no-default-features --features protocol` 非全组合抽查通过。
- **偏差**：方案写 `version = "1"`，实测 dashmap 1.2.1 无 `entry()` 且 `new()` 需传 chunk 参数，而 `src/utils/cost_tracker.rs` 使用 dashmap 6 API、confers 自身亦依赖 dashmap 6.2，按源码现状取 `version = "6"`（Cargo.toml:92-93 注释记录原因）。

#### recId 6 — 生产装配链修复 + 认证挂载与 SHA256 升级（must）

- **实机验证先行**（debug 构建，release 见 §5.1）：修复前 `/health`、`/v1/chat/completions`、`/admin/metrics` 三端点全部 CONN_FAIL。根因四层：
  1. sdforge rc.5 对空 `ApiKey keys` 是 build 时直接报错（fail-fast，`../sdforge/src/http/http_impl.rs:389`），非方案预判的 fail-open/closed → 改 `AuthConfig::None`，认证由自建中间件承担；
  2. `allowed_origins=["*"]` 被 rc.5 CORS 校验拒绝（sdforge cors.rs:33）→ main.rs 读 [cors] 段并过滤 `"*"`，过滤后为空则不挂 CORS 层；
  3. main.rs 建站后未 merge `create_router` → `build_with_config` 后显式 `app.merge(...)`；
  4. `#[forge]` 宏的 `cfg(feature="http")` 按目标 crate feature 求值，nexusflow 无 http feature 时路由注册代码被整体 cfg 掉 → Cargo.toml 新增空 feature `http` 挂进 web-server。
- **认证**：`ApiAuthState` 改 SHA256 hex 摘要存储（`add_api_key` 现场摘要、`from_hashes`/`from_section` 接受配置摘要）+ 手写常量时间比较 `constant_time_eq`（摘要长度恒 64，无长度泄漏）；`Config` 补 [auth]（enabled/api_keys/header 子段）与 [cors]（serde 全 default，兼容旧配置）；`create_router` 在限流层之上叠 auth_middleware，层序 Trace→auth→rate_limit→handler。
- **连带修复**（create_router 从未被生产路径执行暴露的潜伏 bug）：axum 0.8 路径参数 `:id`→`{id}` 语法（旧语法直接 panic）；健康检查实际路径为 `/api/v1/health`（`#[forge(version="v1")]` 宏加版本前缀，sdforge-macros lib.rs:1886）——装配本身无恙，auth/限流豁免列表同步补齐；六个测试文件的 `Config` 字面量构造点补 `auth:None, cors:None`（编译必需，未动测试逻辑）。
- **文件**：src/main.rs、src/api/router_builder.rs、src/middleware/auth.rs、src/middleware/mod.rs、src/config/mod.rs、Cargo.toml、config.toml、tests/assembly_smoke_test.rs（新增）及 5 个既有测试文件。
- **库映射**：sdforge `build_with_config` 返回原生 axum::Router（`http_impl.rs:274`）+ axum `Router::merge`/`from_fn_with_state`；SHA256 摘要手写（sha2 0.10 + hex）——七库无 API key 摘要存储/常量时间比较能力，sdforge AuthConfig 仅站点级明文 keys，sdforge security 是 RBAC 域不符。
- **测试**：新增 tests/assembly_smoke_test.rs 7 项 oneshot 断言全过。实机可达性矩阵（debug）：`/api/v1/health` 无 key→200；`/v1/chat/completions` 无 key→401、错 key→401、对 key+缺 model→400（认证通过进入 handler）；`/admin/metrics|config|vendors` 无 key→401、对 key→200。诊断用临时探针已删除，服务进程与端口已清理。

#### recId 13 — 配置热更新地基：校验共享化 + 原子写 + 执行层热读（must）

- **改动**：
  - 校验共享化：loader 私有 validate 提为 `pub validate_config`，加载与更新路径共用；
  - `update_config` 重写为事务序：validate → toml 序列化 → 同目录 tmp 文件 + `std::fs::rename` 原子替换 → 成功后才 `DynamicField::update` 换内存态（校验/落盘失败磁盘与内存均不变，rename 失败清理 tmp）；
  - 路径记忆：`ConfigManager` 加 `path: Arc<RwLock<Option<PathBuf>>>`，`ConfigLoader::load` 注入，update 写回同一路径，无路径时拒绝落盘——消灭硬编码 CWD `"config.toml"`；
  - 执行层去快照：`VendorInstance` 删除 config 快照字段（保留 queue/limiter/CB/vendor_id），worker 与 execute_stream/passthrough/passthrough_stream 三条直接路径均经 `fresh_config`（=`ConfigManager::get_vendor`）热读；pricing 读取点由启动快照改为 worker 计算时从热配置现读构建 CostCalculator；vendor 已从配置移除时请求以 VendorUnavailable 拒绝。
- **文件**：src/config/manager.rs、src/config/loader.rs、src/config/mod.rs、src/vendor/manager.rs、Cargo.toml、tests/config_test.rs 及 5 个既有测试文件。
- **库映射**：confers 0.6.0-rc.5 正式入列依赖（`default-features = false, features = ["toml","dynamic"]`，Cargo.toml:97，本会话核实）：`loader::load_file + LoaderConfig` 做路径防护/大小限制加载，`dynamic::DynamicField<Config>` 做内存热读态（read() 返回 `get_ref()` 的 Arc）。**三项不迁移（已核实并如实记录）**：①vendor/model id 去重不迁 `confers::validator`——其 ValidationRule 仅字段级规则（`impl_/validator.rs:42-62`），无唯一性/跨条目规则，`validate_config` 保留手写；②原地原子写不迁 `SnapshotManager`——其 save 是时间戳命名新文件+prune（`impl_/snapshot.rs:337`），无原地 rename 语义，保留手写 tmp+rename；③confers 的 ConfigValue 无 serde Deserializer 且 AnnotatedValue 带 6 字段元数据壳——强类型桥接手写剥壳递归（loader.rs 的 `annotated_to_json`/`config_value_to_json`）。
- **测试**：tests/config_test.rs 新增 hot_reload 4 项（①重复 vendor id 的 update 被拒且磁盘字节不变；②合法 update 落盘且 [auth]/[cors] 段保留——回归"热更新抹段"；③update 后 read() 立即可见；④update 后 get_vendor 入口拿到新 api_key_env——回归"改了没生效"）+ manager.rs 单测 4 项全过；实机 e2e：POST /admin/config 回写后磁盘保留全部段内容、GET 立即见新值、无 .tmp 残留；vendor_cost_integration_test 4 项（cost 精确值断言）在去快照后仍全过。
- **偏差**：LoaderConfig 默认拒绝绝对路径，按"配置路径来自操作者显式 CLI 参数、可信输入"放行 `allow_absolute()`（symlink/max_size 防护保留默认）。

**波 1 门禁**：check 通过（0 整改轮）、test exit 0；复审整改 2 处后 check exit 0、test exit 101（门禁记录原文如此，材料未附失败明细；波 2 起各轮全量 test 均 0，本会话复核全量 463 过 0 挂，见 §4）。顺延：#13 的 confers 快照备份（should 级增强）、#6 的 release 实机验证（见 §5.1）；另实测发现 sqlx 默认不创建新 SQLite 文件、需 `?mode=rwc`（AGENTS.md"数据库文件自动创建"不成立），属 database 层既有问题未修（见 §5.3）。

### 第 2 波（must：#2、#5；should：#7）

#### recId 2 — 熔断器迁移 limiteron::circuit + 熔断状态接进路由过滤（must）

- **改动**：
  - 手写 `CircuitBreaker` 整文件删除（死锁根因：half_open_calls 从不在探针间清零 + success_threshold=3，首个探针成功后 success_count=1<3 恒拒，熔断一次即永久拒绝）；grep 复核 src/ 内 `arc_swap` 代码引用仅该文件三处，`dep:arc-swap` 从 resilience feature 与依赖表同步移除（本会话核实 Cargo.toml 已无 arc-swap）；
  - `VendorInstance.circuit_breaker` 与 manager 级表换 `limiteron::circuit::CircuitBreaker`，配置 `CircuitBreakerConfig::new(5, 3, Duration::from_secs(30)).error_classifier(...)`（half_open_max_calls=3 且 half_open_calls 每次状态转换统一清零，探针序列可收敛）；测试经 dev-dep 的 test-clock feature 注入 MockClock；
  - 新增 `src/vendor/resilience/error_map.rs` 双向映射层：`to_limiteron` 将 NexusFlowError 编码为 `nexusflow:<variant>:<detail>` 前缀串；`from_limiteron` 将 LimitError（拒行串）→`VendorUnavailable{vendor:"{id} (circuit open)"}`、带前缀 Other 解析还原、解析失败降级 VendorUnavailable（有损降级以测试锁定）；`NexusFlowErrorClassifier`：VendorUnavailable/Timeout 计失败、RateLimited/Validation/Protocol 不计（语义对齐库默认分类器的"保护机制错误不计失败"）。映射层因库 `execute`/`ErrorClassifier` 签名锁定 LimiteronError 所致，非等价手写熔断逻辑；
  - 删除 `force_circuit_open`（已核实 limiteron 公开面无 force API，transition_to 为私有）；`get_circuit_state` 改 async `cb.get_state().await`；CB 包装点改 `cb.execute(|| inner().map_err(to_limiteron)).await` 后 from_limiteron 还原；
  - 路由过滤接线：worker 完成分支回写 VendorState——成功→mark_available+reset_error、失败→inc_error、circuit-open 或 CB 已 Open→mark_unavailable、HalfOpen→mark_throttled；router 的 `== Available` 过滤借同步状态自动生效。
- **文件**：src/vendor/resilience/circuit_breaker.rs（删除）、src/vendor/resilience/error_map.rs（新增）、src/vendor/resilience/mod.rs、src/vendor/manager.rs、src/error.rs、Cargo.toml、tests/circuit_breaker_test.rs、tests/vendor_circuit_breaker_test.rs（本会话核实：circuit_breaker.rs 已不存在、error_map.rs 存在）。
- **测试**：重写为自然路径驱动——mockito 预置 5×500 自然熔断（配置 [retry] max_retries=0 保证每请求恰好 1 次 HTTP）+ `route()` 不再选中该 vendor + 公开 `update_status(vendor, Unavailable)` 直测过滤 + 死锁回归（超时后 3 探针连续成功→Closed，手写版此场景永久卡 HalfOpen）。
- **偏差**：①方案要求 get_status 保持同步，但 vendor_test.rs:655 锁定"Token 耗尽→get_status 立即 Throttled"，而 #5 将 check() 改为 async peek（FixedWindowLimiter 私有计数无同步读口）→ get_status/get_window_info async 化（连带 router.rs 三处、admin.rs 一处、相关测试加 .await）；CB 状态仍按方案由 worker 回写，async get_state 未扩散到 router。ThreadRng（!Send）不得跨 await，weighted_select 的 rng 创建移至权重收集后。②worker 回写补强：第 threshold 次失败瞬间错误串非 circuit-open 但 CB 已转 Open，原两分支都不命中→增加 `cb_state==Open→mark_unavailable` 分支（vendor_circuit_breaker_test 用例 1 锁定）。③过渡期测试用 400（Fatal）驱动避免旧重试循环放大时长，#7 落地后回归 5×500 终态。

#### recId 5 — 限流记账修复：窗口重置 + 原子预扣 + 流式 permit 生命周期（must）

- **改动**：
  - Token/Interval 变体迁移 `limiteron::limiters::FixedWindowLimiter`：`allow(cost)` 内置 `check_and_reset_window()` 惰性重置 + CAS 原子消费，一举根除两缺陷——should_reset 只匹配 Interval 且 `_ => false` 致 Token 窗口永不重置（vendor 被路由永久排除）、check() 与 consume_tokens 分离非原子可超卖；`check()` 改 `peek` 非消费预检（同时为 get_window_info 提供精确 remaining/reset_secs）、`consume_tokens` 改 `allow` 全有全无；
  - `RateLimitConfig::Token` 加 `window_hours: Option<u64>`（serde default 24h，兼容旧配置）并进 config.toml 示例；`ConcurrencyConfig` 加 `max_concurrent_requests: Option<usize>`（此前硬编码 =queue_size 100）；删除后台 reset worker（惰性重置取代）；
  - worker 顺序对调：先 `queue.recv` 后 `rate_limiter.acquire`——空闲不烧 RPS 令牌、突发期消除 100ms 轮询延迟；Token peek 拒绝时 mark_throttled 回写；
  - 流式 permit：`drop(permit)`/`let _permit` 两处（execute_stream_request 与 execute_passthrough_stream_request，方案只点名前者，后者同类缺陷一并修）改为 permit move 进返回 Stream（`futures::stream::unfold` 持有，Box::pin 解 Unpin），并发上限对流式真实生效；
  - 流式 token 记账按方案留注释钩子（依赖 #3 SSE 分帧），**该跨波次项未随波 3 交付，见 §5.2**。
- **文件**：src/vendor/rate_limiter.rs、src/vendor/manager.rs、src/config/mod.rs、src/router/router.rs、src/api/admin.rs、tests/vendor_test.rs。
- **库映射**：`limiteron::limiters::{FixedWindowLimiter, Limiter}`（quota-control feature 已启用）；`with_clock` 供 MockClock 注入。评估 `limiteron::quota::QuotaController` 不采用（per-user keyed 配额、需 QuotaStorage 后端；vendor 级单键窗口用 FixedWindowLimiter 更轻、零存储依赖）。TimeWindow（每日时段）变体手写保留——limiteron 无"时段"窗口原语，fixed/quota 均为时长窗口。
- **测试**：tests/vendor_test.rs 增补 4 项：①假时钟 Token 窗口重置回归（永久 Throttled 根除）；②100 并发对 max=50 窗口 allow 恰 50 成功（CAS 原子性）；③流式 permit 生命周期（第 3 流在前两流存活时被阻塞）；④worker 空闲 10s 后 3 请求 <500ms（顺序对调回归）。
- **偏差**：测试④设计修正——TokenBucket 容量硬编码 =queue_size=100，空闲 2s 只烧 20 个令牌、新旧顺序行为无法区分，改为空闲 10s（旧顺序恰好烧光 100 个）后断言。

#### recId 7 — 重试策略升级：延迟偏移 + jitter + Retry-After + 耗尽换 vendor（should）

- **改动**：
  - 修延迟偏移：原实现先 `retry_count += 1` 再取指数致 2s 起步、与注释 "1s, 2s, 4s" 矛盾——延迟计算迁 `limiteron::retry::delay_for_attempt`（Cargo.toml features 追加 "retry"，本会话核实 Cargo.toml:59 注释记录"删除 backon 后唯一的重试延迟实现"），首试 1s（测试①锁定）；
  - `limiteron::retry::RetryPolicy::execute` 不采用（如实记录：其签名无 per-attempt 延迟注入点，429 的 Retry-After 分支需要自管薄循环）——提取纯函数 `retry_delay_for(category, attempt, retry_after, policy)` 到 utils/retry.rs（delay_for_attempt 打底 + Retry-After 覆盖），纯函数无 IO 可脱离 HTTP 栈单测；
  - jitter=0.2；重试参数热读：Config 新 [retry] 段（max_retries/retryable_status_codes/initial_delay_ms/factor/max_delay_ms/jitter/max_total_attempts 共 7 字段 + Default），worker 每请求现读，POST /admin/config 即热生效；429 优先读上游 Retry-After（秒数形式，HTTP-date 降级 None）；
  - 耗尽/429/5xx 换 vendor：`VendorConfig` 加 `priority: u8`（default 1）；Router 新增 `route_excluding`（排除集）与 `build_fallback_chain`（priority→FallbackTarget，trigger=All）；api 层 `execute_request_with_fallback` 包住 openai/anthropic 单发调用（总尝试预算 max_total_attempts 默认 8 防爆炸；无候选时原错误冒泡 503；actual_vendor 记最终成功者）；
  - 冗余重试栈合并：删除 backon 版 `src/vendor/resilience/retry.rs` 及悬空再导出（vendor/resilience/mod.rs 的 `pub mod retry`/`pub use retry::with_retry`、utils/mod.rs 的 with_retry 再导出、utils/retry.rs 的 cfg(resilience) 再导出），backon 依赖移除，resilience feature 变纯门控（本会话核实 Cargo.toml 已无 backon、src/vendor/resilience/retry.rs 已不存在）。
- **文件**：src/vendor/manager.rs、src/vendor/resilience/retry.rs（删除）、src/vendor/resilience/mod.rs、src/utils/retry.rs、src/utils/mod.rs、src/config/mod.rs、src/router/router.rs、src/api/mod.rs、src/api/openai.rs、src/api/anthropic.rs、Cargo.toml、config.toml、tests/retry_test.rs、tests/fallback_test.rs、tests/config_toml_example_test.rs（新增）。
- **库映射**：`limiteron::retry::delay_for_attempt`；换路评估 `limiteron::fallback::FallbackManager` 不适用（ComponentType 是库内部组件枚举、构造需其自身 l2_cache，面向 limiteron 内部组件降级）→ 接线自研 FallbackChain 存量复活（非新写）。
- **测试**：delay_for_attempt 偏移回归；retry_delay_for 纯函数 4 用例（429+Retry-After→7s、429 无头→指数值、5xx→固定 1s 等，无 paused clock）；mockito 主 vendor 500×3 耗尽换路（X-Vendor 路由提示直达主 vendor 保证确定性）；单 vendor 失败 503 冒泡（VendorUnavailable→SERVICE_UNAVAILABLE 固定映射，与 #14 同口径）；build_fallback_chain 3 用例；config.toml 示例反序列化验证。
- **连带**：组合编译验证暴露既有缺陷（非本波引入）——sse/api/router/database/pool/cost/vendor_cost/assembly_smoke 等 10 个测试文件缺文件级 feature 门控（如 sse_test 引用 web-server 门控的 nexusflow::api 却无 `#![cfg]`），已补文件级 `#![cfg(feature = ...)]`，fallback_test 文件级改 `all(resilience, vendor-management)`。

**波 2 门禁**：check 通过（0 整改轮）、test exit 0；复审整改 3 处后 check exit 0、test exit 0。

### 第 3 波（must：#14、#3、#8；实施顺序 14→3→8）

#### recId 14 — 流式路径错误传播：上游状态码不再是 200 正文（must）

- **改动**：manager 新增 `check_stream_status`：execute_stream_request 与 execute_passthrough_stream_request send 后先校验状态——非 2xx 读 body 记 warn（对齐非流式既有写法 manager.rs:600-628）转 NexusFlowError，经 handler `?` 冒泡走 error.rs 固定映射，客户端与重试逻辑可分辨成败；中流错误：分帧层解析出上游 error 帧或流中断→产出语义化 Err，api 层转 `encode_sse_error` 后提前终止且不再 chain [DONE]（删除 openai.rs 旧"错误后仍以 [DONE] 收尾"行为）；两处流式错误分支补 `metrics.record_request(vendor,false,duration)`+`record_error`（此前流式零指标）。
- **文件**：src/vendor/manager.rs、src/api/openai.rs、src/api/anthropic.rs、tests/api_test.rs。
- **库映射**：reqwest 原生状态检查（七库无 HTTP 客户端封装，sdforge 为服务端框架，如实注明）；错误映射复用 error.rs 存量 IntoResponse 固定映射（RateLimited→429 error.rs:49、VendorUnavailable→503 error.rs:47），零新写。
- **测试**：api_test.rs 新增 streaming_error_propagation mod（mockito）：透传流 401→503 JSON 且 Content-Type 非 text/event-stream + metrics 递增；转换流 401→503（两路径同口径）；转换流 429→429 + metrics 递增。
- **偏差**：方案原文写"转 VendorUnavailable"，但 VendorUnavailable 固定映射 503 与测试②"上游 429→客户端收 429"冲突，以测试规约为准：429 转 RateLimited（映射 429）。可选项"缓冲启动事件使 200 后仍可重试"（Bifrost 做法）未实施（方案标注可选）。

#### recId 3 — 跨协议流式转换 + SSE 增量分帧（两阶段）（must）

- **阶段一（分帧器）**：新增 `src/protocol/sse_frame.rs` `SseFrameParser`——跨块 byte buffer、字节层扫描 `\n\n`/`\r\n\r\n`/`\n\r\n` 定界（0x0A/0x0D 均非合法 UTF-8 尾随字节，字节层切帧安全）、帧完整才解码（消除 U+FFFD 多字节劈裂）、逐行解析 data:/event:/注释行；`SseFrame` 保留帧原文 raw 供透传字节级保真（注释行结构化解析跳过、透传走 raw 不受影响）。透传两路径经 `reframe_passthrough_stream` 逐帧原样转发——删除 openai.rs 旧的 `from_utf8_lossy` 整块再包 `data:` 嵌套包装，anthropic.rs 旧实现连重帧都没有，一并接入。实现源头放 protocol/（vendor-management feature 不含 web-server，保 feature 矩阵），api/sse.rs 保持 re-export 兼容。
- **阶段二（接通 IR）**：`ProtocolConverter` 新增 `parse_stream_frame`（OpenAI 解析 chat.completion.chunk 的 delta/finish_reason/usage 尾帧；Anthropic 解析 content_block_delta/message_delta/ping）；`execute_stream_request` 返回类型改为 `impl Stream<Item=Result<StreamChunk, NexusFlowError>>`（bytes_stream→分帧→parse→IR，[DONE] 提前收流，流中断/上游 error 帧产出语义化 Err）；api 层消费 IR 流经存量 `stream_chunk_to_external`（此前零调用方）编码出站；Anthropic 出站新增 `AnthropicStreamEncoder` 状态机补齐 message_start/content_block_start/…_delta/…_stop/message_stop 生命周期。
- **文件**：src/protocol/sse_frame.rs（新增）、src/api/sse.rs、src/protocol/mod.rs、src/vendor/manager.rs、src/api/openai.rs、src/api/anthropic.rs、src/protocol/openai_converter.rs、src/protocol/anthropic_converter.rs、tests/sse_test.rs、tests/protocol_test.rs。
- **库映射**：分帧器与反向解析手写（已核实 sdforge::streaming 仅出站编码 stream_to_sse/create_stream_channel 与 StreamEvent 枚举，`streaming_impl.rs:54-98`，无上游 SSE 字节流解析能力）；协议映射复用自研 IR StreamChunk（protocol/unified.rs:276-292）与存量 stream_chunk_to_external——零新增等价机制。
- **测试**：sse_test.rs 33 行→24 测试（劈块出整帧/四字节 UTF-8 劈界无 U+FFFD/单块多事件/CRLF 及混合变体/flush 残余/error 帧检测/透传重帧保真+终止）；protocol_test.rs 阶段二新增 6 测试（OpenAI chunk→Anthropic 事件序列含 message_start/content_block_delta/message_stop；usage 尾帧→message_delta.output_tokens；tool_call delta 不丢弃；Anthropic 帧→IR；[DONE]/role-only 跳过；劈块端到端管线）。
- **首版限制**（如实声明）：Anthropic message_delta 的 delta.stop_reason 流式传递丢弃（IR Usage chunk 无 finish_reason 字段）；message_start 的 input_tokens 丢弃（转换流出站填 0）；AnthropicStreamEncoder 首版仅 text delta+usage+生命周期事件（方案 risk 段明示），工具流式后置。见 §5.4。

#### recId 8 — 协议断点修复：工具调用与 system 四处断裂（must）

- **改动**（TDD：先补 6 个失败测试跑 Red，66 存量全过确认基线，再改实现转 Green）：
  1. `parse_anthropic_message` 在聚合层处理（不改 ContentBlock 枚举，控制爆炸半径）：content 数组的 tool_use 块经新 helper `parse_tool_use_block` 聚入 ToolCalls/TextWithToolCalls（与 parse_response 既有聚合共用）；user 消息 tool_result 块经 `aggregate_anthropic_tool_result` 聚为 ToolResult 且 role 规范化为 Role::Tool；
  2. `parse_openai_message` String 分支后补查顶层 `tool_calls`→TextWithToolCalls，None|Null 分支与 String 分支共用提取出的 `parse_tool_calls_field` helper（Extract Function）；
  3. `parse_response` 补读 message.tool_calls→TextWithToolCalls/ToolCalls；
  4. system 双向：`to_unified` 读顶层 `body["system"]`（string 或 blocks 数组，此前 grep `get("system")` 全仓零命中）→首条 Role::System 消息；`request_to_external` 将 System 消息 split 出去提升为 Anthropic 顶层 system 参数（`extract_system_text`，多条以空行连接）；
  5. stop_reason↔finish_reason 静态映射表：tool_use↔tool_calls、max_tokens↔length、end_turn/stop_sequence→stop（反向取 end_turn），双向应用于 parse_response 与 response_to_external，未知值原样保留。
  - 配套修复：出站 `message_to_openai` 将 ToolCalls/TextWithToolCalls 输出为 OpenAI 规范的顶层 tool_calls 字段（此前塞 content 数组会被 OpenAI 上游拒收——测试①断言 assistant.tool_calls 的必然要求）。
- **文件**：src/protocol/anthropic_converter.rs、src/protocol/openai_converter.rs、tests/protocol_test.rs。
- **库映射**：手写（LLM 协议转换是业务域逻辑而非可下沉机制，七库均无此能力）；复用 unified.rs 存量 ToolCall/ToolResult/TextWithToolCalls 类型，零新增类型。
- **测试**：protocol_test.rs 60→72 全绿，新增 6 个形状断言（多轮工具历史往返保留 tool_calls/role:tool+tool_call_id；String+tool_calls→TextWithToolCalls；parse_response 提取 tool_calls；system 双向提升；stop_reason 双向映射）。
- **首版折衷**：同一 user 消息多个 tool_result 块合并为单个 ToolResult（首块 tool_use_id + content 换行拼接）；end_turn 与 stop_sequence 都归一为 stop、反向取 end_turn（静态映射表固有信息损失）。见 §5.4。

**波 3 门禁**：check 通过（0 整改轮）、test exit 0；复审整改 2 处后 check exit 0、test exit 0。另记录：diting 代码审查技能未调用（任务清单未点名，其大范围审查建议会越出"不改本波之外代码"约束）。

### 第 4 波（must：#4、#9；should：#10、#15；实施顺序 4→9→15→10）

#### recId 4 — queue_factor 接真实队列 + metrics 覆盖全部执行路径（must）

- **改动**：`get_queue_length`/`get_all_status` 改读 `instance.queue.len()`（async-channel bounded 直读 receiver.len()，queue.rs:39-41 现成方法）——原读的 QueueManager 全库无 insert 方，queue_factor 恒 1.0、3D 权重退化 2D 的根因即此；删除死件 QueueManager（结构+manager 字段+mod.rs re-export+vendor_test 用例）；`PendingRequest` 加 enqueued_at 传入 worker，完成分支成功/失败各记 `metrics.record_request`、失败另记 `record_error`（延迟口径=排队+执行全程）；两条流式路径在 send 成功（check_stream_status 后）按"派发处计"记成功，错误分支沿用既有记账（避开 Drop impl 的 panic 安全问题，口径已在文档注明）；handler 不重复计（注释锁定，防双计）。
- **RNG 注入改造**：`weighted_select` 拆出同步选择核心（rng 注入），原 `weighted_select` 变 `thread_rng` 薄 wrapper，生产行为不变。
- **文件**：src/vendor/queue.rs、src/vendor/mod.rs、src/vendor/manager.rs、src/router/router.rs、src/router/mod.rs、tests/vendor_test.rs、tests/router_test.rs。
- **库映射**：VendorQueue 本就是 async-channel bounded 队列，len() 直读无需迁移；业务 metrics 为项目内 GlobalMetrics——limiteron telemetry 经复核确由 `cfg(any(feature="telemetry",feature="monitoring"))` 门控（`../limiteron/src/lib.rs:149-150、267-270`）且面向限流可观测域，不适用。
- **测试**：构造 vendor 塞 N 条 pending 不取走→get_queue_length>0 且 compute_weight 相应下降；两 vendor 等权一忙一闲→`weighted_select_with` 注入固定 seed StdRng 断言 200 次选择分布偏向空闲侧（可复现）；mockito 成功/失败各一→snapshot().vendors 计数正确（透传与转换路径各一）。
- **偏差**：`weighted_select_with` 签名与方案不同——方案为 `(candidates, strategy, rng: &mut dyn RngCore)`，但 `&mut dyn RngCore` 非 Send 不能跨 await（编译器实测，axum handler future 报 !Send），且 calculate_weight 是 async——故拆 `pub calculate_weights`（async 收集权重）+ 同步选择核心两段式，RNG 注入语义达成。

#### recId 9 — 日志管道接线：AsyncLogger/PreAggregator 改造 + token 语义修复 + 失败落库（must）

- **改动**（先改造后接线）：
  - AsyncLogger：flush_buffer 逐条 await INSERT 改单事务批量（`Repository::log_requests_batch`，单条失败回滚整批）；`log()` 改同步 `try_send` + `dropped_logs` AtomicU64 计数（满即丢并计数，绝不阻塞响应路径；计数经 admin::metrics 暴露为 dropped_logs 字段）；`Err(_)=>break` 分支退出前补最终 flush（新增断开补写测试）；AsyncLoggerHandle 去掉每条一把的 tokio Mutex（改 Clone sender + AtomicBool，log 无锁直通）；
  - 接线：main.rs 构造 `AsyncLogger(1024, 1s)` 与 `TokenPreAggregator(60s)` 注入 ApiState 新字段（`logger`/`pre_aggregator: Option<_>`，未接线=降级不落日志）；flush worker 带停机位，serve 返回后 `logger.shutdown()` + 预聚合器最终 flush；
  - handler：log_chat_request/log_request 改 push 即返回（移出响应关键路径）；四个执行点（透传流/非流、转换流/非流）Err 分支在 `?` 早退前补记失败；
  - token 语义修复：prompt_tokens 从响应 usage 取（原实现填请求 max_tokens）；status_code 从实际结果取（上游状态码提取 helper + error.rs 固定映射兜底，原硬编码 200）；
  - PreAggregator：handler 拿到 usage 后 `record_raw`，flush worker 挂 main.rs 并带停机退出条件；
  - 大 payload 截断：messages/response >64KB 按 UTF-8 边界截断加标记。
- **文件**：src/database/repository.rs、src/database/async_logger.rs、src/database/pre_aggregator.rs、src/database/mod.rs、src/api/state.rs、src/api/mod.rs、src/api/openai.rs、src/api/anthropic.rs、src/api/admin.rs、src/main.rs、tests/database_test.rs、tests/api_test.rs。
- **库映射**：dbnexus 连接层事务——**与方案偏差**：方案写 `session.begin_transaction()`，实核实 dbnexus `Session::execute_raw` 只收裸 &str 无参数绑定、事务内走 `execute_unprepared`（session.rs:584/612）需手写 SQL 转义引入注入面，故改 `session.connection()?.begin()` 取参数化 DatabaseTransaction 循环 execute_raw 后 commit，语义等价"单事务批量+提交"且连接仍来自 dbnexus 池；背压复用现有 async-channel try_send。inklog 评估不适用（grep `../inklog/src` 零 request_logs/token_usage 命中——它是运行日志管线，业务数据表走 dbnexus，未迁移）。
- **测试**：plan tests ①②③（push 100 条 shutdown 后恰 100 条/容量 1 背压丢 2 条计数/channel 断开补写）以 AsyncLogger 内部单测覆盖（与 database_test 等价且更内聚）；database_test 补事务批量原子性；api_test：成功落库行 prompt_tokens==响应 usage.prompt_tokens 且 status_code==200；上游 500→落库行 error_message 非空且 status_code==500；token_stats 端点在 PreAggregator flush 后非空。

#### recId 15 — embeddings 端点回归供应商治理（should）

- **改动**：`build_vendor_url` 拆出 `build_vendor_url_at(config, path)`，新增 `execute_passthrough_request_at(vendor_id, body, path)`（向后兼容：`execute_passthrough_request` 变薄 wrapper）——embeddings 删每请求 `reqwest::Client::new()` 与硬编码 api.openai.com 兜底，改治理路径传 `"embeddings"`（现成获得共享连接池、RPS/并发限流、advanced limiter、metrics.record_request）；删除 `ApiState.client` 字段（grep 核实生产代码零使用；main.rs+api_test×4+retry_test+assembly_smoke_test 共 7 处构造同步更新）；删除 `embedding_response_from_openai` 死代码（mod.rs re-export + 本体 + protocol_test 两用例；`embedding_request_to_openai` 用例保留，该函数仍被使用）。
- **文件**：src/api/openai.rs、src/vendor/manager.rs、src/api/state.rs、src/main.rs、src/protocol/mod.rs、src/protocol/embedding_converter.rs、tests/api_test.rs、tests/protocol_test.rs、tests/retry_test.rs、tests/assembly_smoke_test.rs。
- **库映射**：复用 VendorManager 治理管道（limiteron TokenBucket/ConcurrencyLimiter/FixedWindowLimiter 在 execute_passthrough_request_at 内全部生效）；出站 HTTP 仍 reqwest（七库无 HTTP 客户端封装能力，sdforge 仅服务端，如实注明）。
- **测试**：embeddings 成功透传且 metrics.snapshot().vendors 含该 vendor requests==1（治理接线证明，旧实现不计 metrics）；配额耗尽后 5 并发全被拒且 mockito 断言零额外出站；上游 401→客户端 503（与 #14 同口径）。
- **实施中发现并当场修复的真实 bug**：wrapper 默认 path 曾用常量 `chat/completions`，会把 anthropic 透传打到错误 URL——被既有测试 retry_test::single_vendor_failure 捕获，改为按 vendor 协议分派（anthropic→messages）。
- **测试语义修正**：api_test 限流用例——配额耗尽后路由层把 Throttled vendor 排除（Validation 400）或 manager 层 429，两者均为治理生效形态，断言"非 200 + mockito 恰 1 次出站"。

#### recId 10 — 权重公式升级：系数可配置热更新 + EWMA 延迟/错误率第 4 维（should）

- **改动**：
  - `[routing]` 段进 Config：w_model/w_queue/w_window（默认 0.4/0.3/0.3 保持现行为）+ w_latency/w_error（默认 0）+ latency_ref_ms=1000 + temperature=1.0 + selection="weighted"；`RoutingSection::preset(quality/latency/cost)` 命名预设；config.toml 示例与示例测试断言锁定；
  - EWMA：VendorMetrics 加 ewma_latency_ms/ewma_error_rate（α=0.2，`ewma = α*x + (1-α)*ewma`，首样本播种；avg_duration_ms 保留兼容输出）；`GlobalMetrics::vendor_ewma` 供路由读取；
  - 权重公式加两因子：`latency_factor = ref/(ref+ewma_ms)`（有界 (0,1)，无数据中性 0.5）与 `error_factor = 1 - ewma_error_rate`（无数据 1.0 不惩罚冷启动）——默认权重 0，行为不变；
  - herding 缓解：temperature 平滑 `w^(1/T)`（T 非法按恒等，纯函数无 RNG）；`selection="power_of_two"` 实验开关：均匀采样 2 候选按 EWMA 延迟择优、平局回退平滑权重，经 #4 的 RNG 注入点实现，默认 weighted。
- **文件**：src/config/mod.rs、src/router/router.rs、src/utils/metrics.rs、config.toml、tests/router_test.rs、tests/config_toml_example_test.rs。
- **库映射**：[routing] 系数经 Router 每次路由 `self.config.read().routing` 热读（ConfigManager 底层即 confers DynamicField，update 后 read() 立即可见，复用 #13 地基，未另造热更新机制）；EWMA/温度平滑/Po2C 为统计算法手写（七库无路由权重计算能力，如实注明）。
- **测试**：RoutingSection 默认值结构断言 + w=0 时 EWMA 不影响权重的行为不变断言；EWMA 数值断言（100→200→400ms 逐次上升、α 语义）放 utils/metrics.rs 内部单测；error_factor/temperature 纯函数断言；Po2C 固定 seed StdRng 断言选择分布均衡（可复现）。
- **偏差**：①EWMA 存储用 per_vendor RwLock 写锁内 f64 而非方案的 AtomicU64（字段已在锁保护下，避免手工 Serialize/Clone impl，语义一致）；②latency_factor 的 ref 取 `[routing].latency_ref_ms` 配置（默认 1000ms、可热更）而非方案写"全局中位"——跨厂商中位数排序复杂度/收益比差，配置点等价；③EWMA 数值断言放 metrics 内部单测而非方案指定的 router_test（等价更内聚），router_test 补 EWMA 影响权重的集成断言。
- **默认行为不变有结构性保证**：新因子默认 0 权重、系数默认同旧值，测试锁定防线上路由突变。

**波 4 门禁**：check 通过（0 整改轮）、test exit 0；复审整改 2 处后 check exit 0、test exit 0。

### 第 5 波（should：#11、#12；could：#16；实施顺序 16→12→11）

#### recId 11 — 会话/前缀亲和路由（oxcache 亲和表 + TTL 活跃续期）（should）

- **改动**：新建 `src/router/affinity.rs` `AffinityTable`——键 `affinity:{session_key}`，TTL 1h，lookup 命中即 expire 续期（活跃会话粘住、闲置自动过期），存取失败静默降级；`Router::with_affinity` 接线，新增 `route_with_session(_excluding)`，try_abstract_route/try_global_route 在合格集合（enabled + get_status==Available）内亲和 boost——命中在集合内直接选中（hit）、绑定 vendor 被排除则 unbind+miss、无绑定 miss，**绝不强选**；只记"上次成功服务的 vendor"：API 层全部执行成功点（透传/转换×流式/非流式）bind，最终失败点 clear，resilience 换路成功后绑定为最终 decision.vendor；会话键：`X-Session-Id` header（扩展 RoutingHints 既有模式）优先，缺失兜底 `sha256(messages[0] 前 2KB)` 截 16 hex；Cargo.toml router feature 增加 dep:oxcache。
- **文件**：src/router/affinity.rs（新增）、src/router/mod.rs、src/router/router.rs、src/api/routing_hints.rs、src/api/state.rs、src/api/mod.rs、src/api/openai.rs、src/api/anthropic.rs、src/utils/metrics.rs、src/main.rs、Cargo.toml、tests/router_test.rs。
- **库映射**：`oxcache::backend::CacheBackend`（`CacheWriter::set` 带 TTL / `CacheReader::get` / `CacheWriter::expire`，interface.rs:140/217/231）+ `default_memory_backend`；亲和命中计数复用 GlobalMetrics（AtomicU64）。
- **测试**：tests/router_test.rs session_affinity mod 5 例全过：①同 session 连续 route 同 vendor（TTL 内）；②50ms TTL 过期允许漂移；③`update_status(vendor, Unavailable)` 制造排除→回落 vendor-b 且旧键清除+miss 计数；④无 header 同前缀 messages 哈希同 vendor；⑤hit_rate 从 0 递增且无 session_key 的路由不进分母。
- **偏差**：update_status 实测位于 manager.rs:1205（方案写 821-826），公开同步 API 语义一致照用。

#### recId 12 — 精确响应缓存落地 oxcache（租户隔离键 + 确定性请求门控）（should）

- **改动**：新建 `src/api/response_cache.rs` `ResponseCache`——键 `resp:{api_key_sha256}:{vendor_id}:{model_id}:{sha256(canonical_body)}`，canonical_body = `serde_json::to_value` 后递归按 key 排序再序列化（数组保序，不依赖 serde_json Map 内部实现，防键序差异错命中）；门控仅 `stream==false && tools 为空 && temperature==0/缺省` 的确定性请求查/写（有工具/带采样语义的请求永不缓存）；`X-No-Cache: 1` 跳过读写；`[cache]` 段 enabled（默认 false）/ttl_secs（默认 300）进 Config，ResponseCache 每次操作热读（POST /admin/config 即时生效，上线由运维显式打开）；main.rs 接线 oxcache memory 后端并打启用状态日志；openai/anthropic 四处非流式执行路径（透传+转换）接 check→HIT 提前返回（`X-Cache: HIT`）/Miss 成功后 store（`X-Cache: MISS`），Disabled 不带头；auth.rs `extract_api_key` 改 pub(crate) 供键组装复用（租户哈希与认证同源）。
- **文件**：src/api/response_cache.rs（新增）、src/api/state.rs、src/api/mod.rs、src/api/openai.rs、src/api/anthropic.rs、src/config/mod.rs、src/main.rs、src/middleware/auth.rs、config.toml、tests/api_test.rs。
- **库映射**：`oxcache::backend::CacheBackend`（get/set(key,value,Some(ttl))）+ default_memory_backend；绝不手写 HashMap 缓存；canonical JSON 排序为键规范化纯函数（业务算法）；sha256 与 #6 共用 sha2 依赖。
- **测试**：tests/api_test.rs response_cache_tests mod 6 例全过（mockito expect 计数锁定上游请求次数）：①相同 body 两次上游仅 1 次 + 第二次 HIT + body 一致；②temperature=0.7 两次回源；③带 tools 不缓存；④stream=true 不缓存且无 X-Cache 头；⑤X-No-Cache:1 强制回源且不写；⑥不同 API key 同 body 各自回源（租户隔离）。
- **偏差**：①方案与要点提示对跳过缓存头名不一致（X-No-Cache vs X-Skip-Cache），按方案正文实现 `X-No-Cache: 1` 为主、兼容 `X-Skip-Cache` 别名（测试断言两者均生效）；②X-Cache 头仅参与缓存的请求携带，Disabled（流式/带工具/采样）不带——测试②③④断言据此为"无头"；③语义缓存二期顺延（见 §5.1）。

#### recId 16 — GlobalMetrics 加 top-1 占比与流量份额熵（只做第②步）（could）

- **改动**：utils/metrics.rs 新增 `SlidingCounter`（60s 窗/10 桶环形，绝对桶序+惰性清零过期桶，读侧锚=全体 counter max(last_seen) 使停写 vendor 旧桶仍随全局时间衰减，时钟可注入供测试，时钟回退 clamp 防御）；`GlobalMetrics` 新增 traffic map 与 `traffic_shares()`/`top1_share()`/`share_entropy()`（Shannon H=-Σp·ln p 除以 ln V 归一到 [0,1]，V≤1 定义 0 防除零）及 `should_alert()`（top1_share>0.9 且合格 vendor>2）；`record_request` 顺带触碰当前桶；`MetricsSnapshot` 加 top1_share/share_entropy 字段——admin.rs 直接序列化 snapshot 故自动带出（**admin.rs 零 diff**，方案 files 列出但实测无需改动，已实跑验证）；snapshot 时 should_alert 为 true 则 tracing::warn（对应论文 When Routing Collapses 的路由崩溃失败模式告警钩子）；`reset()` 同步清 traffic。
- **文件**：src/utils/metrics.rs、src/api/admin.rs（实际零改动）。
- **库映射**：手写（Shannon 熵/top-1 占比/滑窗份额是业务统计算法，七库无此能力；limiteron telemetry/monitoring 的指标均为限流域而非业务流量分布，libMapping 已核实并注明）。
- **测试**：#[cfg(test)] 单测 6 例全过：①3 vendor 均分 entropy≈1.0/top1≈0.333；②单 vendor 独占 entropy==0/top1==1.0；③空窗 top1==0 且 should_alert()==false（无流量不算崩溃，语义锁定）；④top1=0.95 且 3 vendor 活跃→告警、2 vendor 同占比不告警；⑤前 30s 灌量+假时钟推进→旧桶衰减出窗（65s 后各半、130s 后全部出窗）；+1 例 affinity 计数。
- **顺延**：第①步（离线质量-成本前沿度量）与第③步（tier 判别路由）按方案限定不在范围（见 §5.1）。

**波 5 门禁**：check 通过（0 整改轮）、test exit 0；复审整改 2 处后 check exit 0、test exit 0。

---

## 3. 自研库迁移总账

### 3.1 实际迁移落地的能力（按库）

| 库 | 迁移能力 | 对应建议 | 说明 |
|----|---------|---------|------|
| **limiteron** (0.3.0-rc.5) | `circuit::{CircuitBreaker, CircuitBreakerConfig, ErrorClassifier}` + `error::CircuitState` | #2 | 手写三态机整文件删除（HalfOpen 死锁根除）；库公开面无 force API → force_circuit_open 一并删除，测试改自然路径驱动；dev-dep test-clock feature 的 with_clock 供测试注入 MockClock |
| | `limiters::{FixedWindowLimiter, Limiter}` | #5 | Token/Interval 窗口限流：惰性窗口重置 + CAS 原子消费 + peek 非消费预检 |
| | `retry::delay_for_attempt` | #7 | 重试延迟计算打底（features 追加 "retry"） |
| **confers** (0.6.0-rc.5，**本期新增依赖**) | `loader::load_file + LoaderConfig` | #13 | 带路径防护/大小限制的配置加载（allow_absolute 放行 CLI 可信输入） |
| | `dynamic::DynamicField<Config>` | #13、#10（复用）、#12（[cache] 热读） | 内存热读态，update 后 read() 立即可见 |
| **oxcache** (0.5.0-rc.5) | `backend::CacheBackend`（set 带 TTL / get / expire）+ `default_memory_backend` | #11（亲和表）、#12（响应缓存） | 全部缓存存储走 CacheBackend，**零手写缓存 map** |
| **dbnexus** (0.6.0-rc.5) | `session.connection()?.begin()` 参数化事务 → 批量 INSERT 后 commit | #9 | 请求日志单事务批量写（单条失败回滚整批） |
| **sdforge** (0.5.0-rc.5) | `http::build_with_config`（返回原生 axum::Router）+ `Router::merge` | #6 | 生产装配链修复的核心；`streaming` 的 `encode_sse_error` 用于流式错误事件（#14） |

**新增第三方依赖**（仅通用原语）：dashmap 6（#1，CostTracker 并发容器）、sha2 0.10 + hex 0.4（#6 认证摘要，#11/#12 键哈希复用）、confers（#13，自研库正式入列）。
**移除依赖**：backon（#7，两套重试栈合并为一）、arc-swap（#2，随手写熔断器删除，grep 复核全仓仅 circuit_breaker.rs 三处使用）。

### 3.2 评估后未采用的能力及原因（不迁移，逐条记录）

| 库能力 | 评估结论 | 原因 | 涉及建议 |
|--------|---------|------|---------|
| limiteron `fallback::FallbackManager` | 不适用 | ComponentType 是 limiteron 内部组件枚举、构造需其自身 l2_cache，面向库内部组件降级而非任意 vendor 链 → 接线自研 FallbackChain 存量复活（非新写） | #7 |
| limiteron `quota::QuotaController` | 不适用 | 面向 per-user keyed 配额且需 QuotaStorage 后端；vendor 级单键窗口计数用 FixedWindowLimiter 更轻、零存储依赖 | #5 |
| limiteron `retry::RetryPolicy::execute` | 部分不用 | 签名无 per-attempt 延迟注入点，429 的 Retry-After 分支需要自管薄循环调用 retry_delay_for；delay_for_attempt 仍采用 | #7 |
| limiteron telemetry/monitoring | 不适用 | 模块由 `cfg(any(feature="telemetry", feature="monitoring"))` 门控（lib.rs:149-150），Metrics/TelemetryConfig re-export 另由 cfg 门控（:267-270），均为限流可观测域指标而非业务流量分布 → 业务流量分布指标手写 | #4、#16 |
| confers `validator` | 不适用 | Validate 是 garde derive 再导出（impl_/validator.rs:31），ValidationRule 枚举仅字段级规则（MinLength/MaxLength/Range/Email 等，:42-62），无唯一性/跨条目规则 → vendor/model id 去重保留手写 validate_config | #13 |
| confers 持久化 `SnapshotManager` | 不适用（备份可选顺延） | 其 save/save_blocking 是快照式持久化（时间戳命名新文件 + prune，impl_/snapshot.rs:337），无 config.toml 原地原子替换语义 → 原地更新手写 tmp+rename；更新前留时间戳备份属 should 级增强，顺延 | #13 |
| confers ConfigValue serde 桥接 | 不可用 | ConfigValue 无 serde Deserializer 且 AnnotatedValue 序列化带 6 字段元数据壳 → 强类型桥接手写剥壳递归（annotated_to_json/config_value_to_json） | #13 |
| sdforge `AuthConfig::ApiKey` | 不适用 | 仅站点级明文 keys: Vec<String>，无摘要存储、无管理面/数据面分域；且 rc.5 对空 keys 是 build 时 fail-fast（http_impl.rs:389，实机证实）→ 认证保留自建中间件 + sha2 摘要 | #6 |
| sdforge `streaming`（上游解析） | 能力不存在 | 仅提供出站编码 stream_to_sse/create_stream_channel（streaming_impl.rs:54-98），无上游 SSE 字节流解析/分帧能力 → 分帧器手写 | #3 |
| oxcache `DashMapMemoryBackend` | 不适用（#1 评估） | 是带 TTL 语义的缓存后端封装（get/set 带 TTL），非裸并发 map 的能力暴露，不适合当 CostTracker 容器 → 维持第三方 dashmap | #1 |
| inklog | 不适用 | 是运行日志管线（LoggerManager/LogRecord/sink），request_logs/token_usage 是业务数据表 → 走 dbnexus；grep `../inklog/src` 零命中 | #9 |
| limiteron `with_clock`（生产路径） | 仅测试用 | 生产用库默认时钟；测试经 dev-dep test-clock feature 注入 MockClock（#2/#5/#16） | #2/#5/#16 |

**trait-kit (0.5.0-rc.6)**：本期 16 条未涉及新增迁移，DI 容器维持现状；#11 方案中"经 trait-kit kit require::<OxcacheModule>() 取共享实例"的路径未采用，实施取 `default_memory_backend` 直构注入（波 5 记录）。

### 3.3 手写保留域（均已在代码/记录中注明原因）

| 手写项 | 原因 | 涉及 |
|--------|------|------|
| NexusFlowError↔LimiteronError 双向映射（src/vendor/resilience/error_map.rs） | 库 execute/ErrorClassifier 签名锁定 LimiteronError；非等价手写熔断逻辑；有损降级以测试锁定 | #2 |
| SSE 分帧器与反向解析（src/protocol/sse_frame.rs） | sdforge::streaming 无上游解析能力 | #3 |
| TimeWindow 每日时段窗口 | limiteron 无"时段"窗口原语（fixed/quota 均为时长窗口） | #5 |
| 流式 permit 生命周期（futures::stream::unfold 持有） | sdforge 流式封装工作在 HTTP 层，vendor 层 permit 无库能力可委托 | #5 |
| retry_delay_for + Retry-After 解析（src/utils/retry.rs） | RetryPolicy::execute 无 per-attempt 延迟注入点；delay_for_attempt 为库文档明示的自管循环配套 | #7 |
| SHA256 摘要存储 + 常量时间比较 | 七库无 API key 摘要存储/常量时间比较能力（sdforge security 是 RBAC 域） | #6 |
| tmp+rename 原子写 | confers 仅快照式持久化，无原地替换语义 | #13 |
| confers 强类型桥接（剥壳递归） | ConfigValue 无 serde Deserializer | #13 |
| EWMA/温度平滑/Po2C/Shannon 熵/SlidingCounter | 业务统计算法，七库无路由权重计算与流量分布统计能力 | #10、#16 |
| LLM 协议转换（工具调用/system/stop_reason/流式 IR） | 业务域逻辑而非可下沉机制 | #8、#3 |
| canonical JSON 键排序 | 键规范化纯函数（业务算法，非缓存机制） | #12 |

---

## 4. 门禁与验证

### 4.1 各波门禁记录（实施会话）

| 波次 | 建议 | 实施轮 check | 实施轮 test | 复审整改后 | 复审整改处数 |
|------|------|------------|------------|-----------|------------|
| 1 | #1 #6 #13 | 通过（0 整改轮） | 退出码 0 | check 0、test 101* | 2 |
| 2 | #2 #5 #7 | 通过（0 整改轮） | 退出码 0 | check 0、test 0 | 3 |
| 3 | #3 #14 #8 | 通过（0 整改轮） | 退出码 0 | check 0、test 0 | 2 |
| 4 | #4 #10 #9 #15 | 通过（0 整改轮） | 退出码 0 | check 0、test 0 | 2 |
| 5 | #11 #12 #16 | 通过（0 整改轮） | 退出码 0 | check 0、test 0 | 2 |

\* 波 1 复审整改后 test 退出码 101 系门禁记录原文，材料未附失败明细；其后波 2–5 各轮全量 test 均为退出码 0，且本会话收尾复核全量 463 过 0 挂（见 4.2），该异常未在最终态复现。

实施会话未单列 clippy 运行记录；实施期门禁声明为 `cargo test --all-features` 与 `cargo clippy --all-features -- -D warnings`（libStrategy 基线核实段），clippy 首手结果由本会话复核补跑（下节）。

### 4.2 收尾复核（本报告撰写会话实跑，首手结果）

| 命令 | 退出码 | 耗时 | 关键输出 |
|------|-------|------|---------|
| `cargo check --tests --all-features` | **0** | 10.15s | Finished dev profile；grep "^warning" 仅 1 条——第三方 `proc-macro-error2 v2.0.1` 的 future-incompat 提示，**非本项目代码告警** |
| `cargo clippy --all-features -- -D warnings` | **0** | 19.36s | 0 代码告警，符合项目生产标准（AGENTS.md：0 clippy 告警） |
| `cargo test --all-features` | **0** | 最长单目标 31.06s | 20 个测试目标（含 doc-tests）全部 ok：**463 passed / 0 failed / 0 ignored** |

（对比：波 4 实施时全量约 430 测试，波 5 新增亲和/响应缓存/熵监控等用例后增至 463，口径一致。）

### 4.3 验证方法说明与限制

- **GitNexus 不可用**：AGENTS.md 要求的 `gitnexus_impact`/`gitnexus_detect_changes` MCP 工具在本环境工具集中不存在，且项目非 git 仓库（禁 git 命令）——影响分析以直接读源码 + 全量 grep 调用点替代（实施会话记录：get_status 5 处、get_window_info 9 处、advanced_limiter.check 5 处、VendorConfig/Config/ConcurrencyConfig 字面量构造点 17/14/15 处均已逐一更新并验证），并以 12 feature 组合编译 + 全量测试兜底（波 2 记录⑨、波 4 记录(8)）。
- **实机验证为 debug 构建**：`cargo run` + curl 行为矩阵在 debug 下完成；release 因项目开 lto+codegen-units=1 全量重编译耗时过长未跑（行为矩阵与优化级别无关，见 §5.1）。
- **测试口径统一**（实施期约定并执行）：①上游单一失败的状态码期望统一为 503（error.rs:50-51 VendorUnavailable→SERVICE_UNAVAILABLE 固定映射；#14 的 429 特例转 RateLimited 映射 429）；②涉及随机性的路由分布断言一律经 `weighted_select_with` RNG 注入点用 StdRng 固定 seed，不依赖 thread_rng；③mockito 集成测试一律真实时钟、不用 tokio::time::pause（在库先例 tests/vendor_circuit_breaker_test.rs:9-14 书面记录的 paused-clock 与真实 HTTP IO 冲突），延迟类断言下沉到纯函数单测。

---

## 5. 顺延清单与理由、遗留风险与建议后续

### 5.1 计划内顺延（方案与波次记录明确声明）

| 项 | 内容 | 理由 |
|----|------|------|
| #13 可选增强 | 更新前调 confers SnapshotManager::save_blocking 留时间戳备份 | confers 快照是独立时间戳文件（非原地替换），与 tmp+rename 主路径正交；本波聚焦 must 级三项，备份属 should 级增强 |
| #6 release 实机验证 | release 构建的 curl 可达性矩阵 | debug 已验证完整行为矩阵；release 开 lto+codegen-units=1 全量重编译耗时过长未跑。行为与优化级别无关，如需可在后续会话补跑 |
| #12 语义缓存二期 | embedding 语义缓存选型与阈值 | 需 #9 修复后的 SQLite 请求日志数据按 P-CHR/ORR 离线回放决定（优化报告 P2-1 引证 arXiv:2606.19719），依赖运行期数据；本期只落精确缓存 |
| #16 第①③步 | ①离线质量-成本前沿度量、③tier 判别路由 | ①依赖 #9 数据底座积累真实流量；③的收益需①的工作点度量验证（RouterBench 方法论），强行先上属无数据支撑的提前优化。第②步（熵监控）已按方案落地 |

### 5.2 跨波次约定项未随波次交付

- **#5⑥ 流式 token 记账**：方案声明"接口在本条留钩子，实现排在第 3 波 recId=3 之后"。波 2 已留注释钩子，但波 3 复审记录明确"流式 token 记账钩子（consume_tokens on usage 尾帧）维持 manager.rs 既有'预留'注释，非本波任务"——**该项至今未实施**。现状影响：流式请求的 limiter 记账不含响应 token（仅请求侧预扣）。建议随下一轮落地：SSE 分帧与 usage 尾帧解析（parse_stream_frame）均已就位，实现面已打通。

### 5.3 实施中发现的既有问题（不在本范围，未修）

- **SQLite 文件不自建**：sqlx 默认不创建新 SQLite 文件，实机启动需在 database.path 加 `?mode=rwc`；AGENTS.md 所述"数据库文件自动创建于 ./data/llm_proxy.db"实测不成立（波 1 deferred 记录）。属 database 层既有问题，建议修复并同步修订 AGENTS.md。
- **（已修复）**：波 2 发现 10 个测试文件缺文件级 feature 门控（阻塞组合编译验证），已随波 2 补齐——列入此处仅为记录其"既有缺陷"属性，非遗留。

### 5.4 首版技术限制（如实声明，后续回归项）

| 项 | 限制 | 出处 |
|----|------|------|
| Anthropic 流式 stop_reason | message_delta 的 delta.stop_reason 流式传递丢弃（IR Usage chunk 无 finish_reason 字段） | #3 波 3 记录 |
| Anthropic 流式 input_tokens | message_start 的 input_tokens 丢弃，转换流出站填 0 | #3 |
| 工具流式 | AnthropicStreamEncoder 首版仅 text delta + usage + 生命周期事件，工具流式不支持 | #3（方案 risk 段明示） |
| 多 tool_result 合并 | 同一 user 消息多个 tool_result 块合并为单个 ToolResult（首块 tool_use_id + content 换行拼接） | #8 |
| stop_reason 静态映射信息损失 | end_turn 与 stop_sequence 都归一为 stop、反向取 end_turn | #8 |
| 流式 200 后不可重试 | "缓冲启动事件使 200 后仍可重试"（Bifrost 做法）可选优化未实施 | #14 |

### 5.5 遗留风险（已按方案声明并缓释）

1. **映射层有损**（#2）：LimiteronError 串编码解析失败时降级 VendorUnavailable——降级行为已用测试锁定；CB Open→VendorState 回写存在瞬态不一致窗口，下一请求执行即校正（方案已声明可接受）。
2. **响应缓存错命中面**（#12）：canonical JSON 与客户端数字精度语义差异可能错命中——键含 vendor+model+完整 canonical body 且仅 temperature=0 场景启用，风险面已收窄；TTL 内响应陈旧是产品语义，[cache] 默认关闭。
3. **亲和与负载均衡冲突**（#11）：Astrolabe herding 证据——boost 方案保底合格集合过滤、绝不强选；#16 熵监控告警兜底路由收敛。
4. **静默降级路径**（#9）：ApiState 的 logger/pre_aggregator 为 Option，未接线=不落日志不告警；main.rs 已接线，但该 Option 语义使装配遗漏无显式失败信号。

### 5.6 建议后续

1. 落地 #5⑥ 流式 token 记账（consume_tokens on usage 尾帧，分帧器已就位）。
2. 修复 SQLite 建库（`?mode=rwc` 或启动时显式建库建目录）并修订 AGENTS.md 相应描述。
3. 补 release 实机验证矩阵（可选：docker 冒烟，AGENTS.md 宣称的多阶段构建）。
4. 工具流式 + Anthropic 流式 stop_reason/input_tokens 传递（需 IR StreamChunk 扩展 finish_reason/input_tokens 字段），与 #8 多 tool_result 合并策略一并回归。
5. CI 固化全量门禁：`cargo clippy --all-features -- -D warnings` + `cargo test --all-features` + 12 组合 feature matrix 编译进 GitHub Actions（AGENTS.md 宣称 CI 已配置，建议核实流水线是否覆盖全量口径）。
6. #16 告警钩子当前为 tracing::warn，建议接入实际告警通道；数据积累后启动 #16 第①③步与 #12 语义缓存选型（离线回放）。
7. 环境治理：GitNexus MCP 工具补装，或修订 AGENTS.md 的 GitNexus 强制条款（当前版本该要求在本环境不可执行）。

---

*报告完。所有"本会话核实/实跑"条目均出自本报告撰写会话；其余实施细节出自各波次实施与复审记录。*
