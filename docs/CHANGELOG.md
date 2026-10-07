# NexusFlow 更新日志

[🔒 安全文档](./SECURITY.md) · [🤝 贡献指南](./CONTRIBUTING.md) · [📖 用户指南](./USER_GUIDE.md) · [🏗️ 架构文档](./ARCHITECTURE.md)

本格式基于 [Keep a Changelog 1.0.0](https://keepachangelog.com/en/1.0.0/)，版本号遵循语义化版本。

> **注**：本项目尚未打 tag，以下版本对应 `Cargo.toml` 的版本线（当前 `version = "0.1.0"`）。

---

## [Unreleased]

### Changed

- **依赖卫生**：清理冗余依赖并收紧特性面——移除死依赖 `sys-info`（全仓零引用，`cc`+`libc` 的 C 编译面与 `links="info"` 约束随之从编译图消失）与 `tokio-util`（全仓零引用）；根 `anyhow`/`futures-util` 直接声明移除（根 crate 零引用，examples 自带 / 仅测试使用且 dev-dependencies 保留）；`hex`/`sha2`/`subtle`/`once_cell` 转 optional 挂 `web-server`，`url` 转 optional 挂 `database`/`sdk`（对应 feature 关闭时不再激活本 crate 的直接依赖）。`sha2` 0.10 → 0.11 对齐兄弟库（`Digest` 导入形态不变）；`sqlx` 侧 0.10 有两条上游锚点（`sqlx-core` 的 migrate 特性为当前编译面；`sqlx-macros-core` 的非 optional 依赖在解析图/Cargo.lock 中，本仓当前组合未开启其 macros feature 故不编译），其升级后双版本自然收敛。移除 inklog 的 `http`/`cli` 特性声明（本仓零调用；无 sdforge 的轻量组合省约 40 个包，full 构建省 axum-server/fs-err/glob 3 个——clap/schemars 经 sdforge 保留）。

- **结构**：`crates/llm-gate` 路由核心库并回主 crate（撤销 N1 workspace 拆分）——config/i18n/protocol/storage/utils/vendor/router 模块直接位于 `src/`，Fluent 目录 `locales/` 移至仓库根（`include_str!` 相对路径不变），llm-gate 集成测试并入 `tests/`（`gate_` 前缀，共享 helper 迁至 `tests/gate_common/`）；`resilience`/`protocol`/`vendor-management`/`router`/`cost-tracking` feature 由「转发 llm-gate 同名 feature」改为直控本 crate 模块与 optional 依赖（单开关语义不变）；`GateError` 定义移至 `nexusflow::error::gate`（顶层 `nexusflow::error::GateError` re-export 保留，`nexusflow::config` 等全部公开路径不变；`llm-gate` 从未发布 crates.io，无外部消费方）。

- **BREAKING**：Rust SDK（`sdk` feature）类型与 admin 面重塑对齐网关真实形状——`ChatMessage.content` 支持多模态内容件（`ChatContent`），`ChatRequest` 增 tools/tool_choice/vendor_extensions/penalties；`ChatResponse` 移除恒空的 `vendor` 字段（网关响应不注入）、finish_reason 结构化为四变体枚举、usage 含 cache_read；`ChatStream` 解析 tool_calls 增量片段与 usage 尾帧；admin 失配修复——`models()` 改 `GET /admin/models`（原 `GET /v1/models` 网关未注册恒 404）、`health()` 改 `GET /api/v1/health`（原 `admin/health` 不存在）、`vendors()` 对齐运行面形状 `{id,status,queue_length,key_pool}`；新增 `upsert_model` / `delete_model`（目录 CRUD）。
- **BREAKING**：`GET /admin/models` 响应形状由 `GlobalModelConfig` 数组（原 global_model 单例包装）变更为 `ModelCatalogEntry` 数组（九字段模型能力目录：context_window / 四档单价（currency，每 1M tokens）/ currency / max_output_tokens / supports_tools / supports_thinking）。全局模型策略改经 `POST /admin/models/global`（存量契约保留）或 `GET /admin/config` 读写。

### Fixed

- **feature 图隐式前提显式化**：`database` 补 `logging` 联动（async_logger 无条件消费 inklog）、`web-server` 补 `infrastructure` 联动（server/shutdown 用 trait-kit）——修复 `--no-default-features --features "database,db-sqlite"` / `"web-server,db-sqlite"` 组合编译失败（此前仅 `full`/default 组合可用；单独开 `database`/`web-server`/`logging` 仍按既有契约需随任一 db-* 后端组，inklog kit 守卫）。
- **feature 组合自洽**：`gate_vendor_test`（resilience）、`gate_price_factor_routing_test`（router）、`gate_vendor_key_pool_test`（vendor-management）补 cfg 门控，三个 `server_*_test` 补 `#![recursion_limit = "256"]`（既有递归深度问题，仅非 sqlite 后端 `--all-targets` 暴露），至此 9 个 feature 组合 `--all-targets` 全绿；特性矩阵文档口径、`docs/CONTRIBUTING.md` 验证命令与测试基线（367/764）、README 双语测试规模表、PR 模板同步更正；新增两条依赖链断言（database→logging、web-server→infrastructure）与 CI 单特性组合检查（database / web-server / logging + db-sqlite 三行）。

### Added

- **SDK 工具环与多模态**：SDK chat 面补齐 function calling（工具定义/选择/调用环回填消息）与多模态图片内容件（URL / base64 data URL），流式 chunk 携带 tool_calls 增量片段与 usage 尾帧——对齐 IR（D2）与网关出站真实形状；SDK admin 面与网关路由表零差异由路由 diff 测试钉住。
- **模型能力目录**：`[[model_catalog]]` 配置段（字段对齐 agentstem `ModelEntry`）+ `/admin/models` CRUD——`GET` 目录列表、`PUT /admin/models/:id` upsert（path 标识权威）、`DELETE /admin/models/:id` 下条（缺条显性 400 validation_error；`global` 为策略段保留键）。`llm-gate` 暴露 per-model 能力查询（`VendorManager::model_capabilities[_for_vendor]`）：已登记模型返回真实值（context_window > 0 强制校验——TUI auto-compact 依赖非 0 窗口），未登记回落 per-flavor 保守默认（openai 128k / anthropic 200k / ollama 4096）。定价单位契约：「currency 计价，每 1M tokens」（对齐 agentstem `estimate_usage_cost` 口径的计价消费）。校验：model_id 去重/非空白、context_window > 0、价格有限非负、条目上限 1024（加载与 `POST /admin/config` 热更新两路同拒）。

- **gRPC 服务器**（`grpc` feature，独立门控不进 `full`——sdforge grpc 编译期需 protoc）：`src/server/grpc.rs` 镜像 HTTP 封装方式——`[grpc]` 配置段（默认禁用，显式 `enabled = true` opt-in）、`build_app` 装配后经 `ServerHandle::take_grpc_server` 产出 `GrpcServer`、`serve` 走 sdforge `build_server_with_graceful_shutdown`（signal 后停止接受新连接、等待 in-flight 完成，与 HTTP 侧 axum `with_graceful_shutdown` 同语义，停机信号经 watch 广播共享）。服务面为 sdforge `sdforge.v1.SdForgeService` 门面（GetInfo / Call / CallStream，JSON over gRPC）：Call/CallStream 逐调用校验 `[auth]` 段 SHA256 摘要（`x-api-key` 或 `authorization: Bearer`，常量时间比较，语义对齐 HTTP 中间件；`[auth]` 缺段或禁用时放行），GetInfo 免认证（对应 HTTP `/health` 白名单位置）。TLS 不经 sdforge 终结（外部反代承担）。`#[forge(grpc_method = "...")]` handler 经 inventory 注册即可被门面路由。冒烟测试 `grpc_smoke_test` 走真实 tonic 链路（认证边界 / handler 路由 / 优雅停机 / 句柄幂等）。

---

## [0.1.0] - 2026-04-04

自初始提交起在 0.1.0 版本线上落地的全部变更（63 个提交归纳）。

### Added

- **网关核心**：智能 LLM 路由代理初始骨架——多厂商统一接入，OpenAI / Anthropic 协议互转（Chat / Embeddings ↔ Messages），入站与出站协议相同时零拷贝透传；SSE 流式编码器并接入流式响应路径
- **路由调度**：3D 权重计算（`model.weight` / `queue_factor` / `window_factor`，含 `WindowInfo`）与加权随机选择
- **厂商管理**：VendorManager（请求队列 / 令牌桶限流 / 并发控制 / 状态机）、基于信号量的 ConnectionPool 并发上限
- **韧性**：熔断器三态机（limiteron::circuit，集成进 VendorManager）；FallbackChain（按 `priority` 建链 + 触发策略）；按状态码分类的重试（429 指数退避 / 5xx 固定间隔）
- **入站防护**：API Key 认证（SHA256 摘要存储 + 常量时间比较）、limiteron 入站限流与封禁、拥塞感知优先级准入（默认关闭）
- **SQLite 持久化**：请求日志异步批量写入、Token 预聚合统计、`request_logs.cost_usd` 列（PRAGMA 迁移）
- **成本追踪链**：`PricingConfig` 价格表 → `CostCalculator`（价格表查询）→ `CostTracker`（dashmap 分片 + 原子累积）→ VendorManager worker 成功路径集成
- **可观测性与管理端点**：GlobalMetrics + `/admin/metrics`；配置热更新（`/admin/config`）、日志查询 / 关键词搜索、Token 统计、厂商状态与重置、模型管理
- **i18n**：Fluent 双语目录（en/zh）+ 语言检测链
- **SDK**：`src/sdk` 模块（`sdk` feature 门控，原顶层独立 crate 已并入）——chat（含 `chat_stream` SSE 解码）/ embed / models / vendors / health
- **DI 组装**：trait-kit AsyncKit 容器统一组装兄弟库模块（启动期 `4/4 modules resolved` 验证）
- **运维**：优雅停机（Ctrl+C / SIGTERM）、feature 门控与组合矩阵测试、Docker 多阶段构建 + docker-compose、GitHub Actions CI/CD
- **测试体系**：vendor / protocol / error / auth / admin / async_logger / queue / rate_limiter 等模块综合测试补齐（mockall / mockito）

### Changed

- **兄弟库迁移**（手写实现 → 自研库）：数据库层 sqlx → dbnexus；日志 tracing-subscriber → inklog；限流手写 → limiteron；缓存手写 → oxcache；TOML 解析手写 → confers；重试延迟 backon → limiteron::retry；熔断手写 → limiteron::circuit；7 个兄弟库经 `[patch.crates-io]` 统一身份，path 版与 registry 版不再并存
- **模块结构**：`mod.rs` 接口隔离（router / database / config / api / infrastructure 的实现迁移至独立文件）；收紧 lib 公共 API，main 改用 lib crate 公共 API
- **错误体系**：`ProxyError` → `NexusFlowError` 统一命名
- **工程化**：Cargo.toml 重构 + 包名统一（nexusflow）；cargo fmt 全量格式化；clippy `-D warnings` 零告警基线；版权头统一；仓库卫生（.gitignore 加固、移除 SAST 产物 / AGENTS.md / 工具产物）；package metadata 补充（repository / keywords / categories）
- **35 项优化落地**（Round 1 工程 16 项 + Round 2 算法 19 项）：配置热更新地基（confers dynamic + snapshot，原子写 + 校验 + 热读）、认证中间件挂载与 SHA256 摘要 key 存储、跨协议 SSE 流式、会话亲和、精确响应缓存、TTFT/e2e 延迟分道 + Peak-EWMA + min+buffer 候选集、失败率软降权（429 感知）、重试放大守门器 + 单请求时间预算、SLA 虚拟队列、LinUCB 评分、LPS 延迟前瞻、滑动窗口限流选项、缓存 TTL 负载联动、token 加权并发票据——**全部新算法默认关闭，基线行为逐位一致**（结构性断言锁定）
- **文档**：Phase 5 文档与发布

### Fixed

- 熔断慢调用误触发：慢调用判定线 500ms → 60s + rate 1.0（LLM 成功调用常态超 500ms，混合流量下熔断与真实失败无关）
- 重试惊群：Retry-After 与 5xx 固定间隔分支加同步展宽抖动，消除多实例同刻重试
- 跨午夜 time_window 窗口计算错误
- 出站三层超时缺失（per-attempt / 流式 TTFT / 流式 idle——此前上游挂起可无限占用并发 permit）
- `[rate_limit]` 配置段被 serde 静默丢弃（限流参数从未生效，恒跑库内置默认规则）
- 熔断 / 重试 / 换路覆盖全部 4 条执行路径（此前仅转换非流式路径有换路 fallback）
- embeddings 端点重新纳入厂商治理
- diting 审查发现的安全关键问题与 Phase 3 审计发现
- doctest 路径错误、重复 mod 声明、retry_test 适配新 utils API
- 死代码与孤儿代码清理（ConverterRegistry、sdforge 孤儿 handler、`#[allow(dead_code)]` 注解）；URL + header 辅助函数提取消除 4 处重复
- `--no-default-features --tests` 构建修复

---

[📖 README](../README.md) · [🤝 贡献指南](./CONTRIBUTING.md) · [🔒 安全文档](./SECURITY.md)
