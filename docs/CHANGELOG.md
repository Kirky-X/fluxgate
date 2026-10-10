# NexusFlow 更新日志

[🔒 安全文档](./SECURITY.md) · [🤝 贡献指南](./CONTRIBUTING.md) · [📖 用户指南](./USER_GUIDE.md) · [🏗️ 架构文档](./ARCHITECTURE.md)

本格式基于 [Keep a Changelog 1.0.0](https://keepachangelog.com/en/1.0.0/)，版本号遵循语义化版本。

> **注**：本项目尚未打 tag，以下版本对应 `Cargo.toml` 的版本线（当前 `version = "0.1.0"`）。

---

## [Unreleased]

### Changed

- **配额与分账成本档源单源化（可见数值漂移，升级前必读）**：配额（WP8）此前只读 `[[pricing]]` 且仅计 input/output
  两档，与成本分账（WP9）所用的 `[[model_catalog]]` 四档价是两个数——同一请求「写进数据库的成本」与「从配额窗口扣掉的
  成本」不一致，启用 prompt caching 的流量在成本窗被系统性低估。现统一走 `estimate_cost_from_catalog`（四档 +
  `prompt_cache_semantics` 同一判定），`[[pricing]]` 降为未登记目录模型的回退档（该路径数值与变更前一致）。
  升级后的**可见后果**：① 带 cache 用量的请求成本窗扣减变大（可能提前触发 429）；② 仅在 `[[model_catalog]]` 登记、
  未写 `[[pricing]]` 的模型开始参与成本扣减；③ 透传流扣减变小（回补生效，见 Fixed）。另将 cache 包含语义从三处内联
  分支（分账/CostTracker/配额，对未知协议方向相反）收敛为 `prompt_cache_semantics` 单点：仅 `openai` 为 Inclusive，
  其余 Exclusive（基数不缩减、计价保守）；两种真实协议（openai/anthropic）行为不变。
- **tests 目录模块化隔离**：平铺的 41 个集成测试文件按功能域迁入 `tests/<domain>/` 子目录（api / routing / resilience / config / database / protocol / vendor / cost / server / i18n / matrix / utils），解决根目录结构混乱；保持「一文件一测试二进制」不变（`server_run_*`/`grpc_smoke` 等进程独占语义不破坏）——根 `Cargo.toml` 关闭自动发现（`autotests = false`）并逐文件显式登记 `[[test]]`（name = 原文件 stem，`--test` 过滤器与 CI 日志口径零漂移）；共享 helper 仍居 `tests/common/mod.rs`，消费方改 `#[path = "../common/mod.rs"] mod common;` 显式引入；本地在途的 `tests/dead_code_guard.rs`（死代码守卫，未跟踪）一并归入新域 `tests/guard/` 并同步登记，注册表与磁盘布局收敛为 42 二进制；用例数与覆盖行为改前逐位一致。新增测试登记纪律见 `tests/AGENTS.md` 与 `docs/CONTRIBUTING.md`。
- **配置面治理**：运行配置 `config.toml` 从版本库移出并加入 `.gitignore`（它承载部署私有物：API Key 摘要、vendor 直连地址与配额），`config.example.toml` 升为**随仓唯一的全量参考**——覆盖 `Config` 结构体的每个段与每个字段（补齐此前只在 `config.toml` 出现的 `[auth]`/`[cors]`/`[rate_limit]`/`[retry]`/`[routing]`/`[resilience]`/`[admission]`/`[cache]`，以及此前两边都缺的 `[docs]`、`[cache].ttl_jitter`/`null_cache_ttl_secs`、`[routing].price_factor_enabled`、`concurrency.max_concurrent_requests`、`models.abstract_id`、`api_key_envs`、配额三形态示例），显式值与 serde default 逐位一致（复制即得默认行为）；修正示例 `[[pricing]]` 引用不存在 vendor id 的旧漂移。守卫测试 `tests/config_toml_example_test.rs` 随之改指 `config.example.toml`（CI 干净检出无 `config.toml`，原断言链在 CI 上必然失败），新增键存在性断言与 `validate_config` 门禁（不只看 serde 能解析，id 去重/摘要形态/CB 窗口下限一并校验）。`README`（双语，顶层段计数 15 → 16 含 `docs`）/`docs/USER_GUIDE.md`/`docker/docker-compose.yml`/`examples/assets/*` 同步 `cp config.example.toml config.toml` 起步口径。
- **依赖卫生**：清理冗余依赖并收紧特性面——移除死依赖 `sys-info`（全仓零引用，`cc`+`libc` 的 C 编译面与 `links="info"` 约束随之从编译图消失）与 `tokio-util`（全仓零引用）；根 `anyhow`/`futures-util` 直接声明移除（根 crate 零引用，examples 自带 / 仅测试使用且 dev-dependencies 保留）；`hex`/`sha2`/`subtle`/`once_cell` 转 optional 挂 `web-server`，`url` 转 optional 挂 `database`/`sdk`（对应 feature 关闭时不再激活本 crate 的直接依赖）。`sha2` 0.10 → 0.11 对齐兄弟库（`Digest` 导入形态不变）；`sqlx` 侧 0.10 有两条上游锚点（`sqlx-core` 的 migrate 特性为当前编译面；`sqlx-macros-core` 的非 optional 依赖在解析图/Cargo.lock 中，本仓当前组合未开启其 macros feature 故不编译），其升级后双版本自然收敛。移除 inklog 的 `http`/`cli` 特性声明（本仓零调用；无 sdforge 的轻量组合省约 40 个包，full 构建省 axum-server/fs-err/glob 3 个——clap/schemars 经 sdforge 保留）。

- **结构**：`crates/llm-gate` 路由核心库并回主 crate（撤销 N1 workspace 拆分）——config/i18n/protocol/storage/utils/vendor/router 模块直接位于 `src/`，Fluent 目录 `locales/` 移至仓库根（`include_str!` 相对路径不变），llm-gate 集成测试并入 `tests/`（`gate_` 前缀，共享 helper 迁至 `tests/gate_common/`）；`resilience`/`protocol`/`vendor-management`/`router`/`cost-tracking` feature 由「转发 llm-gate 同名 feature」改为直控本 crate 模块与 optional 依赖（单开关语义不变）；`GateError` 定义移至 `nexusflow::error::gate`（顶层 `nexusflow::error::GateError` re-export 保留，`nexusflow::config` 等全部公开路径不变；`llm-gate` 从未发布 crates.io，无外部消费方）。

- **BREAKING**：Rust SDK（`sdk` feature）类型与 admin 面重塑对齐网关真实形状——`ChatMessage.content` 支持多模态内容件（`ChatContent`），`ChatRequest` 增 tools/tool_choice/vendor_extensions/penalties；`ChatResponse` 移除恒空的 `vendor` 字段（网关响应不注入）、finish_reason 结构化为四变体枚举、usage 含 cache_read；`ChatStream` 解析 tool_calls 增量片段与 usage 尾帧；admin 失配修复——`models()` 改 `GET /admin/models`（原 `GET /v1/models` 网关未注册恒 404）、`health()` 改 `GET /api/v1/health`（原 `admin/health` 不存在）、`vendors()` 对齐运行面形状 `{id,status,queue_length,key_pool}`；新增 `upsert_model` / `delete_model`（目录 CRUD）。
- **BREAKING**：`GET /admin/models` 响应形状由 `GlobalModelConfig` 数组（原 global_model 单例包装）变更为 `ModelCatalogEntry` 数组（九字段模型能力目录：context_window / 四档单价（currency，每 1M tokens）/ currency / max_output_tokens / supports_tools / supports_thinking）。全局模型策略改经 `POST /admin/models/global`（存量契约保留）或 `GET /admin/config` 读写。

- **Docker 基线与工具链治理**：`docker/Dockerfile` 运行阶段 `alpine:3.21` → `alpine:3.24`（3.21 支持期至 2026-11-01），构建阶段 `rust:1.97-alpine` → `rust:1.99-alpine3.24`（Rust 无 LTS，1.97 已退出官方镜像重建窗口，base 冻结在 2026-07-16）。两个 FROM 的 alpine 后缀统一到 3.24（保留同一分支），并在运行阶段加 `ldd /bin/server` 回验门禁（实测两类链接失败均以 `Error` 开头：缺库报 `Error loading shared library`、版本偏旧报 `Error relocating … symbol not found`）。验证同时修正一个误判：本仓 musl 产物实为**全静态**（`ldd` 仅留 `ld-musl` 一条，sqlite 已静态内嵌），因此不存在原先推断的“构建 3.53 头 / 运行 3.48 库”符号错配风险（那个结论对 glibc 发布产物成立、对 musl 镜像不成）；门禁保留作回验，一旦改为动态基座或引入 OpenSSL 绑定即会拦截。基础镜像写全字面量 tag 而非 `${ARG}` 拼接（dependabot-core 的 docker 解析器只对 FROM 行做正则匹配、不展开 ARG，拼接写法脱离自动跟踪），`.github/dependabot.yml` 同步新增 `docker` 生态位每周跟踪。冗余 TLS 声明移除：构建阶段 `libressl-dev`（在 3.24 属 community 仓）、运行阶段 `libssl3`/`libcrypto3`（`Cargo.lock` 无 `openssl`/`openssl-sys`/`native-tls`，TLS 栈为纯 rustls）；实测提醒：后两包随 alpine:3.24 基础镜像自带（apk-tools/musl-utils 依赖链），从 apk 清单移除不省体积，只是不再声称应用需要它。`ca-certificates` 保留供 `rustls-native-certs` 读根证书，sqlite/sqlite-libs 保留供容器内用 sqlite3 CLI 查库。compose 侧：`image` 由 `:latest` 改 `nexusflow:${NEXUSFLOW_IMAGE_TAG:-0.1.0}` 并透传 `APP_VERSION` 标签、SQLite 持久化由命名卷 `driver_opts + ${PWD}/data` 改为 `../data:/app/data`（按 compose 文件目录解析，不再依赖调用 shell 的 cwd）、健康检查收敛为镜像层唯一定义（原 Dockerfile `start-period` 5s 与 compose 10s 漂移且后者静默覆盖）、删除写死子网 `172.28.0.0/16`（与宿主网络重叠即启动失败）。工具链基线：MSRV 下限 `rust-version = "1.97.1"` 提升到根 `Cargo.toml` 作单一事实来源（原先只散落在注释里，导致 `release.yml` 漂到 1.92 而无门禁拦截——该值低于兄弟库 MSRV，cargo 在依赖解析期即拒绝构建，已改 1.99 与 Dockerfile 对齐；`ci.yml` 保留 1.97 下限以固定 clippy lint 集合），并修正两处 `dtolnay/rust-toolchain` pinned SHA 的版本注释（该 commit 实为 action `v1`，原 `# 1.92`/`# 1.97` 注释均已失效）。README 双语与 `docs/{USER_GUIDE,CONTRIBUTING,SECURITY,FAQ}.md` 的版本、镜像包列表、健康检查、发布产物口径同步更正；`.env.example` 新增 `NEXUSFLOW_IMAGE_TAG` 说明。

- **Docker 构建存量缺陷（本轮端到端跑 `docker build` 暴出）**：构建阶段的 bind mount 从未包含 `examples`，而根 `Cargo.toml` 已把它列为 `[workspace]` 成员，cargo 加载 workspace 即在解析期报 `failed to load manifest for workspace member /app/examples`——补上 `examples` 挂载（只需成员 manifest，不编译示例）。验证：以本地暂存源码代替远端 fetch 跑通全链路，`rust:1.99-alpine3.24` 下 `cargo build --locked --release` 编译通过（683s），镜像 78.9 MB，`alpine:3.24` 运行阶段 `ldd` 门禁、`addgroup/adduser` 短选项、非 root `appuser` 均正常，容器启动后 `GET /api/v1/health` 返回 `{"status":"ok","version":"0.1.0"}`（日志：`logs/verify-docker-build3.log`）。

- **SIBLING_PINS 刷新到兄弟库已推送提交**：上述构建又暴出第二层存量问题——`inklog=60ab3972` 等 pin 没本仓 Cargo.toml 要求的 `secret-scan` feature，解析报 `failed to select a version for inklog`（pin 早在兄弟库注释清理 sweep 前就写定，已落在本仓需求之后）。现按不变式重钉 7 个 SHA（均取各仓 `origin/main`，非本地未推送提交），并在 Dockerfile 把「不变式 + 一行生成/校验命令」写成可执行口径，避免下次再靠人肉对。已验证：七个 pin 全部 `HEAD == origin/main` 且工作树干净（sdforge 仅差 1 行注释，已核对为纯注释），各 pin 的包版本 == `Cargo.lock` 记录值，各 pin 的 `[features]` 包含本仓声明的全部特性（本仓向 inklog 声明 6 个、dbnexus 13 个、sdforge 10 个，七个库缺失均为 0）；`cargo check --locked --offline --release --bin nexusflow` 在这些源码状态上通过（日志：`logs/verify-cargo-check-pin.log`）。**未验证项**：`docker build` 的真实远端 fetch 因本轮环境到 auth.docker.io / github.com 均超时未能跑完（镜像层与 pin 内容已本地验证），网络恢复后用一行命令确认 SHA 在远端，再跑一次 `docker build -f docker/Dockerfile .` 即可闭环（也意味着 ci.yml 里「纯远端构建不可行」的前置条件已具备）。

### Removed

- **配额余量响应头（规格 Out of Scope 复核移除）**：`[quota].expose_remaining_headers` 配置位与
  `x-nexusflow-quota-tokens-remaining` / `x-nexusflow-quota-cost-millicents-remaining` /
  `x-nexusflow-quota-window-secs` 三头、`QuotaState::remaining` 只读查询一并移除——quota-cost 规格的
  Out of Scope 明文排除「配额余量的响应头暴露（X-RateLimit-* 头，本期不做）」，原实现属超规格落地面。
  余量观测走 `/admin/cost` 聚合查询侧。随移除的还有注释中引用不存在的 `R-quota-cost-005` 编号
  （已改指真实编号 R-quota-cost-003）。

### Fixed

- **透传流式配额不回补（WP8 缺口关闭）**：`settle_stream` 之前只接在转换流路径，`chat_passthrough_stream`（入站协议
  = 出站协议，即 Claude Code 等直连形态）无任何结算点，按 `max_tokens` 高估的预扣会占满整个窗口（默认 3600s）。
  现由 `reframe_passthrough_stream_with_usage` 逐帧观测 `message_start`/`message_delta`（Anthropic）与 `usage` 尾帧
  （OpenAI）并在正常 EOF 时按实际回补；提数经 `parse_usage_detailed` 单源。帧字段由 `SseFrame` 直接 move（无额外
  拷贝），且仅在「该 key 确有任限额且配额开启」时挂包装——其余形态透传字节逐字节不变。断流/错误帧终止与全程无 usage
  （OpenAI 不带 `stream_options.include_usage`）仍不回补，该条件性缺口从「无 usage 可观测」的笼统声明改写为具体触发形态。
- **clippy 门禁与 MSRV 双事实源**：`clippy.toml` 的 `msrv = "1.75"` 与根 `Cargo.toml` 的 `rust-version = "1.97.1"` 冲突，clippy 报 `the MSRV in clippy.toml and Cargo.toml differ; using 1.75.0 from clippy.toml`——CI 的 `cargo clippy --workspace --all-targets -- -D warnings` 因此必红。危害不止噪声：MSRV 门控的 lint 按 1.75 计算，1.76+ 才建议的惯用法（`Option::inspect`、`ilog2`、`slice::mid` 等）被静默抑制，与仓库自述的「MSRV 单一事实源在 `Cargo.toml`」直接矛盾。修法为删往 `msrv` 键（clippy 改读包自己的 `rust-version`），并给 workspace 成员 `nexusflow-examples` 补上同值 `rust-version`——该 crate 原先无声明，移除 `msrv` 后其 lint 基线会回落到运行时 stable 而漂移。本地 1.97.1（与 ci.yml `RUST_VERSION: '1.97'` 解析同版）复跑两条 CI clippy 命令（`--workspace --all-targets -- -D warnings` 与 `-p nexusflow-examples --no-default-features --features grpc --all-targets -- -D warnings`）均零告警——抬高基线未触发任何新 lint（验证前先 `cargo clean -p nexusflow -p nexusflow-examples` 强制重 lint：cargo 不把 `clippy.toml` 纳入 fingerprint，不清理则复用缓存诊断、验证无效）。

- **feature 图隐式前提显式化**：`database` 补 `logging` 联动（async_logger 无条件消费 inklog）、`web-server` 补 `infrastructure` 联动（server/shutdown 用 trait-kit）——修复 `--no-default-features --features "database,db-sqlite"` / `"web-server,db-sqlite"` 组合编译失败（此前仅 `full`/default 组合可用；单独开 `database`/`web-server`/`logging` 仍按既有契约需随任一 db-* 后端组，inklog kit 守卫）。
- **feature 组合自洽**：`gate_vendor_test`（resilience）、`gate_price_factor_routing_test`（router）、`gate_vendor_key_pool_test`（vendor-management）补 cfg 门控，三个 `server_*_test` 补 `#![recursion_limit = "256"]`（既有递归深度问题，仅非 sqlite 后端 `--all-targets` 暴露），至此 9 个 feature 组合 `--all-targets` 全绿；特性矩阵文档口径、`docs/CONTRIBUTING.md` 验证命令与测试基线（367/764）、README 双语测试规模表、PR 模板同步更正；新增两条依赖链断言（database→logging、web-server→infrastructure）与 CI 单特性组合检查（database / web-server / logging + db-sqlite 三行）。
- **流式失败语义边界（WP1）**：边界取「尚未向客户端写出任何字节」——首字节前上游失败（429/连接错误/5xx）走换路链重派、客户端无感；首字节后断流禁止换路，客户端恰收一个 SSE error 帧（`data: {"error":{...}}` JSON 信封、`upstream_stream_interrupted` 稳定码、i18n 文案、不泄 vendor 拓扑）、无 `[DONE]`、内容不重放，该上游进入冷却（`circuit_open_timeout_ms` 窗口、TTL 惰性恢复）；限流只在请求入口裁决，已放行流中途不中断；非流式路径行为不变。锁定测试：`streaming_boundary_test` 6 用例（首跳 429/连接错误换路、断流单错误帧、冷却 TTL、转换路径 Anthropic/OpenAI 出站断流各一——错误帧后无 `message_stop`/`[DONE]` 收尾且内容不重放）+ `rate_limit_test` 入口裁决两用例（`stream_request_is_adjudicated_at_entry_only`、`admitted_stream_completes_while_entry_quota_exhausted`——cap=1 下在途流于窗口耗尽后仍完整收束）+ `fallback_test::cover_all_paths` 回归全绿；修复流式装配处 `Arc` 先 move 后借用的编译失败（cooldown 先快照再移交所有权，两路同修）。

### Added

- **评审修复（三路安全/架构/性能会审）**：V2 版本化迁移（`token_usage` 分账形态从 V1 UP 拆出为独立 V2，存量 V1 库
  原地升级保数据——修复 V1 FROZEN 被原地改写与升级路径缺失）；配额中间件限额判定前置（非适用路径与无限额租户零缓冲
  零解析，读体失败显性 413）；转换流结算三级早退（默认配置零克隆）；缓存 HIT 请求按 0 结算回补预扣（重复命中不再
  虚耗配额）；`/admin/cost` 的 key 过滤参数改 `x-nexusflow-cost-key` 请求头传递（明文 key 不再进 URL/访问日志）；
  健康探针任务常驻（`mode`/`interval_secs`/`probe_strategy` 全部真实热生效）；`/admin/route/explain` 补 key 池粒度
  （全 key 黑名单报 `cooling_down`）；账本回补不建账且与预扣共用淘汰谓词；透传流帧 usage 提取加廉价预筛；探针出站
  复用共享 Client 并计入 vendor RPS/并发限流；`AttemptTrail` 分配随响应头开关门控；usage 提数/key 摘要辅助下沉
  utils·protocol（middleware 不再反向依赖 api）；`[quota]`×`[auth]` 关闭组合的启动告警。见 Removed（余量响应头）
  与 docs/SECURITY.md（admin 暴露面、无盐摘要、key 熵）。另校准 `extra_headers` 标注：出站构造每请求现读配置，
  实际 ✅ 热生效（早期「需重启」标注不实，行为以实现为准）。
- **供应商出站自定义头（WP2）**：`[[vendors]].extra_headers`（TOML 表 string→string，静态值、不做模板替换，默认空表零行为变化、✅ 热生效——评审校准：早期 restart-required 标注不实）——请求构造处统一注入；与网关已设置的头（Authorization/x-api-key/Content-Length/Host 等鉴权与协议头）冲突时跳过该项并记 warn，不覆盖。锁定测试：出站构造单测（注入/冲突跳过/空表不变）+ mockito 集成（出站请求头匹配）+ `config.example.toml` 守卫同步。
- **数据面端点补齐（WP3）**：`GET /v1/models`（OpenAI list 格式；模型目录 ∪ vendor 配置模型去重、按 id 排序；鉴权；始终现算、不缓存）与 `POST /v1/messages/count_tokens`（入参 model/messages/system?/tools?，返回 `input_tokens`；字符数/4 + 每消息结构开销近似估算、确定性；缺参 400 `validation_error` 复用既有 i18n 键）。实现为 `src/api/` 独立 handler 文件（`models.rs`/`count_tokens.rs`，注册于 `router_builder.rs`）；估算器进 `src/utils/tokens.rs` 供 WP8 配额预扣复用。锁定测试：`tests/api/` 成功/未鉴权/参数错误三路径 + 去重 + 不缓存；README（双语）/API_REFERENCE/USER_GUIDE 同步。
- **回退链可观测响应头（WP4）**：`[resilience].expose_fallback_headers`（默认 false、✅热更；关闭时响应与现状逐字节一致）——开启时成功响应附 `x-nexusflow-vendor`（最终命中）、`x-nexusflow-model`、`x-nexusflow-attempts`（总尝试次数）、`x-nexusflow-fallbacks`（逗号分隔尝试链，含最终命中者；流式在响应头阶段写入）。尝试链由 `FallbackChain` 配套的 `AttemptTrail` 记录（两换路循环填充，单跳直连 attempts=1；缓存命中早退不附头——无实际尝试）。锁定测试：单跳直连与首跳失败换路 × 流式/非流式 + 开关关闭无头；`config.example.toml` 守卫同步。
- **路由可解释性（WP5）**：`POST /admin/route/explain`（管理员鉴权、纯只读：不发上游请求、不改亲和/熔断/队列状态；未知模型 400 `validation_error` 复用既有 i18n 键）。逐候选 `{vendor, model, decision, reason_code, detail}`，reason 优先级 not_in_catalog → disabled → throttled → cooling_down → affinity_bound_elsewhere → queue_over_limit → weight_zero → eligible；入选者附 5 因子权重分解（与 `compute_weight` 同一纯函数）与 selection 档位（weighted/linucb/power_of_two）。候选筛选与权重计算抽为可复用函数（`capacity_shares`/`queue_len_for`/`resolve_effective_model_weight`/`compute_weight_breakdown`，`compute_weight` 委托分解函数；行为不变，路由测试全绿）。锁定测试：各 reason_code + 分解自洽 + 解释与路由同一方法权重一致 + 只读性；API_REFERENCE/USER_GUIDE 同步。
- **密钥池三态（WP6）**：错误分类驱动处置——瞬时错误（429/超时/5xx/网络）维持现状（冷却 TTL 自动恢复、429 退避不变）；永久错误（401/402/403 及正文明确命中 `insufficient_quota`/`expired` 等账务形态）使该 key 进黑名单（不参与轮换、不随时间恢复，池内其余 key 继续服务；全黑 ≡ 池耗尽回退）。恢复通道：配置热更按 key 集合变化裁剪 + `POST /admin/vendors/{id}/reset` 语义扩展（清空黑名单含游标与冷却状态，未知 vendor 400；原 no-op 文案键已删除，新 `admin-vendor-reset` 双语同步）。分类器进 `error_map.rs`（`classify_key_error`，429 文案裸 quota 不误伤）；游标扫描跳过黑名单（轮换语义与现状逐位一致）。锁定测试：池单测（跳过/全黑回退/热更裁剪/重置）+ 分类单测 + 集成（401 黑名单与他 key 服务、reset 恢复、429 同 key 复用回归）+ reset 端点；API_REFERENCE 同步。
- **主动健康检查探针（WP7）**：`[health]` 段（mode=off|suspect_only|all，默认 off 零后台任务；interval_secs=300、timeout_ms=5000、probe_strategy=free|minimal_call 默认 free）+ `src/vendor/probe.rs` 独立模块（并发上限 2）。free 零成本 GET 上游模型列表端点（游标 key 认证），成功仅清 vendor 级冷却；minimal_call 以 max_tokens=1 计费请求验证黑名单/当前 key，成功额外解黑名单、清冷却、重置其名下模型级熔断计数；失败保持现状（debug 日志）。后台任务挂 bootstrap（**常驻**——评审 M8 修正：原「off 不 spawn」的一次性 spawn 使 mode 热改不生效，与 ✅ 热生效标注不符；现每周期热读 mode，off 为 no-op），句柄随 Assembly 移交 ServerHandle，`shutdown()` 首阶段 abort（随优雅停机退出）。锁定测试：三模式 + 黑名单恢复端到端 + 失败保持 + `config.example.toml` 守卫同步。
- **成本与 token 维度配额（WP8）**：`[quota]` 段（enabled=false 默认关零行为变化、window_secs=3600、est_chars_per_token=4；per-key 限额放 `[[auth.keys]]` 新增可选 `token_quota` 与 `cost_quota_millicents`，默认 0=不限；与 rate_limit/admission 独立叠加）+ `src/middleware/quota.rs` 独立模块（实现不进 mod.rs）。预扣 = 预估输入 token×输入单价 + max_tokens×输出单价（count_tokens 同口径估算；单价取 `[[pricing]]` 同模型跨 vendor 最高价，入口 vendor 未定故保守；`src/vendor/cost.rs` 同表）；非流式响应后、转换流式结束后按实际 usage 回补差额（可返还可追扣；透传流无 usage 保持预扣，失败请求不返还）；超限 429 + Retry-After + 新 i18n 文案；接入 `router_builder.rs`（认证后、端点限流前）。计数原语约束：limiteron 未暴露 keyed (key, cost) 原子消费与 refund（`check_and_consume` 私有、`check_retry_after` 固定 cost=1），故账本以 std 原语实现固定窗口精确消费/回补（单实例内存、10k key 上限，待上游暴露后迁移）。回补与入口共用同一预扣函数（无票据传递、不一致即 bug）。锁定测试：`tests/resilience/quota_test.rs` 五场景（入口 429、响应回补、流结束回补、窗口翻转、流中不中断 + 结束回补）+ 账本单测；`config.example.toml` 守卫同步。
- **成本分账落库（WP9，评审后改版本化 V2 迁移）**：`token_usage` 新形态经独立 V2 版本化迁移落地（V1 FROZEN 不动；
  全新库自动应用 V1+V2；存量 V1 库原地升级——增列 + UNIQUE 键换绑 + 旧行映射 prompt→input、completion→output、
  cost_usd_sum 整额入 input_cost、api_key_hash 回填空串，旧数据不丢；三后端同语义；另补 /admin/cost 的
  `(date)` 与 `(api_key_hash, date)` 查询索引）：`(vendor, model, api_key_hash, date, hour)` UNIQUE 聚合，四类 token（input/output/cache_read/cache_creation）与分项成本各自累加；`api_key_hash` 为请求方 key 的 SHA-256 摘要（API 层透传，不复用 key_pool 进程随机盐指纹）；写入补齐 cache 两类入账（`model_catalog` 四档单价，未登记记 0）；`GET /admin/cost?from=&to=&group_by=vendor|model`（可选 api_key 原文过滤，管理员鉴权，空窗返回 []）。`UsageSink` 改 `record_keyed` 单写入路径；`/admin/stats/tokens` 旧字段由新列映射（形状兼容）。锁定测试：表结构断言 + 分账聚合/过滤/窗口 + 旧字段映射 + 端点（分组/过滤/空窗/400/401）；API_REFERENCE/USER_GUIDE 同步。
- **一致性测试套件（WP10）**：`tests/protocol/` 新增 SSE 完整性对账（Anthropic：message_start 起始、起止按 index 配对连续、delta 不出轨、message_delta 携 usage、message_stop 终止；OpenAI：首 chunk role、finish 恰一次、id/created 一致、`[DONE]` 终止；单元驱动编码器 + 全链路 mock）与协议保真参数化回归（tool_choice/tool_result/system/采样/thinking/image）。先红后绿修复三处：OpenAI `tool_choice` any/tool 降级 auto 改 required/具名（旧单测断言同步更新）、tool_result 同消息文本块丢失改顺序并入、OpenAI `stop` string 形态丢弃改双形态；Anthropic 编码器文本/工具块序号复用改闭合递增；新增 `OpenAIStreamEncoder`（首块 role + 流 id/created 一致，handler 接入）。锁定测试 13 项全绿，既有协议/集成/SDK 套件回归全绿。
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
