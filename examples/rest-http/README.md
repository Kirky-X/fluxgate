# rest-http：裸 HTTP 接入 NexusFlow 网关

不经过 SDK，直接对网关线级协议编程。任何语言都可对照本目录移植：Rust（reqwest 直连 + 手工 SSE 解析）、curl、Python（官方 openai 库）、Node.js（官方 openai npm 包）。

## 运行前提

1. **网关已启动**：仓库根 `cargo run --release`（默认 feature 即含二进制所需的 `full`），默认监听 `127.0.0.1:8080`（`config.toml [server]` 段）。
2. **上游厂商就绪**：仓库自带 `config.toml` 的厂商是 `mock-openai` / `mock-anthropic`，上游指向 `http://127.0.0.1:8001/v1`，需要该地址有可用的 OpenAI/Anthropic 兼容服务；换成真实厂商配置时，同步修改各文件顶部的 `MODEL` / `VENDOR` / `ANTHROPIC_MODEL` 常量。
3. **API key**：dev 配置 `[auth].enabled = true` 且 `api_keys` 为空——不配 key 时所有 `/v1` 与 `/admin` 请求一律 401。先 `echo -n "你的key" | sha256sum`，把摘要填进 `config.toml [auth].api_keys`，再 `export NEXUSFLOW_API_KEY="你的key"`（明文）。
4. **多语言依赖**（按需）：
   - Python：`pip install openai`（v1.x）
   - Node.js：`npm install openai`（Node ≥ 18，本目录 `package.json` 已声明 `type: module`）

## 环境变量

| 变量 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- |
| `NEXUSFLOW_BASE_URL` | 否 | `http://127.0.0.1:8080` | 网关地址 |
| `NEXUSFLOW_API_KEY` | **是** | 无 | 缺失时报错退出，示例不做静默默认 |

## 文件说明

| 文件 | 内容 |
| --- | --- |
| `src/main.rs` | reqwest 直连四连：健康检查 → 非流式 chat（带 `X-Vendor`/`X-Model` 提示头）→ Anthropic 格式 `/v1/messages`（验证协议互转）→ SSE 流式（`bytes_stream` 手工解析） |
| `Cargo.toml` | 独立示例成员，不依赖 nexusflow crate；第三方依赖全部 `default-features = false` + 显式 features |
| `curl_samples.sh` | 各端点的 curl 命令，含响应缓存 `X-Cache: MISS→HIT` 与 `X-No-Cache: 1` 的对照演示 |
| `python_client.py` | 官方 openai 库用法：`base_url` 指向网关 `/v1`，非流式 + 流式 |
| `node_client.js` + `package.json` | openai npm 包同款用法（ESM） |
| `README.md` | 本文件 |

## 运行

```bash
# Rust（在仓库根执行）
cargo run -p nexusflow-example-rest-http

# curl
NEXUSFLOW_API_KEY=你的key bash examples/rest-http/curl_samples.sh

# Python / Node（在 examples/rest-http 目录）
NEXUSFLOW_API_KEY=你的key python3 python_client.py
NEXUSFLOW_API_KEY=你的key npm install openai && NEXUSFLOW_API_KEY=你的key node node_client.js
```

所有示例失败即非零退出并打印网关状态码与响应原文；成功流式必须等到 `data: [DONE]`，否则按断流报错。

## 协议速查（含勘误）

| 端点 | 认证 | 说明 |
| --- | --- | --- |
| `GET /api/v1/health` | 免认证 | 实际注册路径；裸 `/health` 未注册，会 404 |
| `POST /v1/chat/completions` | Bearer（也接受 `x-api-key`） | OpenAI 兼容；`X-Vendor`/`X-Model` 路由提示**仅此端点生效**，值须命中 `[vendors]` 配置；`temperature` 缺省或 0 才会参与响应缓存（响应带 `X-Cache: MISS/HIT`） |
| `POST /v1/messages` | Bearer（也接受 `x-api-key`） | Anthropic 兼容（`model`/`messages` 必填，`max_tokens` 建议带上）；网关按目标厂商协议自动互转，回包恒为 Anthropic 形状；**不消费** `X-Vendor`/`X-Model` |
| `GET /admin/models` | 需要 | 模型列表 |

勘误（以源码为准，编写本示例时核实）：

- **`GET /v1/models` 未在网关注册**，请求会 404；模型列表用 `GET /admin/models`。SDK 内 `NexusFlowClient::models`（src/sdk/admin.rs）引用的 `v1/models` 与服务端路由表不一致，属已知偏差。
- SSE 错误以 `event: error` + `data: <msg>` 帧下发，**此后网关不再补 `[DONE]`**——客户端见到 `event: error` 或 data 内嵌 `error` 对象时应按失败终止，不要等 `[DONE]`。

## 移植提示（手工解析 SSE 的四个要点）

1. `bytes_stream` 的块边界可能劈在任意字节处：按字节缓冲、凑齐完整行（`\n`）再解码，多字节 UTF-8 不会被劈坏。
2. 只关心两类行：`event:`（记录类型）与 `data:`（载荷）；空行是帧定界。
3. `data: [DONE]` 是成功流的终止标记；`data:` 后的 JSON 内嵌 `error` 对象是上游透传错误的规范形状。
4. 逐增量打印时记得 flush 输出缓冲，否则流式观感退化成一次性输出。
