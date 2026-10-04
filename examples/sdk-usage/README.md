# sdk-usage — Rust SDK 调用已部署网关示例

演示 `nexusflow` crate 的 `sdk` feature（Rust HTTP 客户端）如何调用已部署的 NexusFlow 网关，覆盖非流式 chat、流式 chat、embedding、运维端点四类场景。依赖只启用 SDK 门面（`default-features = false, features = ["sdk"]`），不构建网关服务器本体。

## 前置条件

1. 已部署并启动 NexusFlow 网关（启动方式见仓库根 README）。
2. 网关侧已配置可用 vendor/model，并持有网关签发的 API Key。
3. 导出环境变量：

```bash
export NEXUSFLOW_BASE_URL="http://127.0.0.1:8080"
export NEXUSFLOW_API_KEY="<网关签发的 key>"
```

任一变量缺失或为空时示例直接报错退出，不会带空值发请求。

## 运行

```bash
cargo run -p nexusflow-example-sdk-usage --bin chat         # 非流式：完整 ChatRequest + RequestConfig 路由提示头，打印 token 用量与实际 vendor
cargo run -p nexusflow-example-sdk-usage --bin stream_chat  # 流式：chat_stream 逐 chunk 消费 SSE
cargo run -p nexusflow-example-sdk-usage --bin embed        # embedding：两条文本向量，打印维度与用量
cargo run -p nexusflow-example-sdk-usage --bin admin        # 运维：health / vendors / models
```

## 说明

- 模型名（`gpt-4`、`text-embedding-3-small`）与 `vendor_hint`（`mock-openai`）取自仓库 `config.example.toml` 与 SDK 集成测试的样例值，运行时按自己网关的 vendor/model 配置调整。
- `vendor_hint` / `model_hint` 映射 `X-Vendor` / `X-Model` 路由头，不进入请求 body。
- 非流式请求默认总超时 30s（`NexusFlowClientBuilder::timeout` 可覆盖）；流式不设总超时，由 `read_timeout` 按读看护连接停滞。
- 错误路径不静默：认证失败区分 `NexusFlowSdkError::Auth` 单独提示，其余错误原样上抛，进程以非零码退出。
