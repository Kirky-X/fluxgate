# Fluxgate 示例集（examples/）

`examples/` 是 Fluxgate 各种使用方式的样例集合：从裸 HTTP 直连、Rust SDK 客户端、库形态嵌入到 gRPC 门面调用，覆盖网关的四种典型接入形态。

拆分前这里是四个各自独立的 workspace 成员 crate；现已合并为**单一 crate `fluxgate-examples`**（本目录 `Cargo.toml`），示例逻辑按功能收拢为 lib 模块（`rest_http` / `sdk_usage` / `embedded_server` / `grpc_client`），每个模块由各自 feature 门控，`src/bin/` 下保留八个二进制入口点。本 crate 是 workspace 成员（根 `Cargo.toml` 的 `[workspace].members`），并声明 `publish = false`——禁止发布，不会随任何 crate 发布流出。

> 注意：根包 `fluxgate` 已设 `autoexamples = false`，因为 `examples/` 现在是独立成员 crate 的目录，不再充当根包的示例约定目录。

## Feature 与模块划分

| feature（默认开？） | lib 模块 | bin 入口 | 演示形态 | 依赖的 fluxgate feature |
| --- | --- | --- | --- | --- |
| `rest`（默认） | `rest_http` | `rest_http` | 裸 HTTP 直连网关，不经 SDK | 无（不声明 fluxgate 依赖） |
| `sdk`（默认） | `sdk_usage`（chat / stream_chat / embed / admin） | `chat` / `stream_chat` / `embed` / `admin` | SDK 客户端调用已部署网关 | `sdk`（`default-features = false`） |
| `embedded`（默认） | `embedded_server`（standalone / mount_router） | `standalone` / `mount_router` | 库形态嵌入宿主应用 | `full` + `db-sqlite` |
| `grpc`（非默认） | `grpc_client` | `grpc_client` | tonic gRPC 客户端调用门面 | 无（不依赖 fluxgate 本体；网关侧需 `grpc` feature） |

`default = ["rest", "embedded", "sdk"]`，刻意不含 `grpc`：gRPC 代码生成需 `protoc`，与根 `Cargo.toml` 把 `grpc` 隔离在 `full` 之外同理。`build.rs` 用 `CARGO_FEATURE_GRPC` 门控，未启用 `grpc` 时绝不调用 `protoc`，因此普通 `cargo check/build` 无需 protoc。

各 bin 的 `required-features` 与上表一一对应：未启用相应 feature 时该 bin 被自动跳过，从而保住拆分前各 crate 独立的依赖足迹——例如 SDK 客户端可用 `--no-default-features --features sdk` 得到不编译网关服务器的轻量构建。

## 运行总览

```bash
# 默认 features 即覆盖 rest + embedded + sdk
cargo run -p fluxgate-examples --bin rest_http                       # 裸 HTTP 直连
cargo run -p fluxgate-examples --bin chat                            # SDK 非流式 chat
cargo run -p fluxgate-examples --bin stream_chat                    # SDK SSE 流式
cargo run -p fluxgate-examples --bin embed                          # SDK embedding
cargo run -p fluxgate-examples --bin admin                          # SDK 运维端点
cargo run -p fluxgate-examples --bin standalone                     # 库形态一行起服
cargo run -p fluxgate-examples --bin mount_router                   # 库形态挂载宿主路由

# 轻量 SDK 构建（不拉起网关服务器本体）
cargo run -p fluxgate-examples --no-default-features --features sdk --bin chat

# gRPC 客户端（需 protoc；追加 grpc feature）
cargo run -p fluxgate-examples --features grpc --bin grpc_client

# 非 Rust 对照组（在 examples/assets 目录内）
FLUXGATE_API_KEY=你的key bash examples/assets/rest-http/curl_samples.sh
FLUXGATE_API_KEY=你的key python3 examples/assets/rest-http/python_client.py
cd examples/assets/rest-http && npm install openai && FLUXGATE_API_KEY=你的key node node_client.js
```

---

## rest-http — 裸 HTTP 直连网关（`mod rest_http`）

**是什么**：不经过 SDK，直接对网关线级协议编程。`src/rest_http.rs` 用 reqwest 依次跑四步：健康检查 → 非流式 chat（`X-Vendor`/`X-Model` 提示头）→ Anthropic 格式 `/v1/messages`（验证协议互转）→ SSE 流式（`bytes_stream` 手工解析）。curl / Python / Node.js 三语言对照组（`assets/rest-http/`）与 Rust 版一一对应，任何语言都可对照移植。

**依赖**：`rest` feature 不声明 `fluxgate` 依赖（裸 HTTP 不经 SDK）。第三方依赖全部 `default-features = false` + 显式 features：`reqwest 0.13 [json, stream]`、`tokio 1`、`futures-util 0.3 [std]`、`serde_json 1 [std]`、`anyhow 1 [std]`，版本对齐根 `Cargo.toml` / `Cargo.lock` 以复用构建缓存。

**前置条件**：

1. **启动网关**：仓库根 `cargo run --release`（默认监听 `127.0.0.1:8080`，取 `[server]` 段）。
2. **配置 API key**：dev 配置 `[auth]` 开启且 `api_keys` 为空——不配 key 则所有 `/v1` 与 `/admin` 请求一律 401。先 `echo -n '你的key' | sha256sum`，把摘要填进 `config.toml` 的 `[auth].api_keys`，再 `export FLUXGATE_API_KEY=明文key`。
3. **上游就绪**：仓库自带 `config.toml` 的 mock 厂商（`mock-openai` / `mock-anthropic`）上游指向 `http://127.0.0.1:8001/v1`，需该地址有 OpenAI/Anthropic 兼容服务；换真实厂商时，同步修改 `src/rest_http.rs` 顶部的 `CHAT_MODEL` / `HINT_VENDOR` / `ANTHROPIC_MODEL` 常量。

**协议事实与勘误**（编写时以源码核实）：

- `GET /v1/models` 已在网关注册（`src/api/router_builder.rs:703`）：OpenAI list 格式，返回「模型目录 ∪ vendor 配置模型」去重排序、需鉴权、始终现算不缓存。`GET /admin/models` 是另一个端点——`[[model_catalog]]` 能力目录（含定价/能力位），管理员鉴权，二者用途不同。SDK 的 `FluxgateClient::models`（`src/sdk/admin.rs:140-143`）打的是 `GET /admin/models`（旧版误打未注册的 `/v1/models` 恒 404 的失配已修复），已无 SDK 偏差。
- `X-Vendor`/`X-Model` 等提示头两个推理端点均读取（`RoutingHints::from_headers`：`src/api/openai.rs:56`、`src/api/anthropic.rs:57`）——`/v1/chat/completions` 与 `/v1/messages` 都支持 `X-Vendor` / `X-Model` / `X-Session-Id` / `X-Weight`。
- SSE 错误帧 `event: error` 之后网关不补 `[DONE]`（`src/api/openai.rs:592-624`、`src/protocol/sse_frame.rs:25-46`），客户端解析器按失败终止，不要等待 `[DONE]`。

---

## sdk-usage — Rust SDK 客户端调用已部署网关（`mod sdk_usage`）

**是什么**：`chat` 非流式（完整 `ChatRequest` + `RequestConfig` 路由提示头，打印 token 用量与实际 vendor）、`stream_chat` 流式（`chat_stream` 逐 chunk 消费 SSE）、`embed` embedding（向量维度与用量）、`admin` 运维（health / vendors / models）。四个 `run` 共用 `sdk_usage::require_env`：任一环境变量缺失或为空即报错退出，不带空值发请求。

**依赖**：`sdk` feature = `fluxgate` 的 `sdk` 门面（`default-features = false`），只引 HTTP 客户端不拉起网关本体；`futures`（stream_chat 的 `StreamExt`）、`tokio`。

**前置条件**：

1. 已启动 Fluxgate 网关，并 `export FLUXGATE_BASE_URL`（如 `http://127.0.0.1:8080`）与 `FLUXGATE_API_KEY`（网关签发的 key）。
2. 两个关键点：
   - SDK 类型的导入路径是 `fluxgate::sdk::{ChatRequest, FluxgateClientBuilder, FluxgateSdkError, RequestConfig, ...}`，**不在 crate 根**——crate 根的 `FluxgateError` 是网关自身错误类型，勿混用。
   - 示例模型名 `gpt-4` / `text-embedding-3-small` 与 `vendor_hint = "mock-openai"` 取自仓库 `config.example.toml` 与 SDK 集成测试样例值，运行时按自己网关实际配置的 vendor/model 名称调整。

---

## embedded-server — 库形态嵌入宿主应用（`mod embedded_server`）

**是什么**：`fluxgate` 既能独立进程部署，也能作为库装配进宿主程序。两个 bin 演示两种形态：

- `standalone`：`server::run(Some("config.toml".into()))` 一行起服，阻塞到停机（信号停机与 flush 收尾由 `run()` 内部承担）。
- `mount_router`：`build_app` 拿到网关 `Router` 与 `ServerHandle`，merge 宿主自有路由（`GET /examples-info`）后自行 bind/serve，Ctrl+C 退出路径调用 `handle.shutdown()`。

**依赖**：`embedded` feature = `fluxgate` 的 `full` + `db-sqlite`；`axum 0.8 [macros]`、`serde_json`、`tokio`。`mount_router` 用到的 axum `json`/`serve` 与 tokio `net`/`signal` 由依赖侧特性联合补齐。

**前置条件**：运行目录（CWD）须有 `config.toml`，可从仓库根 `config.example.toml` 复制为起点；无配置或配置非法直接非零退出，无默认配置回退。监听地址取 `[server]` 段的 `host` / `port`。

**与独立部署的差异**：

- 嵌入形态下进程生命周期归宿主持有：`standalone` 整体交给 `run()`，`mount_router` 由宿主自行 serve 并收尾。
- 退出前必须调用 `ServerHandle::shutdown().await`，否则 flush worker 泄漏且缓冲日志丢失（`standalone` 的 `run()` 内部已处理）。
- 同进程 `run` / `build_app` 合计只能调用一次——内部全局态（OnceCell）二次写入即 panic。
- 默认 `full` feature 不含 gRPC：需要时自行启用 `grpc` feature，并经 `ServerHandle::take_grpc_server` 取出启动。

---

## grpc-client — tonic gRPC 客户端（`mod grpc_client`）

**是什么**：`build.rs`（启用 `grpc` feature 时）经 tonic-prost-build 0.14 从 vendored proto（`proto/sdforge.v1.proto`，sdforge 仓库同名文件的逐字节副本）生成 `sdforge.v1.SdForgeService` 客户端 stub；`src/grpc_client.rs` 依次演示三条路径：GetInfo 免认证直取服务信息、Call 无凭据撞认证门（`Unauthenticated`）、Call 带 `x-api-key` 凭据（库存网关终点 `NotFound`，gRPC Status details 携带 UnifiedError JSON）。三路径均在真实网关端到端实测。

**依赖与 feature**：本样例不依赖 fluxgate 本体（客户端只消费 proto wire 契约），故 `grpc` feature 不声明 fluxgate；演示的网关侧配置是 fluxgate 的 `grpc = ["dep:sdforge", "sdforge/grpc"]`（根 `Cargo.toml`，独立门控不进 `full`），以 `cargo build --features grpc` 启用。样例自身依赖（均 `default-features = false`）：`tonic 0.14 [transport, codegen]`、`tonic-prost 0.14`、`prost 0.14 [derive]`、`tokio 1`；build-dependencies 与 sdforge build.rs 同套：`tonic-build 0.14` + `tonic-prost-build 0.14`。版本与 Fluxgate `Cargo.lock` 对齐，零新增外部包。

**前置条件**：

1. `protoc` 在 PATH（网关侧 grpc feature 与本样例 build.rs 的共同构建前置）。
2. 网关侧启用三步：`--features grpc` 编译启动、`config.toml` 加 `[grpc]` 段且 `enabled = true`（默认端口 50051）、`[auth].api_keys` 存 API key 的 SHA256 hex 摘要；配置片段见 [`assets/grpc-client/grpc_config_snippet.toml`](assets/grpc-client/grpc_config_snippet.toml)。

**三点注意**（均以代码为准）：

1. 仓库 `config.toml` 注释称 GetInfo 也要求 key，与代码行为不符——sdforge `src/grpc/grpc_impl.rs` 的 `get_info` 不经 `authenticate`，Fluxgate `src/server/grpc.rs:19-21` 模块文档确认 **GetInfo 免认证**。
2. 库存网关未注册任何业务 `grpc_method`，Call 的预期终点是 `NotFound`；把 `DEMO_METHOD` 换成真实方法名即得 `success = true` + JSON `data`。
3. 取舍：样例不依赖 fluxgate 本体，避免把整个网关栈拖进客户端构建（fluxgate 无 gRPC 客户端 API）；代价是网关侧启用（feature + `[grpc]` 段）只能经本 README 与配置片段表达，而非依赖声明。

---

## 约束说明

- **自研库经 registry 集成**：`confers`、`dbnexus`、`inklog`、`limiteron`、`oxcache`、`sdforge`、`trait-kit` 由根 `Cargo.toml` 以 crates.io 版本声明、`Cargo.lock` 锁定校验和，cargo 自动拉取——克隆本仓库即可编译，无需同级放置这七个库（见根 README「环境要求」）。示例 crate 经 `fluxgate = { path = ".." }` 引用本仓库网关（该 path 指向仓库自身，非兄弟库），同为 workspace 成员。
- **示例不随发布流出**：`fluxgate-examples` 声明 `publish = false`，仅作为本地 workspace 成员存在；`cargo publish` 与任何发布流水线都不会带上它。
- **protoc 隔离**：`grpc` 不在 default，`build.rs` 依 `CARGO_FEATURE_GRPC` 提前返回，故 `cargo check/build --workspace` 无需 protoc；CI 的 grpc 编译/lint 覆盖由 `.github/workflows/ci.yml` 中显式的 `cargo check/clippy -p fluxgate-examples --features grpc` 步骤承担（这些作业已安装 protoc）。
- **根 README 未包含本目录入口**：根 `README.md` / `README_EN.md` 目前未列出 `examples/`，本文件是示例集的独立入口。
