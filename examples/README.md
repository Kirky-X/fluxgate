# NexusFlow 示例集（examples/）

`examples/` 是 NexusFlow 各种使用方式的样例集合：从裸 HTTP 直连、Rust SDK 客户端、库形态嵌入到 gRPC 门面调用，覆盖网关的四种典型接入形态。四个 crate 均为 workspace 成员（根 `Cargo.toml` 的 `[workspace].members`），且全部声明 `publish = false`——禁止发布，不会随任何 crate 发布流出。

## 总览

| 目录（crate 名） | 演示形态 | 语言 | 依赖的 nexusflow feature |
| --- | --- | --- | --- |
| [`rest-http/`](rest-http/)（nexusflow-example-rest-http） | 裸 HTTP 直连网关，不经 SDK：健康检查 → 非流式 chat（`X-Vendor`/`X-Model` 提示头）→ Anthropic 格式 `/v1/messages`（协议互转）→ SSE 流式（`bytes_stream` 手工解析 `data:` 行与 `[DONE]`）；curl / Python（openai 库）/ Node.js（openai npm 包）三语言对照组齐备 | Rust / bash / Python / JavaScript | 无（不声明 nexusflow 依赖） |
| [`sdk-usage/`](sdk-usage/)（nexusflow-example-sdk-usage） | 四个自包含 bin（chat / stream_chat / embed / admin）：SDK 客户端调用已部署网关，覆盖完整 `ChatRequest` + `RequestConfig` 路由提示头、SSE 流式逐 chunk 消费、embedding 向量、health / vendors / models 运维端点 | Rust | `sdk`（`default-features = false`） |
| [`embedded-server/`](embedded-server/)（nexusflow-example-embedded-server） | 库形态嵌入宿主应用：`standalone` 一行 `run()` 起服、阻塞到停机；`mount_router` 经 `build_app` 拿 `Router` 后 merge 宿主路由（`GET /examples-info`）自行 bind/serve | Rust | 默认（`full`） |
| [`grpc-client/`](grpc-client/)（nexusflow-example-grpc-client） | tonic gRPC 客户端：`build.rs` 从 vendored proto 生成 `sdforge.v1` 客户端 stub，演示 GetInfo 免认证、Call 无凭据认证门（`Unauthenticated`）、Call 带 `x-api-key` 凭据（`NotFound` + UnifiedError 细节），三路径均在真实网关端到端实测 | Rust（构建期代码生成） | 无（不依赖 nexusflow 本体；网关侧需 `grpc` feature） |

---

## rest-http — 裸 HTTP 直连网关

**是什么**：不经过 SDK，直接对网关线级协议编程。`src/main.rs` 用 reqwest 依次跑四步：健康检查 → 非流式 chat（`X-Vendor`/`X-Model` 提示头）→ Anthropic 格式 `/v1/messages`（验证协议互转）→ SSE 流式（`bytes_stream` 手工解析）。curl / Python / Node.js 三语言对照组与 Rust 版一一对应，任何语言都可对照移植。

**依赖**：本示例不声明 `nexusflow` 依赖（裸 HTTP 不经 SDK，无 feature 需求）。第三方依赖全部 `default-features = false` + 显式 features：`reqwest 0.13 [json, stream]`、`tokio 1 [macros, rt-multi-thread]`、`futures-util 0.3 [std]`、`serde_json 1 [std]`、`anyhow 1 [std]`。版本对齐根 `Cargo.toml` / `Cargo.lock`（reqwest 0.13.1 等）以复用构建缓存。

**前置条件**：

1. **启动网关**：仓库根 `cargo run --release`（默认监听 `127.0.0.1:8080`，取 `[server]` 段）。
2. **配置 API key**：dev 配置 `[auth]` 开启且 `api_keys` 为空——不配 key 则所有 `/v1` 与 `/admin` 请求一律 401。先执行 `echo -n '你的key' | sha256sum`，把摘要填进 `config.toml` 的 `[auth].api_keys`，再 `export NEXUSFLOW_API_KEY=明文key`。
3. **上游就绪**：仓库自带 `config.toml` 的 mock 厂商（`mock-openai` / `mock-anthropic`）上游指向 `http://127.0.0.1:8001/v1`，需该地址有 OpenAI/Anthropic 兼容服务；换真实厂商时，同步修改各文件顶部的 `MODEL` / `VENDOR` / `ANTHROPIC_MODEL` 常量。

**运行**（`NEXUSFLOW_BASE_URL` 缺省 `http://127.0.0.1:8080`）：

```bash
# 仓库根执行
cargo run -p nexusflow-example-rest-http
NEXUSFLOW_API_KEY=你的key bash examples/rest-http/curl_samples.sh

# examples/rest-http 目录内执行（Python / Node 按需 pip install openai / npm install openai）
NEXUSFLOW_API_KEY=你的key python3 python_client.py
NEXUSFLOW_API_KEY=你的key npm install openai && NEXUSFLOW_API_KEY=你的key node node_client.js
```

**文件清单**：

| 文件 | 内容 |
| --- | --- |
| `src/main.rs` | reqwest 直连四连（健康检查 / 非流式 chat / Anthropic 格式 / SSE 手工解析） |
| `curl_samples.sh` | 各端点 curl 命令，含 `X-Cache: MISS→HIT` 与 `X-No-Cache: 1` 对照演示 |
| `python_client.py` | 官方 openai 库用法（`base_url` 指向网关 `/v1`，非流式 + 流式） |
| `node_client.js` + `package.json` | openai npm 包同款用法（ESM） |
| `Cargo.toml` / `README.md` | 独立示例成员清单与运行文档 |

**协议事实与勘误**（编写时以源码核实）：

- `GET /v1/models` 未在网关注册（服务端路由表 `src/api/router_builder.rs:360-377` 无此路由），请求会 404；真实模型列表是 `GET /admin/models`。SDK 的 `NexusFlowClient::models`（`src/sdk/admin.rs:42-46`）引用了这条不存在的路由，属 SDK 与服务端的既有偏差。
- `X-Vendor`/`X-Model` 仅 `/v1/chat/completions` 消费（`src/api/openai.rs:83-95`）；`/v1/messages` 的 handler 只读 `X-Weight`，路由只认 body 的 `model`。
- SSE 错误帧 `event: error` 之后网关不补 `[DONE]`（`src/api/openai.rs:592-624`、`src/protocol/sse_frame.rs:25-46`），客户端解析器按失败终止，不要等待 `[DONE]`。

---

## sdk-usage — Rust SDK 客户端调用已部署网关

**是什么**：四个自包含 bin 演示 `nexusflow` 的 `sdk` feature（Rust HTTP 客户端）如何调用已部署网关：`chat` 非流式（完整 `ChatRequest` + `RequestConfig` 路由提示头，打印 token 用量与实际 vendor）、`stream_chat` 流式（`chat_stream` 逐 chunk 消费 SSE）、`embed` embedding（向量维度与用量）、`admin` 运维（health / vendors / models）。

**依赖**：`nexusflow = { path = "../..", default-features = false, features = ["sdk"] }`；`tokio = { version = "1", default-features = false, features = ["rt-multi-thread", "macros"] }`、`futures = { version = "0.3", default-features = false, features = ["std"] }`。版本声明对齐仓库根 `Cargo.toml`，`Cargo.lock` 解析 tokio 1.53.1 / futures 0.3.32 以复用缓存。

**前置条件**：

1. 已启动 NexusFlow 网关，并 `export NEXUSFLOW_BASE_URL`（如 `http://127.0.0.1:8080`）与 `NEXUSFLOW_API_KEY`（网关签发的 key）。任一变量缺失或为空时，示例直接报错退出，不带空值发请求。
2. 两个关键点（示例 README「说明」节已注明）：
   - SDK 类型的导入路径是 `nexusflow::sdk::{ChatRequest, NexusFlowClientBuilder, NexusFlowSdkError, RequestConfig, ...}`，**不在 crate 根**——crate 根的 `NexusFlowError` 是网关自身错误类型，勿混用。
   - 示例模型名 `gpt-4` / `text-embedding-3-small` 与 `vendor_hint = "mock-openai"` 取自仓库 `config.example.toml` 与 SDK 集成测试样例值，运行时按自己网关实际配置的 vendor/model 名称调整。

**运行**：

```bash
cargo run -p nexusflow-example-sdk-usage --bin chat         # 非流式 chat
cargo run -p nexusflow-example-sdk-usage --bin stream_chat  # SSE 流式
cargo run -p nexusflow-example-sdk-usage --bin embed        # embedding
cargo run -p nexusflow-example-sdk-usage --bin admin        # health / vendors / models
```

**文件清单**：`src/bin/chat.rs`、`src/bin/stream_chat.rs`、`src/bin/embed.rs`、`src/bin/admin.rs`、`Cargo.toml`、`README.md`。

---

## embedded-server — 库形态嵌入宿主应用

**是什么**：`nexusflow` 既能独立进程部署，也能作为库装配进宿主程序。两个 bin 演示两种形态：

- `standalone`：`server::run(Some("config.toml".into()))` 一行起服，阻塞到停机（信号停机与 flush 收尾由 `run()` 内部承担）。
- `mount_router`：`build_app` 拿到网关 `Router` 与 `ServerHandle`，merge 宿主自有路由（`GET /examples-info`）后自行 bind/serve，Ctrl+C 退出路径调用 `handle.shutdown()`。

**依赖**：`nexusflow = { path = "../.." }`（默认 `full`）；`tokio = { version = "1", default-features = false, features = ["rt-multi-thread", "macros", "net"] }`、`axum = { version = "0.8", default-features = false, features = ["macros"] }`（版本与 features 对齐根 `Cargo.toml:57-58` 现有行，locked axum 0.8.9 / tokio 1.53.1）、`serde_json = { version = "1", default-features = false, features = ["std"] }`。按约束只声明最小 features（axum 仅 macros、tokio 无 signal）：`mount_router` 用到的 axum `json`/`tokio`/`serve` 与 tokio `signal` 由 nexusflow 依赖侧默认特性经 Cargo 特性联合补齐（该 crate `Cargo.toml` 注释已标明）。

**前置条件**：运行目录（CWD）须有 `config.toml`，可从仓库根 `config.example.toml` 复制为起点；无配置或配置非法直接非零退出，无默认配置回退。监听地址取 `[server]` 段的 `host` / `port`。

**运行**：

```bash
# 仓库根执行
cargo run -p nexusflow-example-embedded-server --bin standalone
cargo run -p nexusflow-example-embedded-server --bin mount_router

# mount_router 启动后验证宿主路由与网关共存（端口取 [server].port）
curl http://127.0.0.1:8080/examples-info
```

**与独立部署的差异**（README「与独立部署的差异」节详述）：

- 嵌入形态下进程生命周期归宿主持有：`standalone` 整体交给 `run()`，`mount_router` 由宿主自行 serve 并收尾。
- 退出前必须调用 `ServerHandle::shutdown().await`，否则 flush worker 泄漏且缓冲日志丢失（`standalone` 的 `run()` 内部已处理）。
- 同进程 `run` / `build_app` 合计只能调用一次——内部全局态（OnceCell）二次写入即 panic。
- 默认 `full` feature 不含 gRPC：需要时自行启用 `grpc` feature，并经 `ServerHandle::take_grpc_server` 取出启动。

**文件清单**：`src/bin/standalone.rs`、`src/bin/mount_router.rs`、`Cargo.toml`、`README.md`。

---

## grpc-client — tonic gRPC 客户端

**是什么**：`build.rs` 经 tonic-prost-build 0.14 从 vendored proto（`proto/sdforge.v1.proto`，sdforge 仓库同名文件的逐字节副本）生成 `sdforge.v1.SdForgeService` 客户端 stub；`src/main.rs` 依次演示三条路径：GetInfo 免认证直取服务信息、Call 无凭据撞认证门（`Unauthenticated`）、Call 带 `x-api-key` 凭据（库存网关终点 `NotFound`，gRPC Status details 携带 UnifiedError JSON）。三路径均在真实网关端到端实测。

**依赖与 feature**：本样例不依赖 nexusflow 本体（客户端只消费 proto wire 契约），故无 nexusflow feature 声明；演示的网关侧配置是 nexusflow 的 `grpc = ["dep:sdforge", "sdforge/grpc"]`（根 `Cargo.toml:45`，独立门控不进 `full`），以 `cargo build --features grpc` 启用。样例自身依赖（均 `default-features = false`）：`tonic 0.14 [transport, codegen]`、`tonic-prost 0.14`、`prost 0.14 [derive]`、`tokio 1 [rt-multi-thread, macros]`；build-dependencies 与 sdforge build.rs 完全同套：`tonic-build 0.14` + `tonic-prost-build 0.14`。版本与 NexusFlow `Cargo.lock` 对齐（tonic / tonic-prost / tonic-build 0.14.6、prost 0.14.4），零新增外部包。

**前置条件**：

1. `protoc` 在 PATH（网关侧 grpc feature 与本样例 build.rs 的共同构建前置）。
2. 网关侧启用三步：`--features grpc` 编译启动、`config.toml` 加 `[grpc]` 段且 `enabled = true`（默认端口 50051）、`[auth].api_keys` 存 API key 的 SHA256 hex 摘要；配置片段见 [`grpc_config_snippet.toml`](grpc-client/grpc_config_snippet.toml)。

**运行**：

```bash
NEXUSFLOW_API_KEY=your-api-key cargo run -p nexusflow-example-grpc-client
```

**三点注意**（示例 README 已如实记录，均以代码为准）：

1. 仓库 `config.toml:341` 注释称 GetInfo 也要求 key，与代码行为不符——sdforge `src/grpc/grpc_impl.rs` 的 `get_info` 不经 `authenticate`，NexusFlow `src/server/grpc.rs:19-21` 模块文档确认 **GetInfo 免认证**。
2. 库存网关未注册任何业务 `grpc_method`，Call 的预期终点是 `NotFound`；把 `DEMO_METHOD` 换成真实方法名即得 `success = true` + JSON `data`。
3. 取舍：样例不依赖 nexusflow 本体，避免把整个网关栈拖进客户端构建（nexusflow 无 gRPC 客户端 API）；代价是网关侧启用（feature + `[grpc]` 段）只能经本目录 README 与配置片段表达，而非依赖声明。

**文件清单**：`build.rs`、`src/main.rs`、`proto/sdforge.v1.proto`、`grpc_config_snippet.toml`、`Cargo.toml`、`README.md`。

---

## 约束说明

- **path 依赖布局**：nexusflow 的兄弟库 `confers`、`dbnexus`、`inklog`、`limiteron`、`oxcache`、`sdforge`、`trait-kit` 在仓库同级目录（根 `Cargo.toml` 的 `[dependencies]` 以 `path = "../<库名>"` 引用，`[patch.crates-io]` 统一身份）。克隆本仓库单目录无法编译，需同级放置这七个库（见根 README「环境要求」）。四个示例经 `path = "../.."` 引用 nexusflow，同为 workspace 成员，共享这一布局。
- **示例不随发布流出**：四个示例 crate 全部 `publish = false`，仅作为本地 workspace 成员存在；`cargo publish` 与任何发布流水线都不会带上它们。
- **根 README 未包含本目录入口**：根 `README.md` / `README_EN.md` 目前未列出 `examples/`，本文件是示例集的独立入口。
