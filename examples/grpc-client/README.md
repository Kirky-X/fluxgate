# NexusFlow gRPC 客户端样例（nexusflow-example-grpc-client）

tonic 客户端样例：演示调用 NexusFlow 网关的 gRPC 门面
 `sdforge.v1.SdForgeService`（GetInfo / Call / CallStream，JSON over gRPC）。

 **采用形态**：`build.rs` 用 tonic-prost-build 0.14 从 proto 生成客户端 stub +
 `src/main.rs` 调 GetInfo 与 Call。proto 取自 sdforge 仓库
 `proto/sdforge.v1.proto`（本目录 `proto/sdforge.v1.proto` 为其逐字节副本），
 protoc 与 tonic-build 均可用、成本可控，故采用完整可运行形态而非说明型降级。

## 前置条件

- `protoc` 在 PATH（网关侧 grpc feature 与本样例 build.rs 的共同构建前置）。
- 网关以 grpc feature 编译启动（见下节）。

## 网关侧启用

1. **编译期启用 grpc feature**（独立门控，不在 `full` 内）：

   ```bash
   cargo build --features grpc
   cargo run --features grpc -- /path/to/config.toml
   ```

2. **config.toml 加 `[grpc]` 段**（片段见 [grpc_config_snippet.toml](grpc_config_snippet.toml)）：

   ```toml
   [grpc]
   enabled = true          # 缺席或 false 时不监听 gRPC 端口
   host = "0.0.0.0"
   port = 50051
   ```

3. **认证**：Call/CallStream 复用 `[auth]` 段——`api_keys` 存 SHA256 hex
   摘要（`echo -n "your-api-key" | sha256sum`），网关对请求 key 现场算摘要
   后常量时间比较，语义与 HTTP 中间件一致。

   与 HTTP `/health` 白名单的差异**以代码为准**：门面仅对 Call/CallStream
   挂 auth_verifier，**GetInfo 免认证**（NexusFlow `src/server/grpc.rs` 模块
   文档；sdforge `src/grpc/grpc_impl.rs` 的 `get_info` 不经过 `authenticate`；
   `config.toml` 内注释「GetInfo 一律要求 key」与代码行为不符）。

## 客户端运行

```bash
NEXUSFLOW_API_KEY=your-api-key cargo run -p nexusflow-example-grpc-client
```

凭据经环境变量传入，源码不落 key 材料；等价的 header 形态是
`authorization: Bearer <key>`（网关侧 `x-api-key` 裸值优先提取）。

## 实测输出（真实网关，[auth] enabled + 测试 key）

```text
GetInfo: SdForge Service v0.5.0-rc.6 — SdForge Multi-Protocol SDK Framework
可用方法: （无——网关未注册 grpc_method）
[无凭据 Call] 被拒: code=Unauthenticated message=missing api key
[带凭据 Call] 失败: code=NotFound message=method 'demo_method' not registered (no matching #[forge(grpc_method = "...")] declaration)
```

- **GetInfo** 免认证直取服务信息；NexusFlow 当前未注册任何业务
  grpc_method，故方法列表为空。
- **无凭据 Call** → `Unauthenticated`（`[auth]` enabled 时；enabled=false 则
  放行进入方法查找）。
- **带凭据 Call** → 通过认证后进入方法查找；库存网关无注册方法，终点是
  `NotFound`。业务错误的 `Status::details` 携带 UnifiedError JSON
  （code/field/trace_id），样例原样透出。网关注册业务方法后，把
  `DEMO_METHOD` 换成真实 grpc_method 即可拿到 `success=true` + JSON `data`。
- 连接失败 / GetInfo 失败 / key 含非法 metadata 字符时以非零退出码终止；
  Call 的认证拒绝与方法未注册属于演示观察值，打印后继续。

## 门面契约（v1）

| RPC | 认证 | 说明 |
| --- | --- | --- |
| `GetInfo(InfoRequest) -> InfoResponse` | 免认证 | 服务名/版本/已注册方法列表 |
| `Call(CallRequest) -> CallResponse` | `x-api-key` 或 `authorization: Bearer` | `method` = 注册的 grpc_method；`parameters`（map<string,string>）逐键映射 handler 参数；`data` 仅在 handler 声明 body 参数时投递；响应 `data` 为 handler 返回值的 JSON 文本 |
| `CallStream(CallRequest) -> stream CallResponse` | 同 Call | 生成代码含 `call_stream`，本样例未调用；流式 handler 项逐条映射为 CallResponse |

业务失败有两条通道：门面内失败走响应体（`success=false` + `error` 文案）；
业务错误映射为真实 gRPC Status（details 携带 UnifiedError JSON）。

## 依赖与 proto 维护

- 版本对齐 sdforge grpc feature 的依赖面（仓库 Cargo.lock：tonic /
  tonic-prost / tonic-build 0.14.6，prost 0.14.4），复用工作区编译缓存。
- `proto/sdforge.v1.proto` 为 sdforge 仓库同名文件的副本：NexusFlow 的 gRPC
  门面即 sdforge 的服务定义（无自有 proto）。sdforge 门面消息/服务变更时需
  重新同步本副本。

## 形态取舍

样例**不依赖 nexusflow 本体**：客户端只消费公开的 wire 契约（proto），不引
网关内部 crate——依赖 nexusflow 会把整个网关栈拖进客户端构建，且 nexusflow
不提供 gRPC 客户端 API。gRPC 网关侧的启用（feature + `[grpc]` 段）通过本
README 与配置片段表达，而非依赖声明。
