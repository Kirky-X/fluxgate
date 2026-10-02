# NexusFlow 手写功能去向分析：迁入自研库 vs 保持手写

**日期**：2026-10-01
**方法**：4 个独立分析（sdforge 域 / limiteron 域 / confers 域 / dbnexus+trait-kit+garrison 域），每个分析先读库源码（模块地图、Cargo features、相关模块全部源码）、再读 NexusFlow 侧手写代码、正反论证后下结论。全部结论带 file:line 证据，置信度自评。
**对象**：NexusFlow 全面实施后仍保留手写的 17 项功能（见《实施交付报告》第 3 节）。
**判定标准**：域契合度、通用性（是否有第二消费方场景）、API 风格一致性、实现/维护成本、对库现有用户的破坏面。自研库均为用户自己的项目（0.x-rc 期），"能不能加"不是问题，"该不该加"才是。

---

## 结论总览

| # | 手写功能 | 裁决 | 目标落点 | 置信度 |
|---|---|---|---|---|
| 1 | AnnotatedValue→强类型桥接 | ✅ 迁入库 | confers::types | 高 |
| 2 | SQLite 建库自举（mode=rwc） | ✅ 迁入库 | dbnexus DbConfig | 高 |
| 3 | 参数化批量执行 | ✅ 迁入库 | dbnexus Session | 高 |
| 4 | 重试延迟注入 + Retry-After | ✅ 部分迁移 | limiteron::retry | 高 |
| 5 | 跨库错误泛型化（消灭映射层） | ✅ 迁入库 | limiteron::circuit | 中 |
| 6 | 上游 SSE 分帧/解析 | ✅ 部分迁移 | sdforge::streaming::parser | 高 |
| 7 | canonical JSON 键归一化 | ✅ 部分迁移 | oxcache::utils | 高 |
| 8 | 每日时段限流窗口 | ✅ 迁入库 | limiteron::limiters | 中 |
| 9 | 常量时间比较 + key 摘要 | 🔄 库已有能力 | garrison::secure::ct_eq / protocol::apikey | 高 |
| 10 | 出站 HTTP 客户端 | ⛔ 保持手写 | — | 高 |
| 11 | 流式 permit 生命周期 | ⛔ 保持手写 | — | 高 |
| 12 | FallbackManager 泛化 | ⛔ 保持手写 | — | 高 |
| 13 | 业务流量指标（熵/滑窗/top-1） | ⛔ 保持手写 | — | 高 |
| 14 | 业务统计算法（EWMA/Po2C） | ⛔ 保持手写 | — | 高 |
| 15 | confers 跨条目唯一性校验 | ⛔ 保持手写 | — | 高 |
| 16 | 配置原地原子写 | ⛔ 保持手写 | — | 中 |
| 17 | LLM 协议转换 / 亲和策略 | ⛔ 保持手写 | — | 高 |

---

## 一、可落实到自研库（8 项）

### 1. AnnotatedValue→强类型桥接 → confers::types ✅

**现状**：NexusFlow `src/config/loader.rs:17-43` 手写 `annotated_to_json`/`config_value_to_json` 剥壳递归（调用点 75-76）。

**为什么该迁**：剥壳逻辑在 confers 生态内已有**三份同构实现**——confers 公共 `AnnotatedValue::to_json`（types.rs:876，Bytes→base64 于 908-913）、confers builder 私有 `value_to_json`（impl_/config/builder.rs:537，调用点 329/469）、NexusFlow 手写。库内都不统一，遑论下游。反向 `ConfigValue::from_json_value` 已存在（types.rs:517），唯独"AnnotatedValue → 任意 DeserializeOwned T"没有公开入口。旁证：NexusFlow 的注释（loader.rs:13）误称"ConfigValue 未实现 serde Deserializer"（types.rs:225 其实已实现，只是 visit_map 按 AnnotatedValue 信封解析 map 值 types.rs:358，且 AnnotatedValueHelper 无 `serde(default)` types.rs:706-714，裸 JSON 嵌套走不通）——连集成者都误判现状，正说明该 API 的可发现性缺口。

**API 草图**：

```rust
// confers/src/types.rs，#[cfg(feature = "json")]
impl AnnotatedValue {
    /// 一步桥接：剥壳为裸 JSON 后反序列化为强类型。
    /// 丢弃 source/priority/version/location 元数据（需文档明示）；
    /// Bytes 按 to_json 的 base64 语义落地。
    pub fn to_typed<T: serde::de::DeserializeOwned>(&self) -> Result<T, ConfigError> {
        let json = self.to_json(); // 复用 types.rs:876 现有剥壳
        serde_json::from_value(json).map_err(|e| ConfigError::InvalidValue {
            key: self.path.to_string(),
            expected_type: std::any::type_name::<T>().to_string(),
            message: e.to_string(),
        })
    }
}
```

**成本/破坏**：低 / 零破坏（纯 additive）。同步动作：库内收敛 builder.rs:537 私有重复实现；NexusFlow 删 loader.rs:17-43 并修正过时注释。

---

### 2. SQLite 建库自举（mode=rwc）→ dbnexus DbConfig ✅

**现状**：NexusFlow `src/database/database.rs:83` 的 `normalize_sqlite_url` + :16-22 父目录自建。dbnexus `DbConfig`（foundation/config/types.rs:549）只承载 url 字符串，建建连统一走 `sea_orm::Database::connect(&config.url)`（pool/db_pool/health.rs:77），没有 SqliteConnectOptions/create_if_missing 面。sqlx 默认不创建库文件——dbnexus 的**每个**自举场景（首次启动、内存外文件库）都会踩同一个坑。

**为什么该迁**："库文件不存在则创建"属于连接构建职责，域内。DatabaseType 已区分 Sqlite（types.rs:304-318），加后端专属连接选项的分支点现成。

**API 草图**：

```rust
// DbConfig 增（serde default 向后兼容）
#[serde(default)]
pub create_if_missing: bool,

// create_connection 的 Sqlite 分支（sqlite feature 门控）：
// create_if_missing=true 时改走
//   sea_orm::SqliteConnectOptions::from_str(&url)?.create_if_missing(true)
//   → Database::connect(opts)

// 便捷构造（把 :memory:/裸路径/显式 scheme/保留用户显式 mode 的
// 规范化逻辑集中到 dbnexus，NexusFlow 的 normalize_sqlite_url 删除）
pub fn for_sqlite_path(path: &str, create_if_missing: bool) -> Self;
```

**成本/破坏**：低 / serde default 序列化兼容。注意 dbnexus 处于 rc 期，配置面扩张需同步文档并验证 feature 组合编译。

---

### 3. 参数化批量执行 → dbnexus Session ✅（优先级最高）

**现状**：`Session::execute_raw(&self, sql: &str)`（session.rs:478）只收裸 SQL，事务/非事务两条路径都落 `execute_unprepared`（session.rs:584/607-613）；参数化执行面只存在于 DuckDB（execute_duckdb_with_params:1134）与 Cypher（:1474）路径。NexusFlow 批量 INSERT 被迫 `session.connection()?.begin()` 拿原生 sea-orm 事务绕道（repository.rs:97-114）——连接仍来自 dbnexus 池，却**完全绕开了其权限解析/慢查询/重试防御链**。

**为什么该迁**：bind 是数据库层通用能力，域内；且现状直接削弱库自己的防御体系。

**API 草图**：

```rust
pub async fn execute_with(&self, sql: &str, values: Vec<sea_orm::Value>)
    -> DbResult<ExecResult>;
// 内部 Statement::from_sql_and_values(backend, sql, values)，
// 复用 execute_raw 同一防御链（DDL 拦截 + 表级权限 + 慢查询指标，
// 均基于 sql 文本不受影响）；有会话事务时 tx.execute(stmt) 否则 conn.execute(stmt)。
// 姊妹方法 query_rows_with；再加 executemany(sql, rows) 内部包单事务，
// 直接服务批量写入场景。
```

**成本/破坏**：中 / 纯新增无破坏。需保证权限链对参数化语句的解析失败语义与 execute_raw 逐字对齐，并验证 sea-orm 2.0 `execute(Statement)` 与 `execute_unprepared` 在 sqlite 上的行为差异。

---

### 4. 重试延迟注入 + Retry-After 覆盖 → limiteron::retry ✅（部分迁移）

**现状**：`RetryPolicy::execute`（limiteron src/retry.rs:153-173）在第 169 行内部直接 `sleep(self.delay_for(attempt))`，签名只有 op + `is_retryable: Fn(&E)->bool`，无 attempt 回调也无延迟覆盖点。NexusFlow 侧：`src/utils/retry.rs:225-248` 的 `retry_delay_for` 纯函数 + `src/vendor/manager.rs:577-660` 自管重试薄循环（429 读上游 Retry-After 头覆盖退避）。

**裁决逻辑**：需求里只有"延迟覆盖"是通用原语——Retry-After 是任何打上游 HTTP 的客户端都会遇到的服务器侧退避指示；按状态码集合分类、per-category max_retries 是 LLM 网关业务策略，**不应下沉**。

**API 草图**（叠加新增，不改既有 API）：

```rust
// limiteron/src/retry.rs
pub enum RetryDecision {
    RetryAfter(Duration), // 覆盖默认退避（如上游 Retry-After 头）
    Retry,                // 用策略默认退避 delay_for(attempt)
    Stop,                 // 立即返回该错误
}
pub async fn execute_with<F, Fut, T, E, D>(&self, op: F, decide: D) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    D: Fn(u32, &E) -> RetryDecision; // attempt 从 1 起
// 既有 execute() 退化为 decide = |_, e| if is_retryable(e) { Retry } else { Stop }
```

**成本/破坏**：低 / 零破坏（纯新增）。注意 retry feature 未纳入 limiteron 的 full，下游需显式开启。

---

### 5. 跨库错误泛型化 → limiteron::circuit ✅（大动作，rc 期窗口）

**现状**：`CircuitBreaker::execute` 锁 `Fut::Output = Result<T, LimiteronError>`（src/circuit/types.rs:377-380），`ErrorClassifier` 锁 `&LimiteronError`（types.rs:40-49），拒绝路径也返回 `LimiteronError::LimitError`（types.rs:415/448）。后果：**两个独立消费方各自手写双向映射**——NexusFlow 127 行前缀串编解码且自认有损（error_map.rs:13-16"不带前缀的 Other 无法还原"），garrison 独立再写一遍 GarrisonError↔LimiteronError 映射（garrison/src/limiteron/circuit.rs:58-91）。blanket impl `From<E: Display>` 因 coherence 不可行（error/mod.rs:34/62/66 已有 `#[from]` 具体实现）。

**API 草图**（默认类型参数保兼容）：

```rust
pub trait FailureClassifier<E: ?Sized>: Send + Sync + std::fmt::Debug {
    fn is_counted_as_failure(&self, error: &E) -> bool;
}
// DefaultErrorClassifier 仅 impl FailureClassifier<LimiteronError>；
// 旧 ErrorClassifier 保留为类型别名/deprecated 桥接一个版本
pub struct CircuitBreakerConfig<E = LimiteronError> {
    pub error_classifier: Arc<dyn FailureClassifier<E>>,
    /* ... */
}
pub struct CircuitBreaker<E = LimiteronError> { /* ... */ }
impl<E: From<LimiteronError>> CircuitBreaker<E> {
    // 业务错误直通；库内拒绝路径经 E::from(LimiteronError::LimitError(..))
    pub async fn execute<F, Fut, T>(&self, operation: F) -> Result<T, E>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<T, E>>;
}
// NexusFlow 侧随之删除 error_map.rs 编解码：
// NexusFlowError: From<LimiteronError> + impl FailureClassifier<NexusFlowError>
```

**成本/破坏**：高 / 对现有用户破坏中等偏高（显式类型标注、`Arc<dyn ErrorClassifier>` 存储与 trait 实现均需迁移，garrison 同受影响）。**rc 期正是一劳永逸修 execute 形态的窗口**；建议旧 trait deprecate 过渡一个版本。泛型化会传染 config（error_classifier: Arc<dyn ErrorClassifier>，types.rs:92），属核心类型重构，需拍板。

---

### 6. 上游 SSE 分帧/解析 → sdforge::streaming::parser ✅（部分迁移）

**现状**：NexusFlow `src/protocol/sse_frame.rs`（SseFrameParser:87-124、scan_frame_delimiter:131-147、build_frame:150-176、SseFrame:45-72、reframe_passthrough_stream:188-235）。sdforge::streaming 现状为纯出站编码：StreamEvent serde 枚举（streaming/mod.rs:25-51）、stream_to_sse 与 IntoResponse（streaming_impl.rs:90-185），grep 全库无任何入站 SSE 解析器；sdforge 依赖树无 reqwest（仅 dev-deps，Cargo.toml:788-792）。

**裁决逻辑**：跨块字节缓冲、`\n\n`/`\r\n\r\n`/`\n\r\n` 字节层定界、帧完整才 UTF-8 解码、data:/event: 行解析是 **WHATWG SSE 规范的纯协议能力，零 LLM 语义**，与 sdforge::streaming"流式工具"定位互补（现有出站编码、缺入站解析）。但 `SseFrame::is_error`（sse_frame.rs:57-71，嵌 Anthropic 错误 JSON 形状）、`[DONE]`（OpenAI 终止标记，manager.rs:948-950 消费）、StreamChunk 反向映射（parse_stream_frame）是 NexusFlow IR 领域，**必须留在应用**，以扩展方法/自由函数挂在库类型上。

**API 草图**：

```rust
// sdforge::streaming::parser（streaming feature，新增可选 bytes 依赖）
pub struct SseFrame { pub raw: String, pub event: Option<String>, pub data: Option<String> }
// 注意：不带 is_error() —— 错误判定是消费方领域语义

pub struct SseFrameParser { /* buffer: Vec<u8> */ }
impl SseFrameParser {
    pub fn new() -> Self;
    pub fn feed(&mut self, chunk: &[u8]);
    pub fn next_frame(&mut self) -> Option<SseFrame>;
    pub fn flush(&mut self) -> Option<SseFrame>;   // EOF 残余帧，对齐 EventSource 语义
    pub fn is_empty(&self) -> bool;
}

pub fn frames<S, E>(stream: S) -> impl Stream<Item = Result<SseFrame, SseStreamError>> + Send + 'static
where S: Stream<Item = Result<bytes::Bytes, E>> + Send + 'static, E: std::fmt::Display;

// 现有透传行为参数化后保留为便捷组合子：
pub fn reframe_passthrough<S, E, F>(stream: S, error_frame: F)
    -> impl Stream<Item = Result<bytes::Bytes, std::io::Error>>
where F: Fn(&SseStreamError) -> String; // 调用方决定中流错误的 SSE error 事件内容
```

**成本/破坏**：中 / 无破坏（新增子模块，现有 API 不动；streaming feature 新增 bytes 可选依赖，default features 不含 streaming 的用户零影响）。NexusFlow 侧 6 个引用点改为库路径，tests/sse_test.rs 一并迁移。

---

### 7. canonical JSON 键归一化 → oxcache::utils ✅（部分迁移）

**现状**：NexusFlow `src/api/response_cache.rs:32-51`（canonicalize 递归键排序、数组保序）+ :59（response_cache_key）。oxcache 的 KeyGenerator 文档已宣言"键验证和规范化"是其职责（utils/key_generator.rs:5-9，含 namespaced_key/validate_key:165/189），serde_json 已是其依赖（Cargo.toml:335）——迁入零新增依赖。

**裁决逻辑**：canonicalize 是键序无关的确定性序列化，属缓存键域通用原语；`resp:` 前缀、租户哈希维度、store/check 编排是应用层业务，保留。

**API 草图**：

```rust
// oxcache::utils
pub fn canonical_json(value: &serde_json::Value) -> serde_json::Value; // 对象键递归字典序、数组保序
pub fn canonical_json_string(value: &serde_json::Value) -> String;
// KeyGenerator 增
pub fn json_hash_key(&self, parts: &[&str], body: &Value) -> Result<String, OxCacheError>;
// canonical → sha256 → 模板键组装
```

**成本/破坏**：低 / 纯新增。**迁移注意**：NexusFlow 侧须逐位保持 canonical 序列化输出一致，否则旧响应缓存键整体失配（一次性失效，可接受，记 changelog）。

---

### 8. 每日时段限流窗口 → limiteron::limiters ✅（建议等第二消费方）

**现状**：NexusFlow `src/vendor/rate_limiter.rs:87-93`（TimeWindow 变体）+ :152-157（时段门控；注：当前实现为纯时段门控，无窗内计量）。limiteron 现有窗口原语全部基于时长：FixedWindowLimiter 的 `window_size: Duration`（fixed_window.rs:42-44）、QuotaLimiter 的 `window_start: Instant`（quota_limiter.rs:31），无法表达"日历对齐的每日时段/午夜重置"——NexusFlow 注释已书面记录此缺口。基础设施齐备：chrono 已是基础依赖，Clock trait 含 unix_timestamp_nanos 支持测试注入（clock.rs:26-34），惰性重置可复用 fixed_window.rs:91-123 的 CAS 模式。

**API 草图**：

```rust
// limiteron/src/limiters/calendar_window.rs（新原语）
pub struct CalendarWindowLimiter {
    slot_start: NaiveTime, slot_end: NaiveTime, // 支持跨午夜 start > end
    utc_offset_secs: i32,                       // 显式时区，不用隐式 Local
    max_requests: u64,
    count: AtomicU64,
    current_slot: AtomicU64, // 槽键（如 days-since-epoch），翻转惰性清零
    clock: Arc<dyn Clock>,
}
#[async_trait] impl Limiter for CalendarWindowLimiter { /* allow/peek/remaining，复用 fixed_window CAS */ }
```

**成本/破坏**：低 / 无破坏（纯新增模块 + re-export）。**置信度中**的原因：唯一消费方是 NexusFlow 约 10 行门控，通用性判断靠领域常识而非多消费方实证——建议等第二个真实用例出现再迁（避免为单点需求扩库 API）。

---

## 二、库已有能力，直接替换（1 项）

### 常量时间比较 + API key 摘要 → garrison（无需加能力）

| 手写点 | garrison 现成能力 | 对比 |
|---|---|---|
| `src/middleware/auth.rs:24` `constant_time_eq` | `secure::ct_eq::constant_time_eq`（secure/ct_eq.rs:47，基于 subtle） | **garrison 严格优于手写**：长度不等也不 early return（pad 到 max_len，ct_eq.rs:55-62）；手写版长度不等立即返回，泄漏长度信息 |
| `auth.rs:17` `sha256_hex` + 摘要表 | `protocol::apikey::ApiKeyInfo.secret_hash` 即 sha256(secret) hex 且明确不存明文（protocol/apikey/mod.rs:54-58，CWE-916），verify 链路 sha256_hex + constant_time_eq（handler.rs:455-461），另带吊销/过期/轮换/scope | lib.rs:128 明文广告此能力，pub mod 导出（lib.rs:374/392），feature 门控 secure-ct-eq（Cargo.toml:475）与 protocol-apikey（:353） |

**分级结论**：`ct_eq` 可立即 drop-in 替换（语义等价微升，顺带修复长度泄漏）。完整采用 `ApiKeyHandler` 意味着 key 签发/存储从 config.toml 静态摘要表迁到 DAO 动态管理，是对外可见的行为变更，**需作为独立迁移决策与用户确认**——本判定仅主张"能力已在 garrison，勿在 NexusFlow 或 garrison 重复造第二份"。

---

## 三、保持手写（8 项，理由均为"域边界"而非"能力不足"）

| # | 功能 | 决定性理由（证据） |
|---|---|---|
| 1 | **出站 HTTP 客户端** | sdforge 依赖树零 reqwest（仅 dev-deps，Cargo.toml:788-792），纯服务端框架（http/health/metrics/rbac/websocket/grpc/mcp-server，lib.rs:188-463）；引入出站客户端 = 域蔓延 + TLS 栈重依赖，且 reqwest 本身就是成熟的"出站 HTTP 库"。NexusFlow 手写中没有一段可通用：build_authed_request 是 vendor 认证语义（manager.rs:502-514），重试循环嵌业务分类（630-642），check_stream_status 嵌 metrics 记账（808-820）。可通用的部分已各归其位（退避→limiteron，熔断→limiteron） |
| 2 | **流式 permit 生命周期** | 票据域归属 limiteron（ConcurrencyPermit 包装其 OwnedSemaphorePermit，rate_limiter.rs:37-45/71-73），sdforge 从未涉足上游连接治理，其 streaming 只做出站编码；实现仅 5 行标准 unfold 惯用法（manager.rs:906-914），造库抽象收益≈0。若未来要消除重复，正确位置是 limiteron 提供 permit-绑流组合子 |
| 3 | **FallbackManager 泛化** | 语义形态不同：limiteron `execute_with_fallback`（src/fallback.rs:282-341）是"同组件 op 失败→执行 fallback_operation"单跳降级；NexusFlow `FallbackChain::next_after_failure`（resilience/fallback.rs:83-100）是"失败 vendor→按 priority 链式换到对等目标"。FallbackManager 还硬耦合 oxcache（fallback.rs:148/174 l2_cache），garrison 作为第二消费方也只用 FallbackStrategy 枚举、明确绕开完整功能（garrison/src/limiteron/fallback.rs:20-23）。泛化 ComponentType（pub，已进入全部签名）= 双重破坏而无可对应收益 |
| 4 | **业务流量指标（SlidingCounter/熵/top-1）** | 不是限流原语：limiteron telemetry 的 MetricsSnapshot 全部是 limiter 自身的请求/延迟分位/CB 跳闸/CPU 指标（src/telemetry/monitoring.rs:48-66），入口是 metrics.record_check；而份额分析不执行允许/拒绝决策，且读侧"全局读锚"约定（metrics.rs:227-233）为 vendor 集合份额衰减定制。收编即域蔓延，monitoring feature 还拖 prometheus 重依赖 |
| 5 | **业务统计算法（EWMA/Po2C/温度平滑）** | 八库全源码 grep 无任何实现（confers secret/crypto.rs:83 与 sdforge auth.rs 的 "entropy" 均为密码学熵，无关）；算法与 vendor_ewma、RoutingSection 配置紧耦合（router.rs:536-538），服务"LLM 厂商调度"单一业务决策 |
| 6 | **confers 跨条目唯一性校验** | confers `ValidationRule`（impl_/validator.rs:42-63）是 10 种字段级变体 + Custom，**全库无消费方**（仅 lib.rs:189 re-export，derive 宏与 CLI 均未引用）——给死 API 加变体无收益。唯一性需要"作用域 Vec + 键路径"参数，形状不匹配；三条规则中 env 存在性检查（loader.rs:116-124）依赖 std::env 副作用，永不属于纯配置校验库。手写仅 20 行且有测试覆盖 |
| 7 | **配置原地原子写** | confers 全库不存在原地替换原语：SnapshotManager save/save_blocking 均为"时间戳唯一文件名 + 写新文件 + prune"（impl_/snapshot.rs:254-261、313、391），写路径哲学是不可变留痕（恰好绕开 Windows rename-to-existing 语义）。NexusFlow 实现的核心是应用层事务协议（写锁串行 108-111、RMW 120-131、磁盘落定才切内存 185），confers 即便提供 atomic_write 原语也覆盖不了这段协议；可抽离的仅 ~25 行 tmp+rename，与 tempfile::NamedTempFile::persist 高度重合。DynamicField 是纯内存态 handle（impl_/dynamic.rs:36-66），硬塞持久化职责破坏单一职责 |
| 8 | **LLM 协议转换 / 亲和策略** | 八库域矩阵（sdforge=多协议 SDK 宏框架、trait-kit=DI、oxcache=缓存、dbnexus=数据库、inklog=日志、limiteron=限流、confers=配置、garrison=身份认证）无一含 LLM API 协议域，全源码 grep openai/anthropic 零命中——2800 行协议语义映射正是网关的核心域价值。亲和存储已落 oxcache，剩余"上次成功服务的 vendor + 命中续期 + boost 不硬粘"是路由业务语义（affinity.rs:4-18），oxcache 不该知道 vendor 概念 |

---

## 四、附带发现

1. **sdforge 疑似存在与 NexusFlow #5④ 同构的 bug**（低置信，建议单独核查）：其 ratelimit-http 的 Tower 中间件若在 handler 返回时释放限流许可、而响应 Body 仍在流式传输，则"流式请求并发上限"失效——与 NexusFlow 刚修复的 recId=5④ 同类问题，可能在 sdforge 侧同样存在。
2. **NexusFlow loader.rs:13 的注释对 confers 的描述已过时**：ConfigValue 的 Deserialize 在 types.rs:225 已存在，缺口是"信封形状"（visit_map 按 AnnotatedValue 信封解析 + Helper 无 serde(default)）而非"没有实现"——集成者误判现状本身佐证了 `to_typed` API 的可发现性价值。落实第 1 项时应顺带修正该注释。
3. **confers builder.rs:537 的私有重复剥壳实现**应在加 `to_typed` 时一并收敛到新 API（内部重构，不动公共面）。

---

## 五、建议落实顺序

| 批次 | 内容 | 特征 |
|---|---|---|
| 立即可做 | garrison ct_eq 替换（顺带修长度泄漏）→ confers `to_typed` → dbnexus `create_if_missing` → limiteron `execute_with` | 各库独立、低成本、零破坏、高置信 |
| 第二批 | oxcache `canonical_json`（注意缓存键失配）→ dbnexus `execute_with/executemany`（注意防御链语义对齐）→ sdforge `streaming::parser` | 中等成本，各有需对齐的细节 |
| 大动作，需拍板 | limiteron `CircuitBreaker<E>` 泛型化 | 收益最硬（消灭两份手写映射）但破坏面横跨 NexusFlow 与 garrison；趁 rc 期做，旧 trait deprecate 过渡 |
| 缓一缓 | limiteron `CalendarWindowLimiter` | 唯一消费方是 NexusFlow 10 行门控，等第二个真实用例再迁（YAGNI） |

---

## 附录：分析过程说明

- 4 个分析各自独立完成，共读双侧源码（库侧 + NexusFlow 侧）约 100+ 次源码访问，全部结论附 file:line。
- 置信度标注含义：高 = 源码证据决定性；中 = 依赖领域常识判断或多消费方证据不足（第 5、8 项）；低 = 无（无低置信结论进入主表）。
- garrison 虽不在七个必用库名单，但同为用户自研，作为潜在落点纳入了评估（第 9 项）。
- 本文档只做分析，未改动任何代码。
