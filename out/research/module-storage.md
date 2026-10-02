# NexusFlow「存储与统计」模块调研笔记

> 调研日期：2026-09-30
> 范围：`src/database/`（mod.rs, database.rs, async_logger.rs, pre_aggregator.rs, repository.rs, schema.sql）+ `src/utils/`（mod.rs, metrics.rs, retry.rs, cost_tracker.rs）
> 方法：通读范围内全部源码 + 交叉核查调用图（grep 全仓）、feature 门控（Cargo.toml / Cargo.lock）、API 暴露端点（api/router_builder.rs）。
> **验证声明**：本环境中兄弟库路径依赖（`../dbnexus`、`../sdforge` 等）不存在，`cargo metadata` 失败（`failed to load source for dependency 'dbnexus'`），因此**未能运行 cargo build / cargo test**，所有编译/运行类结论均为静态分析。dbnexus 的 DbPool 内部实现（WAL/pragma/连接池行为）不可查，相关结论标注为"不可验证"。

---

## 1. 模块职责

| 组件 | 文件 | 职责（声明） | 实际状态 |
|------|------|--------------|----------|
| `Database` | `src/database/database.rs` | 打开 SQLite、建池、初始化 schema | ✅ 生产使用（`main.rs:38`） |
| `Repository` | `src/database/repository.rs` | request_logs/token_usage/vendor_status 的参数化 SQL 读写 | ✅ 生产使用（API 层直接调用） |
| `AsyncLogger` | `src/database/async_logger.rs` | 异步批量日志写入（channel + 缓冲 + 双触发 flush） | ❌ **死代码，零外部调用者** |
| `TokenPreAggregator` | `src/database/pre_aggregator.rs` | 按小时预聚合 Token 使用 | ❌ **死代码，零外部调用者** |
| `GlobalMetrics` | `src/utils/metrics.rs` | 原子计数 + 按厂商分桶的请求/延迟/错误指标 | ⚠️ 半接线：仅透传路径记录，经 `/admin/metrics` 暴露 |
| `CostTracker` | `src/utils/cost_tracker.rs` | micro-USD 定点累加的成本追踪 | ❌ **任何配置都无法编译**（feature 未声明 + 依赖缺失） |
| `retry` | `src/utils/retry.rs` | 错误分类/重试策略/回退计算 | ⚠️ 分类被生产复用；`with_retry` 本体无生产调用者 |
| `vendor/resilience` | `src/vendor/resilience/` | CircuitBreaker/FallbackChain/backon 重试 | ❌ **任何配置都无法编译**（对照为参考） |

数据模型（`src/database/schema.sql`）：
- `request_logs`：请求明细日志，`request_id UNIQUE`，4 个索引（timestamp / actual_vendor / abstract_model / request_id，schema.sql:33-37）
- `token_usage`：`(vendor, model, date, hour)` UNIQUE（schema.sql:22），预聚合目标表
- `vendor_status`：主键 vendor（schema.sql:26-32）— **无人写入**
- `model_stats`：`(model, vendor, date)` UNIQUE（schema.sql:41-49）— **无人写入，也无读取代码**

---

## 2. 数据流（实际运行时）

### 2.1 唯一真实写库路径：API handler 同步单条 INSERT

```
客户端请求 → api/openai.rs:chat_completions / api/anthropic.rs:messages
  → 上游调用成功（所有错误经 `?` 提前返回，不落库）
  → handler 内联日志构造：
      messages  = serde_json::to_string(request.messages)   (openai.rs:199-202)
      response  = serde_json::to_string(response)           (openai.rs:203)
      status_code = 硬编码 Some(200)                         (openai.rs:222)
      prompt_tokens = 请求的 max_tokens                      (openai.rs:225-228)  ← 语义错误
      completion_tokens / total_tokens = None
      cost_usd = None
  → repository().log_request(&log).await                   (openai.rs:205-234)
      → 每次调用 pool.get_session("admin")                  (repository.rs:110)
      → 单条参数化 INSERT，无事务/无批量                    (repository.rs:78-114)
  → 失败静默吞掉（`let _ =`，openai.rs:205；anthropic.rs:175）
  → 返回客户端响应（即 DB 写在响应关键路径上，openai.rs:97-108）
```

流式路径：在**流建立时**（不是流结束时）记日志，`duration_ms` 是到流建立为止的耗时，response 记为 `{"stream": true}`（openai.rs:67-77, 142-152；anthropic.rs:60-75, 108-124）。

### 2.2 查询路径（admin 端点 → Repository）

| 端点 | handler | 查询 | 备注 |
|------|---------|------|------|
| `GET /admin/logs` | `admin.rs:25-39` | `query_logs`（repository.rs:116-148） | `end_time` 参数被解析但 SQL 从不使用（见问题 #6） |
| `GET /admin/logs/search` | `admin.rs:42-55` | `search_logs`（repository.rs:150-175） | 关键词取自 `vendor` 查询参数（admin.rs:46），LIKE 未转义 |
| `GET /admin/stats/tokens` | `admin.rs:58-65` | `get_token_stats`（repository.rs:177-199） | 读 `token_usage`，**该表无人写入，永远返回 []** |
| `GET /admin/metrics` | `admin.rs:69-72` | `GlobalMetrics::snapshot()`（metrics.rs:81-107） | 路由在 router_builder.rs:53 |
| `GET /admin/vendors` | `admin.rs:75-90` | 内存状态（非 vendor_status 表） | |

### 2.3 设计上存在但未接线的路径

```
[未接线] handler ──x──> AsyncLogger.log() ──> bounded channel
                          └─> writer_loop（tokio::select!）
                                ├─ interval.tick() → flush
                                ├─ buffer.len() ≥ 100 → flush
                                └─ Shutdown → flush + break
                          flush_buffer → 逐条 repository.log_request

[未接线] vendor 队列路径响应 usage ──x──> TokenPreAggregator.record()
                          └─> Mutex<HashMap<Key,Val>> 累加
                          └─> start_flush_worker（interval）→ drain
                          └─> INSERT … ON CONFLICT DO UPDATE 增量合并
```

证据：`grep -rn "AsyncLogger|AsyncLoggerHandle" src tests` 仅命中 `src/database/async_logger.rs` 自身；`grep -rn "TokenPreAggregator|record_raw|start_flush_worker" src tests` 仅命中 `pre_aggregator.rs` 与 `mod.rs` 的模块声明。`main.rs` 从未构造二者。

### 2.4 指标与成本

- **GlobalMetrics**：仅 `execute_passthrough_request` 记录（manager.rs:697-703）；队列路径 `execute_request → process_vendor_queue`（manager.rs:592-624, 263-336）**不记录** → 非 OpenAI vendor 的转换路径请求、全部流式请求在 `/admin/metrics` 中不可见。
- **CostTracker**（默认不编译，见问题 #4）：恰好相反，只在队列路径记账（manager.rs:314-327），透传路径不计。成本数据无任何出口：`/admin/cost` 路由不存在（router_builder.rs:50-70 无此路由），`cost_tracker()` 访问器（manager.rs:166-168）零调用者。

---

## 3. 关键算法与数据结构

### 3.1 AsyncLogger：有界 channel + 双触发批量写（未接线）
- `async_channel::bounded(channel_capacity)`（async_logger.rs:36）；命令枚举 `Write(RequestLog) | Shutdown`（:17-22）
- 触发条件两个（async_logger.rs:76-100）：
  1. `tokio::time::interval(flush_interval)` tick（:77-80）
  2. 缓冲区 `buffer.len() >= 100`（:86-88，**硬编码**，与 channel_capacity 无关）
- 背压：`send().await` 在 channel 满时**阻塞调用方**（:48-54）→ 若接入，等于把数据库压力反压回请求路径
- 丢失风险：
  - 进程崩溃/kill 时 buffer 内条目丢失（无 fsync/无 WAL 保证前提不可验证）
  - flush 失败仅 `tracing::error!` 后**丢弃**（:112-116），无重试、无死信
  - `Err(_) => break`（:95-98）：所有 sender drop 后直接退出，剩余 buffer 由 Drop 前 flush?——不，break 后直接结束循环，**buffer 未 flush**（仅 Shutdown 命令路径会 flush，:90-94）
- `AsyncLoggerHandle` 每次写日志要过一把 `tokio::sync::Mutex`（:135-144）→ 串行化所有调用方的锁获取（虽然 channel send 本身并发安全）
- 批量写名不副实：`flush_buffer` 是**逐条 await INSERT**（:112-116），无事务包裹，批量只省了 task 切换、没省 SQLite 写

### 3.2 TokenPreAggregator：内存 HashMap 聚合 + UPSERT 增量合并（未接线）
- 聚合粒度：`(vendor, model, date, hour)`，时间取 `chrono::Utc::now()`（pre_aggregator.rs:18-36）→ 每厂商×模型每天最多 24 行
- 热路径：`record`/`record_raw` 对 `Arc<Mutex<HashMap>>` 加 **tokio Mutex** 后累加（:66-97）
- 刷库：`flush()` 在锁内 `drain()` 取走全部条目（:112-116），逐条执行
  `INSERT … ON CONFLICT(vendor, model, date, hour) DO UPDATE SET x = x + excluded.x`（:122-132）——幂等增量合并，这是全模块设计最好的一段
- 每 entry 独立 `get_session`（:153），无事务；flush worker `loop { interval.tick() }` 无退出条件（:100-109）
- 查询侧：`get_token_stats` 无时间过滤、无 LIMIT（repository.rs:181-191），行数随时间线性增长

### 3.3 GlobalMetrics：原子计数器 + RwLock 分桶
- 全局四计数器 `AtomicU64`（Relaxed）：total / success / failed / total_duration_ns（metrics.rs:15-21）
- `record_request`：4 次 `fetch_add` + **std::sync::RwLock 写锁**内更新 per-vendor 结构体（metrics.rs:35-58）——文档注释宣称"数据通过原子操作写入"（:7），per-vendor 实际是锁 + 非原子 u64 字段
- `vendor.to_string()` 每次调用分配（:47, :64）；`last_request_at`/`last_error` 在锁内取系统时间/拷贝字符串（:56, :66）
- `snapshot()`：多次 Relaxed load 非同一时刻（total/success/failed/total_ns 各自独立读，:82-85），加读锁克隆全部 VendorMetrics（:93-97）→ 数值可能互相不一致（对统计可接受）
- 无分位数/直方图/滑动窗口，只有全局均值 avg_duration_ms（:87-91）
- 暴露：`GET /admin/metrics`（router_builder.rs:53 → admin.rs:69-72），序列化 `MetricsSnapshot`（metrics.rs:167-175）

### 3.4 CostTracker：micro-USD 定点累加（不可编译）
- `u64` 存 USD×1e6，避免浮点累加误差；`(cost * 1e6).round() as u64` 处理 f64 表示误差（cost_tracker.rs:17, 49-57）
- `DashMap<String, AtomicU64>` 按 vendor/model 分桶（:28-32）；`record(≤0.0)` no-op（:50-52）
- `snapshot()` 遍历 DashMap 克隆（:95-115）

### 3.5 重试：一套分类、三套执行（生产只用内联版）
- **分类**（唯一被生产复用的部分）：`ErrorCategory::from_status_code`（retry.rs:40-51）：400/401/403/404→Fatal；429→RetryableExponential；500/502/503→RetryableFixed；其余→Fatal。`from_error_message` 按字符串匹配 timeout/connection refused/reset（:54-65）
- **策略**：`RetryStrategy::for_category`（retry.rs:92-114）、`BackoffStrategy::delay`（retry.rs:130-139，指数 base_ms×2^attempt 封顶 max_ms）
- **执行 A（utils 手写 with_retry，cfg(not(resilience))）**：retry.rs:180-237，签名 `(f, initial_error)`，按初始错误定策略，中途错误需重新过 `is_retryable_error` 检查（:219）
- **执行 B（vendor/resilience backon 版，cfg(resilience)）**：`ExponentialBuilder::default().with_jitter().with_max_times(3)`，**默认重试一切错误**（resilience/retry.rs:33-41）；resilience 启用时 utils/retry.rs:19-20 直接 re-export 它
- **执行 C（实际生产路径）**：`execute_http_with_retry_inner` 内联 loop（manager.rs:474-561）：网络错误重试 2 次（500ms×n，:493-504）；状态码错误用 `ErrorCategory::from_status_code`（:523）分类后重试，指数延迟内联计算 `1000 * 2u64.pow(retry_count.min(4))`（:530-533）——**先 `retry_count += 1` 再计算**（:526-527），导致 429 首次重试延迟为 2s 而非文档/单测声明的 1s（对照 retry.rs:130-139 与 retry 测试 `delay(0)==1000ms`，tests 侧 retry_test.rs 注释明确 1s,2s,4s,8s,16s）
- **交汇点**：resilience 启用时 CircuitBreaker 包装执行 C（manager.rs:445-465）；`NexusFlowError` 的 `IsRetryable` 委托回 `utils::retry::is_retryable_error`（error.rs:119-125）；fallback.rs 复用 `utils::retry::extract_status_code`（fallback.rs:9）
- **死代码**：`FallbackChain` 零调用者（grep 全仓仅自身与 mod.rs）；`with_retry` 两个版本均零生产调用者（grep 仅定义/测试）

### 3.6 CircuitBreaker（不可编译，参考）
- 三态 Closed/Open/HalfOpen，`ArcSwap<State>` 无锁转换 + AtomicU64 计数（circuit_breaker.rs:82-98）
- HalfOpen 探测限流 `half_open_calls.fetch_add`（:131-135）；成功 N 次闭合、可重试失败即开（:138-152, 156-173）
- 配置默认 5 失败开 / 3 成功闭 / 30s 超时 / 半开 1 并发（:62-71）

### 3.7 SQLite 配置
- 连接：`DbConfig { url, max_connections: pool_size, admin_role: "admin" }`（database.rs:26-31）；pool_size 默认 10（config/mod.rs:39-41），config.toml 实配 5
- 索引：request_logs 上 timestamp / actual_vendor / abstract_model / request_id 四索引（schema.sql:33-37）；token_usage 与 model_stats 靠 UNIQUE 约束支撑 UPSERT
- **WAL / busy_timeout / synchronous 等 pragma：本仓库零配置**（grep `journal_mode|wal|synchronous|busy_timeout` 无命中）；dbnexus 侧默认值不可验证（源码不在环境中）
- main.rs:55-63 在 DI 演示块里**另建第二个 DbPool**（`min_connections: 1`）连同一个 SQLite 文件——与主池并存，各自独立连接数预算
- schema 以 `split(';')` 逐句执行（database.rs:47-56），语句内含分号即碎裂（当前 schema 无触发器，尚安全）
- `Database::close()` 为空 no-op（database.rs:69-71）；优雅停机只停 HTTP（main.rs:135-137, 154-176），无 flush 钩子

---

## 4. 问题与优化切入点（按影响排序）

### P0 — 宣称能力与实际不符 / 数据完整性

**#1 「异步批量日志」与「Token 预聚合」均为死代码，未接入请求路径**
- `AsyncLogger`/`AsyncLoggerHandle` 零外部调用者；`TokenPreAggregator` 零外部调用者（grep src+tests 仅自身文件与 mod.rs 声明）；`main.rs` 不构造二者
- 实际路径是**每请求一次同步单条 INSERT**，且在响应关键路径上（openai.rs:97-108, 205-234）
- 后果：`/admin/stats/tokens` 永远返回 `[]`（token_usage 无人写，admin.rs:63 → repository.rs:177-199）；AGENTS.md「异步日志批量写入 / Token 使用预聚合」两条 PRD 改进项实际未生效
- 优化切入点：在 `main.rs` 构造 AsyncLogger/PreAggregator 并注入 `ApiState`，把 handler 的 `log_request` 换成 `AsyncLogger::log`（channel send 是微秒级）；flush_buffer 改为单事务批量 INSERT

**#2 失败请求完全不落库**
- 日志构造只在所有 `?` 错误传播之后（openai.rs:176-186、anthropic.rs:144-152 均在成功分支）；`error_message` 字段永远是 None（openai.rs:224）
- 后果：request_logs 无法回答"哪些请求失败了、为什么失败"——日志表只记成功，错误观测全靠 tracing
- 优化切入点：在错误分支（IntoResponse 之前）补记 `status_code`/`error_message`；或将日志下沉到 middleware/tower layer 统一捕获

**#3 Token/成本明细语义错误**
- `prompt_tokens` 填的是**请求的 max_tokens**（openai.rs:225-228；anthropic.rs:196-199 同样代码），`completion_tokens`/`total_tokens` 恒 None，`cost_usd` 恒 None
- 而真实 usage 在 vendor 层拿得到：manager.rs:307-310 用 `response.usage.total_tokens` 喂限流器、:316-321 喂成本计算——**唯独不持久化**
- 后果：request_logs 的 token 列不可用于计费/审计
- 优化切入点：日志记录下沉到 `process_vendor_queue`/`perform_http_request` 拿到 UnifiedResponse.usage 的位置，或 handler 从 response.usage 提取

**#4 CostTracker 与 vendor/resilience 在任何配置下都无法编译**
- `cost-tracking`、`resilience` 两个 feature 在 Cargo.toml `[features]` 中**未声明**（Cargo.toml:15-25 只有 8+full）；`dashmap`、`backon`、`arc-swap` 不在 `[dependencies]`（虽出现在 Cargo.lock:166,626,1146，但都是传递依赖，2021 edition 不可直接 import）
- 后果：`#[cfg(feature="cost-tracking")]`/`#[cfg(feature="resilience")]` 恒 false → cost_tracker.rs、vendor/resilience/ 全体、manager 的熔断包装与成本记账（manager.rs:83-86, 104-105, 314-327）从不参与编译；AGENTS.md 宣称的"熔断器""成本统计"实际不在产物里
- 连带：`tests/circuit_breaker_test.rs`、`tests/vendor_circuit_breaker_test.rs`、`tests/cost_tracker_test.rs` 无 cfg 门控却引用这些模块 → `cargo test`（默认 features）应当编译失败（未能运行验证，静态推断）
- 优化切入点：声明 feature + 补 `[dev-dependencies|dependencies]`（backon/arc-swap/dashmap），或删除死模块

**#5 Metrics 只覆盖零拷贝透传路径**
- `metrics.record_request` 唯一调用点是 `execute_passthrough_request`（manager.rs:697-703）；队列路径（manager.rs:592-624）与其 worker（:263-336）不记录
- 后果：Anthropic 入口、跨协议转换请求、所有流式请求不计入 `/admin/metrics`——指标严重低估；路由的 queue_factor 也拿不到这批请求
- 优化切入点：把记录移到 `PendingRequest` 完成处（manager.rs:330 `response_tx.send` 附近），一处覆盖所有队列路径；流式在流结束时记录

### P1 — 查询/迁移正确性

**#6 `query_logs` 忽略 `end_time`**
- `LogFilter.end_time` 有定义（mod.rs:69）、admin.rs:30 有解析，但 SQL 构造（repository.rs:116-140）只拼 `start_time/vendor/model`——`grep end_time repository.rs` 零命中
- 后果：`/admin/logs?end_time=…` 静默无效
- 优化切入点：repository.rs:120 后补 `if let Some(end) … AND timestamp <= ?`

**#7 v0.1→v0.2 列迁移不在生产路径**
- `ensure_column_exists`（repository.rs:43-76，PRAGMA 检查 + ALTER TABLE）只在 `Repository::initialize`（:33-36）里调用，而 initialize 仅被测试调用（tests/database_test.rs 多处）；生产 `main.rs:38` 只走 `Database::new`，其 schema 执行用 `CREATE TABLE IF NOT EXISTS`（不会给旧表加列）
- 后果：老库缺 `cost_usd` 列时，`log_request` 的 INSERT（repository.rs:79-87）直接报错 → 日志全部写入失败（且被 `let _ =` 吞掉）
- 优化切入点：Database::new 完成建表后调用迁移逻辑

### P2 — 性能与资源

**#8 写库在响应关键路径 + 全量 JSON 双重序列化**
- `log_chat_request(...).await` 完成才返回响应（openai.rs:97-108, 176-186）；messages 与 response 各 `serde_json::to_string` 一次（openai.rs:199-203），repository 再把 String 拷进 `Vec<Value>`（repository.rs:89-107）→ 每请求至少两次全量文本拷贝 + 一次（可能很大的）TEXT 行写入
- 流式路径 duration 语义失真（流建立即记，openai.rs:142-152）
- 优化切入点：接通 AsyncLogger（#1）即同时解决关键路径问题；response 可只存截断摘要或 usage 摘要

**#9 manager 内联重试的指数延迟偏移 + 与 utils/retry 策略脱节**
- manager.rs:526-533 先自增再计算 `2^retry_count` → 429 重试序列为 2s,4s,8s,16s,16s，与文档/单测的 1s,2s,4s,8s,16s（retry.rs:130-139）不符；且未使用 `BackoffStrategy::delay`，逻辑双份维护
- 优化切入点：`execute_http_with_retry_inner` 改调 `RetryStrategy::for_category(category).backoff.delay(attempt)`

**#10 GlobalMetrics 热路径写锁**
- 每次 `record_request` 拿 std RwLock 写锁并 `to_string` 分配（metrics.rs:45-57）；文档宣称纯原子（metrics.rs:7）与实现不符
- 优化切入点：per-vendor 换 `DashMap<String, VendorMetricsAtomic>`（VendorMetrics 字段原子化），或预分配 vendor 键避免字符串分配

### P3 — 死表/死代码/健壮性细节

**#11 `vendor_status`、`model_stats` 两张表无人写、前者读取函数零调用**
- schema.sql:26-49 建表；`get_all_vendor_status`（repository.rs:201-209）零调用者（grep 全仓）；`/admin/vendors` 读的是内存状态（admin.rs:76）
- 优化切入点：删除或接入（厂商状态持久化对重启后恢复熔断/限流状态有价值）

**#12 `/admin/cost` 端点不存在**
- manager.rs:163 注释宣称"用于 /admin/cost 端点"，router_builder.rs:50-70 无此路由；CostSnapshot 无出口（即便 #4 修复）

**#13 杂项**
- `search_logs` 关键词取自 `vendor` 查询参数（admin.rs:46）语义错位；LIKE 的 `%`/`_` 未转义（repository.rs:155）
- `map_request_log` 等用 `unwrap_or_default()` 逐字段静默容错（repository.rs:212-233）——类型不符时 timestamp=0 等脏值无告警
- `get_token_stats` 无 LIMIT 无时间过滤（repository.rs:181-191），表随时间线性增长后全表返回
- `request_id UNIQUE`（schema.sql:9）+ 失败被吞：同 request_id 重复写（重试场景）第二次 INSERT 报错无感知
- schema 初始化逻辑双份（database.rs:39-56 与 repository.rs:18-39）；`split(';')` 脆弱（database.rs:47）
- DI 演示块在 main.rs:55-63 对同一 SQLite 文件开第二个连接池（min_connections=1），连接预算翻倍
- AsyncLogger 若启用：Shutdown 竞态（先入队的 Write 在 Shutdown 后仍会被处理，语义 OK；但 `Err(_)=>break` 分支不 flush，async_logger.rs:95-98）；handle 每写一条过一把 tokio Mutex（:135-144）
- PreAggregator 若启用：flush worker 永不退出、无 graceful shutdown（pre_aggregator.rs:100-109）；每 entry 独立 session 无事务（:153）

---

## 5. 未尽事项 / 不可验证项

1. **cargo build / cargo test 未运行**：兄弟库路径依赖（../dbnexus 等 6 个）不在环境中，`cargo metadata` 报 `failed to load source for dependency 'dbnexus'`。AGENTS.md 的"0 warning / 测试全绿"声明本次无法复核。
2. **dbnexus DbPool 的 SQLite pragma（WAL、busy_timeout、synchronous）与 session 语义**（get_session 是否池化、是否串行化）不可查——`#10 写库延迟`与并发行为受此影响，需在补齐兄弟库后验证。
3. **sdforge::http::build_with_config 如何挂载路由**不可查：main.rs:115 未调用 `create_router`（router_builder.rs:50），而 admin handler 使用 axum `State` 提取器；sdforge 源码缺失导致"`/admin/*` 是否真正被生产服务暴露"无法100%确认（`init_global_state` 存在暗示 sdforge 侧有全局状态机制，api/global_state.rs:1-37）。
