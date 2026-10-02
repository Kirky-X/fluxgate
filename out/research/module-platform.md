# NexusFlow 平台层调研笔记：配置与基础设施

> 调研日期：2026-09-30
> 范围：`src/config/`（mod.rs / loader.rs / manager.rs）、`src/infrastructure/`（mod.rs / di.rs）、`src/main.rs`、`src/lib.rs`、`src/error.rs`；为核实「热更新生效范围 / 生产路由 / feature 门控」额外追读了 `src/api/admin.rs`、`src/api/router_builder.rs`、`src/api/global_state.rs`、`src/api/mod.rs`、`src/middleware/auth.rs`、`src/router/router.rs`、`src/vendor/manager.rs`（节选）、`Cargo.toml`、`tests/config_test.rs`、`tests/feature_matrix.rs`、`tests/circuit_breaker_test.rs`、`config.example.toml`。

## 0. 环境限制（重要）

- **本环境无法编译/测试**：Cargo.toml 以 path 依赖引用兄弟库 `../dbnexus` `../sdforge` `../limiteron` `../oxcache` `../trait-kit` `../inklog`（Cargo.toml:39-57），这些目录在本机不存在。实测：
  - `cargo metadata --format-version 1 --no-deps` → 退出码 0
  - `cargo check` → `error: failed to get 'dbnexus' as a dependency ... failed to read /home/dev/projects/dbnexus/Cargo.toml ... No such file or directory`
  - 因此 `cargo test`（含 config 相关测试）**未运行**。下文所有结论均来自静态阅读源码，未经验证的推断均已标注。
- **trait-kit / sdforge 内部机制无法核实**：`AsyncKit` 的拓扑排序、`require::<M>()` 类型擦除、sdforge `build_with_config` 的路由注册方式，只能从 NexusFlow 侧的调用与注释推断（di.rs:4-15 的文档注释），兄弟库源码缺失，此部分标注为「不可核实」。

---

## 1. 模块职责

| 文件 | 职责 |
|------|------|
| `src/config/mod.rs` (149 行) | 纯数据层：`Config` 及子结构体的 serde 定义（`ServerConfig`/`DatabaseConfig`/`VendorConfig`/`ModelConfig`/`RateLimitConfig`/`ConcurrencyConfig`/`GlobalModelConfig`/`PricingConfig`），全部 `Deserialize + Serialize + Clone`（mod.rs:11-148） |
| `src/config/loader.rs` (77 行) | TOML 文件 → `Config` 的加载 + 校验（重复 vendor ID / 重复模型 ID / API key env 警告），产出 `ConfigManager`（loader.rs:21-41） |
| `src/config/manager.rs` (139 行) | 运行时可变配置容器：`Arc<RwLock<Config>>`，提供读接口与热更新写入（manager.rs:10-50），以及模型解析查询（`resolve_model` manager.rs:60-89 等） |
| `src/infrastructure/mod.rs` (23 行) | 仅做 feature 门控转发：四 feature 全开时才编译 `di` 子模块（mod.rs:16-22） |
| `src/infrastructure/di.rs` (246 行) | trait-kit `AsyncKit` DI 组装：注册 oxcache/limiteron/dbnexus/inklog 四模块，拓扑 build，`verify_kit` 验证四能力可解析（di.rs:80-150） |
| `src/main.rs` (177 行) | 启动编排：日志 → 配置 → Database → VendorManager → （可选）DI kit → Router/ApiState → sdforge app → axum serve + 优雅停机（main.rs:17-150, 154-176） |
| `src/lib.rs` (51 行) | crate 门面：模块声明 + feature 门控 + 公共 re-export（lib.rs:6-50） |
| `src/error.rs` (324 行) | 统一错误 `NexusFlowError`（8 变体，thiserror）+ `IntoResponse` 映射 + 各来源 `From` 转换 + 12 个响应格式单测（error.rs:12-40, 42-65, 84-123） |

---

## 2. 数据流

### 2.1 启动流（main.rs:17-137）

```
argv[1] 或 "config.toml"                     main.rs:28
  → ConfigLoader::new(path).load()           main.rs:30
      ├─ 读文件 → toml::from_str → Config     loader.rs:30-36
      ├─ validate（vendor ID 去重 / 模型 ID 去重 / API key env 只 warn） loader.rs:43-75
      └─ ConfigManager::new(config)（Arc<RwLock<Config>>） loader.rs:40, manager.rs:16-20
  → db_config = config.database.clone()       main.rs:33-36（一次性快照）
  → Database::new(&path, pool_size)           main.rs:38（业务 DB 池，独立于 DI）
  → VendorManager::new(config_manager)        main.rs:41
      └─ pricing 快照 → CostCalculator        vendor/manager.rs:135-139（一次性）
  → VendorManager::initialize()               main.rs:42, vendor/manager.rs:195-215
      └─ get_enabled_vendors() 快照 → 逐个 get_or_create_instance（含队列 worker、限流器）vendor/manager.rs:196-199, 218-257
  → [cfg 四 feature] build_kit_with_config + verify_kit，kit 为局部变量，块结束即丢弃
                                              main.rs:47-64（见问题 P2）
  → Router::new(config_manager, vendor_manager) main.rs:66
  → ApiState { config, vendor_manager, router, client, 两个 converter, database } main.rs:68-76
  → init_global_state(api_state)（OnceCell 全局单例，供 sdforge handler 用）main.rs:78, api/global_state.rs:16-26
  → sdforge AppConfig 组装（host/port 来自配置；timeout=300s、CORS="*"、Auth=ApiKey 全部硬编码）main.rs:82-113
  → sdforge::http::build_with_config → axum::serve + graceful shutdown main.rs:115-137
```

### 2.2 请求热路径的配置读取（每请求）

```
请求 → Router.route(model)
  ├─ try_abstract_route: config.resolve_model()          router.rs:172 → manager.rs:60-89
  │    └─ 对每个候选：vendor_manager.get_status()         router.rs:181
  ├─ try_direct_route: config.get_vendor()                router.rs:149
  ├─ try_global_route: config.read() 遍历全表             router.rs:193-213
  └─ weighted_select → calculate_weight → compute_weight  router.rs:222-300
决策 RoutingDecision{vendor, model}（config 的克隆，新鲜）
  → vendor_manager.execute_*(&decision.vendor.id, ...)    api/openai.rs:63 等
      └─ 按 id 查 instances HashMap → instance.config（启动时快照！）vendor/manager.rs:597-604, 631-639, 712-720, 784-792
```

关键点：**路由决策用「新配置」，实际执行用「旧快照」**——两段读的是同一个 `ConfigManager` 的不同副本（见 P1）。

### 2.3 热更新流（/admin/config）

```
PUT /admin/config（admin.rs:129-139 update_config）
  → state.config.update_config(new_config)                manager.rs:30-50
      ├─ toml::to_string(&new_config)
      ├─ std::fs::write("config.toml", ...)  ← 写死 CWD 相对路径，非原子
      └─ *guard = new_config（写锁整体替换）
生效范围：
  ✅ Router 路由决策（vendor/model 增删、enabled、weight）——每请求实时读 manager.rs:60-89, router.rs:99/149/172/193
  ❌ VendorInstance（api_base / type_ / api_key_env / AdvancedRateLimiter / RPS / 队列）
     —— get_or_create_instance 命中旧实例直接返回 vendor/manager.rs:221-224；
        execute_* 全部使用 instance.config vendor/manager.rs:652, 735, 807
  ❌ 新增 vendor：Router 能路由到（decision.vendor 新鲜），但 execute_* 查 instances 无此 id
     → 直接 VendorUnavailable（vendor/manager.rs:597-604 等），因为实例只在启动时 initialize() 创建
  ❌ pricing：CostCalculator 启动快照 vendor/manager.rs:135-139
  ❌ server host/port、database 路径/池、sdforge 超时/CORS/认证：启动一次性读取 main.rs:38/55/82-113
其他写入口：
  PUT /admin/vendors/:id（admin.rs:149-168）：get_config() 克隆 → 改数组 → update_config（读-改-写竞态）
  POST /admin/models/global（admin.rs:186-255）：仅接受 id=="global"，改 global_model
```

### 2.4 DI 组装流（infrastructure/di.rs）

```
build_kit_with_config(db_config)              di.rs:80-111
  ├─ kit.set_config(OxcacheConfig::default()) di.rs:85（默认内存缓存）
  ├─ kit.set_config(minimal_flow_config())    di.rs:86（硬编码 1 条 0.0.0.0/0 令牌桶规则：capacity=100, refill=10, Reject）di.rs:29-51
  ├─ kit.set_config(db_config)                di.rs:87（生产 DB 路径的第二个连接池）
  ├─ register 4 模块（Oxcache → Limiteron → DbNexus → Inklog）di.rs:89-104
  └─ kit.build().await（拓扑排序，错误统一包成 NexusFlowError::Config）di.rs:106-110
verify_kit(&kit)                              di.rs:130-150
  └─ require::<M>() 逐一取出 4 能力：
     CacheBackend / Governor / ConnectionPool / LogDbProvider
DI 链（文档注释，不可核实）：oxcache → dbnexus → inklog；limiteron 独立   di.rs:13-14
```

生产装配是 `#[cfg(all(infrastructure, database, vendor-management, logging))]` 块（main.rs:47-64），lib.rs:44-50 同门槛导出 `di`。

---

## 3. 关键算法与数据结构

### 3.1 配置容器：`Arc<RwLock<Config>>`（std 同步锁）

- `ConfigManager` 可 Clone（Arc 共享），`read()` 返回 `RwLockReadGuard`，`get_config()` 整体深克隆（manager.rs:10-28）。
- 写入是「序列化→写文件→写锁替换」三步，无版本号/无 CAS（manager.rs:30-50）。
- 模型解析 `resolve_model`：抽象 ID 归一化为小写，O(V×M) 全表扫描，命中即克隆 `(VendorConfig, ModelConfig)`（manager.rs:60-89）；`abstract_id` 缺省回退 `model_id`（manager.rs:76-80）。`get_all_model_mappings` 同模式（manager.rs:109-137）。

### 3.2 校验算法（loader.rs:43-75）

两级 HashSet 去重：vendor.id 全局唯一（loader.rs:44-51）；vendor 内 model_id 唯一（loader.rs:53-62）；API key env 缺失仅 `tracing::warn` 不拒绝（loader.rs:64-72）。**没有**：未知字段拒绝（无 `deny_unknown_fields`，mod.rs:131-139）、weight/端口的数值范围、strategy 合法性、api_base URL 格式。

### 3.3 3D 权重（router.rs:279-325，配置消费方）

`compute_weight = model.weight×0.4 + queue_factor×0.3 + window_factor×0.3`（router.rs:290-300）；
`queue_factor = 1 - min(len/100, 1)`（router.rs:303-305）；window_factor：None→0.5、无预算→0、临期→budget×0.5、正常→budget（router.rs:312-325）。
选择：加权随机（`rng.gen::<f64>() * total_weight` + 累积比较，router.rs:248-260）；总权重≤0 时均匀随机（router.rs:239-246）。

### 3.4 DI 容器接口约定（di.rs）

- 配置注入靠类型：`set_config` 三种异构配置（oxcache/limiteron/dbnexus）由 trait-kit 按类型分发（di.rs:85-87）。
- 能力获取靠类型擦除 trait 对象：`Arc<dyn CacheBackend>`、`Arc<Governor>`、`Arc<dyn ConnectionPool>`、`Arc<dyn LogDbProvider>`（di.rs:130-150）。
- `minimal_flow_config()`：因 `FlowControlConfig::default()` 空 rules 过不了 validate，手工构造兜底规则（di.rs:29-51）——限流参数在代码里硬编码，不经 config.toml。

### 3.5 错误映射（error.rs）

- 8 变体 → HTTP 状态码：Validation→400、VendorUnavailable→503、Timeout→504、RateLimited→429、Config/Database/Routing→500、Protocol→502（error.rs:44-54）；响应体 `{error:{message,type}}`，type 为固定 snake_case 代码（error.rs:56-64, 68-81）。
- 来源转换：`From<dbnexus::DbError>`（database feature，error.rs:84-91）、`From<io::Error>`（error.rs:93-99）、`From<RecvError>`（error.rs:101-107）、`From<sdforge::ConfigError>`（sdforge-integration feature，error.rs:109-116）、`IsRetryable for NexusFlowError`（resilience feature，error.rs:118-123）。

### 3.6 Feature 门控组织（lib.rs / Cargo.toml）

- 声明 9 个 feature + default=full：`web-server/database/vendor-management/router/protocol/infrastructure/sdforge-integration/logging/full`（Cargo.toml:16-26），依赖链 `web-server→router→vendor-management→database→protocol`（Cargo.toml:19-22）。
- 模块门控：`config/error/protocol/utils` 恒编译；其余按 feature（lib.rs:6-27）；`di` 需四 feature 合取（infrastructure/mod.rs:16-22, lib.rs:44-50, main.rs:47-52）。
- 二进制 `required-features=["full"]`（Cargo.toml:11-14）。
- **另有 `resilience`/`cost-tracking` 两个 feature 被 44 处 `cfg` 引用但从未在 `[features]` 声明**（见 P5）。

---

## 4. 问题与优化切入点（按影响排序）

### P1【高】热更新只对路由决策层生效，执行层全部走启动快照 —— 半生效状态

- 证据链：
  - 写入口只替换 `ConfigManager` 内存 + 文件（manager.rs:47-48）。
  - Router 每请求实时读（router.rs:99、149、172、193）→ **生效**。
  - 但 `execute_*` 按 id 查 `instances` HashMap 并用 `instance.config` 构建 URL/认证/限流（vendor/manager.rs:597-604、631-639、712-720、784-792；`instance.config` 用于 652、735、807）；实例只在 `initialize()` 创建一次（vendor/manager.rs:195-215），`get_or_create_instance` 命中旧实例直接返回（221-224）→ **不生效**。
  - 新增 vendor 更糟：路由可达但执行必失败 `VendorUnavailable`（实例表无此 id，vendor/manager.rs:599-603）。
  - pricing 快照 vendor/manager.rs:135-139；server/db/sdforge 参数一次性 main.rs:38、55、82-113。
- 影响：`/admin/config` 的实际语义与「配置热更新」的预期严重不符；改 `api_base`/`type_`/`rate_limit`/新增 vendor 静默失效或报错，极易误判为「改了没保存」。
- 切入点：a) `update_config` 后触发 `VendorManager::reload()`（diff 配置、重建/复用实例、迁移队列）；b) 或 `VendorInstance` 不存 `VendorConfig` 快照，每次执行前 `config.get_vendor(id)` 取新鲜值，只保留有状态部分（队列/限流器/CB）；c) `CostCalculator` 改为每请求从 `ConfigManager` 读 pricing（或 pricing 变更时重建）。

### P2【高】DI 容器「建而不用」，且与业务组件各建独立资源池

- 证据：kit 是 main.rs:53-64 cfg 块内局部变量，`verify_kit` 后即丢弃；`Database::new`（main.rs:38）与 DI 的 `DbNexusModule` 池（main.rs:55-61）是**两个独立连接池指向同一个 SQLite 文件**；oxcache 在 src/ 中除 di.rs 外零引用（grep 证实）；kit 里的 limiteron `Governor` 与 router_builder.rs:51 自建的 `Governor` 也是两个实例（后者还只存在于未被生产挂载的 `create_router` 里，见 P3）。
- 影响：生产请求路径完全没有用到 DI 容器的任何能力（无缓存、无限流、无统一池）；启动多花一次建池开销；`minimal_flow_config` 硬编码的限流参数（di.rs:41-48）永远不会作用于真实流量。
- 切入点：要么把 kit 能力真正注入 `ApiState`（governor 进中间件层、ConnectionPool 作为 Database 后端、CacheBackend 接入路由/配置缓存），要么删掉这段仪式性代码并移除四个 path 依赖的 `kit` feature——两者都比现状诚实。

### P3【高】生产路由装配不可见：`create_router`（全部 /v1、/admin 路由 + 限流中间件）从未被 main.rs 调用

- 证据：`create_router` 定义于 api/router_builder.rs:50-73，唯一 re-export 是 api/mod.rs:18；main.rs:115 用 `sdforge::http::build_with_config` 构建 app，其后直接 `axum::serve`（main.rs:135），无任何 `.route/.nest/.merge`。仓库内唯一的 sdforge 声明式端点是 `#[service_api] health_check`（api/health.rs:10-16）。集成测试自己手工搭 Router（tests/api_test.rs:119 `create_app`）。
- 限制：sdforge 源码不在本环境，**无法核实** `build_with_config` 是否会自动发现/注册这些 handler（`#[service_api]` 宏的全局注册机制同样不可核实）。仅陈述可见事实：在本仓库范围内，找不到生产二进制把 `/v1/chat/completions`、`/admin/config` 等 handler 挂到 app 上的代码路径；若 sdforge 无自动发现机制，则生产服务器只有 /health 可用，且测试测到的路由与生产路由不是同一条代码路径。
- 切入点：在 main.rs 显式 `app.nest("/", create_router(state))`（或等价方式），让生产与测试共用同一装配函数；同时 `create_router` 内自建 Governor（router_builder.rs:51）应改为接收 DI kit 的 Governor。

### P4【高】热更新写入：绕过校验、非原子、路径硬编码、读-改-写竞态

- 证据：
  - `update_config`（admin.rs:129-139 → manager.rs:30-50）**不调用** `ConfigLoader::validate`：可通过 API 写入重复 vendor ID、重复模型 ID（loader.rs:44-62 只在启动路径执行）。
  - 写文件路径硬编码 `"config.toml"`（manager.rs:43），忽略启动时 `argv[1]` 传入的任意路径（main.rs:28-30）——自定义路径启动时，热更新会写错文件（在 CWD 新建/覆盖 config.toml）。
  - `std::fs::write` 直接整文件覆盖，无 tmp+rename，无备份：进程崩溃在写入中途 → config.toml 损坏，下次启动直接失败（loader.rs:24-28 只是「不存在」报错，无损坏恢复）。
  - 先写文件后换内存（manager.rs:43-48），两步非事务：并发两个 `update_config` 可交错为「文件=B、内存=A」；`update_vendor` 是 get_config() 克隆→改→整体写回（admin.rs:155-158），并发下丢更新。
- 切入点：update 前跑 `validate`（需把 validate 从 loader 提为独立函数）；tmp+rename 原子写并记住加载路径；写文件与内存替换放进同一把锁；`update_vendor` 改为在写锁内原地改（或引入 config 版本号 CAS）。

### P5【高】`resilience` / `cost-tracking` feature 未在 Cargo.toml 声明 → 44 处 cfg 块永远死代码

- 证据：`grep -rho 'feature = "[a-z-]*"' src/ tests/ sdk/src` 结果中 `resilience`×23、`cost-tracking`×21；而 `[features]` 表只有 default/full/web-server/database/vendor-management/router/protocol/infrastructure/sdforge-integration/logging（Cargo.toml:16-26）。未声明的 feature 无法被 `--features` 启用，`#[cfg(feature=...)]` 恒为 false。
- 直接后果（静态推论，本环境无法编译验证）：
  - `src/vendor/resilience/` 整个模块（mod/retry/circuit_breaker/fallback 4 文件）被 vendor/mod.rs:8 门控掉——熔断器、重试策略的「实现」全部不参与编译；`execute_http_with_retry` 永远走 `#[cfg(not(resilience))]` 分支（vendor/manager.rs:460-464）。
  - `utils/cost_tracker.rs`（utils/mod.rs:7-20）、`error.rs:118-123` 的 `IsRetryable` impl、VendorManager 的 `circuit_breakers` 字段（vendor/manager.rs:178-180）等同样不编译。
  - tests/circuit_breaker_test.rs:1 **无** `#![cfg(...)]` 门、直接 `use nexusflow::vendor::resilience::...` → `cargo test` 对该测试目标编译必失败（对照：vendor_circuit_breaker_test.rs:15 与 cost_tracker_test.rs:11 有门 → 被静默跳过）。即当前树 `cargo test` 大概率编译不过（无法运行验证）。
  - Cargo.toml:28-29 的 `unexpected_cfgs` lint 只登记了 `"openapi"`，未登记这两个 → 应有编译警告。
- 影响：AGENTS.md 宣称的「熔断器」「智能重试按 resilience 门控」等能力实际从未启用；README/docs 与代码行为脱节。
- 切入点：在 `[features]` 声明 `resilience`、`cost-tracking` 并决定是否并入 `full`；或删掉死代码；`unexpected_cfgs` check-cfg 补登记。

### P6【中】错误类型设计缺陷：错分类 + 混乱字段 + 声明与实现不符

- `From<io::Error> → Database` 变体（error.rs:93-99）：所有 IO 错误（含配置文件读写失败）都会被标成「数据库错误」，HTTP 响应为 500 database_error，误导排障。
- `From<RecvError> → Timeout { vendor: "unknown" }`（error.rs:101-107）：channel 关闭不等于超时，且 vendor 字段无意义。
- `Protocol` 变体带 `source: String` 与 `inner: Option<Box<dyn Error>>` 双字段（error.rs:16-21）：字段名 `source` 与 thiserror 的 `#[source]` 语义冲突；实际构造点全部 `inner: None`（anthropic_converter.rs:203、openai_converter.rs:199、api/openai.rs:257/293/302/308）——链式错误能力形同虚设。
- AGENTS.md 约定「anyhow + thiserror」，实际 src/ 中 anyhow **零引用**（grep 证实），anyhow 是无用依赖（Cargo.toml:62）。
- 切入点：io::Error 按路径/操作细分或引入 Io 变体；Protocol 变体保留 `inner` 并真正填充；删 anyhow 或真正采用。

### P7【中】配置校验面窄 + 配置项「死配置」

- 无 `deny_unknown_fields`（mod.rs:11-148 无该属性）：TOML 键拼写错误（如 `wight`）静默吃默认值，无任何提示。
- `GlobalModelConfig.strategy` 是自由 String（mod.rs:119，默认 "weighted"），**无消费者**：Router 的 `route()`（router.rs:38-62）不读 strategy，也不检查 `global_model.enabled`；`ConfigManager::is_global_model_enabled`（manager.rs:100-107）在 src/ 中无调用者（grep 证实）——策略配置是摆设。
- RateLimitConfig 的三种变体（mod.rs:86-102）只被 `AdvancedRateLimiter::new(&config.rate_limit)`（vendor/manager.rs:98）在**启动时**消费一次（叠加 P1 失效）。
- `ServerConfig.host` 被强解析为 `Ipv4Addr`（main.rs:122-127）：`localhost`、域名、IPv6 配置直接启动失败，配置层无此约束说明。
- 切入点：serde `deny_unknown_fields` + strategy 改 enum 并接到路由分支选择 + host 解析放宽或文档化。

### P8【中】锁与分配：async 上下文使用 std 同步锁 + 热路径深克隆

- `ConfigManager`/`instances`/`circuit_breakers` 均为 `std::sync::RwLock` 且 `.unwrap()`（manager.rs:22-24、47；vendor/manager.rs:207、220、568、598…）：锁毒化后所有后续请求 panic；临界区内做深克隆（`get_config` manager.rs:26-28；`resolve_model` 每命中克隆整个 VendorConfig+ModelConfig，manager.rs:60-89；`weighted_select` 再次克隆 vendor+model，router.rs:251-259；`try_global_route` 持读锁遍历克隆 router.rs:193-213）。请求热路径每次 O(V×M) 扫描 + 多次含 String 的结构体克隆。
- 现状流量下多半不是瓶颈（临界区短、无 await 持锁），但这是每请求固定开销的上界。
- 切入点：`RwLock<Arc<Config>>` 写时整体换 Arc、读侧只 `Arc::clone`（读路径零锁零克隆，或用 arc-swap/`watch` channel）；`resolve_model` 结果按抽象 ID 缓存 + 配置版本失效。

### P9【低】运维面：管理端点权限、硬编码运行参数、admin.rs 遗留疑问注释

- `/admin/config`、`/admin/vendors/:id` 可改写包括 vendors 定义在内的全部配置，与业务 API 共用同一 sdforge ApiKey 认证（main.rs:107-110），无独立授权层（create_router 里的限流中间件之外无 auth layer，router_builder.rs:53-72；middleware/auth.rs 的 `ApiAuthState` 在 src/ 中无使用者，仅测试使用 tests/api_test.rs:668）。仓库可见范围内无法确认 sdforge 是否对 /admin 有额外防护。
- `request_timeout_secs=300`（main.rs:90）、CORS `allowed_origins=["*"]`（main.rs:92）、认证方式（main.rs:107-110）全部硬编码，不属 Config，无法热更新也无法配置。
- admin.rs:175-242 `update_model` 留有约 45 行自我质疑的思考注释（"It DOES NOT have model_id…"），暴露 API 契约未想清楚：`/admin/models` 返回的是全局策略单例的 Vec 包装（admin.rs:171-184）。
- `get_models`/`update_model` 只支持 `id=="global"`（admin.rs:243-254），扩展性受限（单全局策略，无多策略）。

### P10【低】零内联测试、测试跳过不可见

- src/config/ 与 src/infrastructure/di.rs 之外的 config 相关单测全部位于 tests/config_test.rs（13 个测试函数，覆盖 manager 读路径与 pricing 反序列化，未覆盖 update_config 写路径与 loader 的 validate——`validate` 是私有方法，无直接测试）。di.rs 有 6 个内联测试（di.rs:152-244），但覆盖的是 trait-kit 交互，需要兄弟库才能跑。
- 由于 P5，`cargo test` 实际执行的测试集与表面看起来的不一致（部分目标静默 `#![cfg]` 跳过、一个目标编译失败）。

---

## 5. 值得保留的设计（避免优化时误伤）

- 配置读取/写入分离清晰（loader 只在启动、manager 运行时），`ConfigManager` Clone 语义（Arc 共享）让状态注入非常轻。
- `IntoResponse` 的错误码固定字符串映射（error.rs:68-81）不泄漏内部枚举结构，且有 12 个单测锁行为（error.rs:125-323）——这是 error.rs 里质量最高的部分。
- `resolve_model` 的 `abstract_id → model_id` 回退逻辑（manager.rs:76-80）与大小写归一化，是路由可用性的关键约定，重构时必须保留。
- feature 门控的分层（核心恒编译、重模块按 feature、di 四合取，lib.rs:6-50）思路正确，问题只在 P5 的声明缺失。

## 6. 本次实际执行的验证命令汇总

| 命令 | 结果 |
|------|------|
| `cargo metadata --format-version 1 --no-deps` | 退出码 0 |
| `cargo check` | 失败：`failed to read /home/dev/projects/dbnexus/Cargo.toml (No such file or directory)` → **测试未运行** |
| `grep -rho 'feature = "[a-z-]*"' src/ tests/ sdk/src \| sort \| uniq -c` | resilience×23、cost-tracking×21（对照 Cargo.toml [features] 无此二项） |
| 各处 `grep -n` 定位（create_router 调用点、ApiAuthState 使用点、anyhow 引用、is_global_model_enabled 调用者、oxcache 引用） | 均为空或仅定义/导出处，支撑 P2/P3/P6/P7 结论 |
