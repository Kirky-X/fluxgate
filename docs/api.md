# NexusFlow REST API 文档

**版本**: v1.0
**Base URL**: `http://{host}:{port}` (默认 `http://0.0.0.0:8080`)
**内容类型**: `application/json` (流式响应为 `text/event-stream`)

---

## 目录

- [认证](#认证)
- [通用错误响应](#通用错误响应)
- [错误码映射](#错误码映射)
- [LLM 推理端点](#llm-推理端点)
  - [POST /v1/chat/completions](#post-v1chatcompletions)
  - [POST /v1/embeddings](#post-v1embeddings)
  - [POST /v1/messages](#post-v1messages)
- [管理端点](#管理端点)
  - [GET /admin/logs](#get-adminlogs)
  - [GET /admin/logs/search](#get-adminlogssearch)
  - [GET /admin/stats/tokens](#get-adminstatstokens)
  - [GET /admin/metrics](#get-adminmetrics)
  - [GET /admin/vendors](#get-adminvendors)
  - [GET /admin/vendors/:id/status](#get-adminvendorsidstatus)
  - [POST /admin/vendors/:id/reset](#post-adminvendorsidreset)
  - [GET /admin/config](#get-adminconfig)
  - [POST /admin/config](#post-adminconfig)
  - [GET /admin/models](#get-adminmodels)
  - [POST /admin/models/:id](#post-adminmodelsid)
- [健康检查](#健康检查)
  - [GET /health](#get-health)

---

## 认证

除 `/health` 与 `/` 外的所有端点均要求认证。

| 项 | 值 |
|---|---|
| 认证方式 | Bearer Token (API Key) |
| 请求头 | `Authorization: Bearer <api_key>` |
| 备用请求头 | `x-api-key: <api_key>` |
| 配置位置 | `config.toml` 的 `[auth]` 段 |
| 启用开关 | `auth.enabled` (`true` / `false`) |

认证失败的统一响应：

```json
{
  "error": {
    "message": "Missing API key",
    "type": "authentication_error"
  }
}
```

HTTP 状态码：`401 Unauthorized`（`type` 可能为 `authentication_error`，无效 key 时 `message` 为 `"Invalid API key"`）。

> `/health` 与 `/` 路径在认证中间件中被显式放行（见 `src/middleware/auth.rs`）。

---

## 通用错误响应

所有非 2xx 响应均使用以下结构（见 `src/error.rs` 中的 `IntoResponse` 实现）：

```json
{
  "error": {
    "message": "<人类可读的错误描述>",
    "type": "<错误类型代码>"
  }
}
```

字段说明：

| 字段 | 类型 | 说明 |
|---|---|---|
| `error.message` | string | 错误详细描述（来自 `thiserror` 的 `#[error(...)]`） |
| `error.type` | string | 固定的错误类型代码，不暴露内部枚举结构 |

---

## 错误码映射

以下映射来源于 `src/error.rs` 中 `NexusFlowError::into_response` 的实际实现（代码为准）。

| 错误类型 (`error.type`) | HTTP 状态码 | 触发场景 |
|---|---|---|
| `validation_error` | 400 Bad Request | 请求体缺少必填字段（如 `model`） |
| `protocol_error` | 502 Bad Gateway | 协议转换失败、上游返回非预期响应、Embeddings 调用非 OpenAI 厂商 |
| `vendor_unavailable` | 503 Service Unavailable | 厂商不可用、网络请求失败 |
| `timeout` | 504 Gateway Timeout | 请求超时、`oneshot` 通道接收失败 |
| `rate_limited` | 429 Too Many Requests | 厂商侧限流 |
| `config_error` | 500 Internal Server Error | 配置错误（如 API key 环境变量未设置） |
| `database_error` | 500 Internal Server Error | 数据库操作失败、IO 错误 |
| `routing_error` | 500 Internal Server Error | 路由决策失败（无可用厂商等） |

> 限流中间件 `rate_limit_middleware`（基于 `limiteron::Governor`）独立于上述错误码：
> - 超出速率限制 → `429 Too Many Requests`，body 为纯文本 `"Rate limit exceeded"`
> - 被封禁 → `403 Forbidden`，body 为纯文本 `"Access denied"`
> - `/health`、`/healthz`、`/ready` 路径放行，不进入限流检查

---

## LLM 推理端点

### POST /v1/chat/completions

OpenAI Chat Completions 兼容端点。根据请求体中的 `model` 字段进行路由决策，当目标厂商类型为 `openai` 时走零拷贝透传路径，否则经 `UnifiedRequest` 转换。

**认证**: 需要 Bearer Token

**请求头**:

| Header | 必填 | 说明 |
|---|---|---|
| `Authorization` | 是 | `Bearer <api_key>` |
| `Content-Type` | 是 | `application/json` |
| `X-Vendor` | 否 | 路由提示：优先使用的厂商 ID |
| `X-Model` | 否 | 路由提示：优先使用的模型 ID |

**请求体** (`Json<Value>`, OpenAI 兼容):

```json
{
  "model": "gpt-4",
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "Hello"}
  ],
  "temperature": 0.7,
  "max_tokens": 1024,
  "stream": false,
  "top_p": 1.0,
  "frequency_penalty": 0.0,
  "presence_penalty": 0.0,
  "stop": ["\n\n"],
  "user": "user-123",
  "tools": [],
  "tool_choice": "auto"
}
```

必填字段校验（缺失返回 `validation_error`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `model` | string | 是 | 模型 ID，用于路由决策 |
| `messages` | array | 是 | 消息列表 |
| `stream` | boolean | 否 | 默认 `false`；`true` 时返回 SSE 流 |

**响应 (非流式, `stream: false`)**:

`Content-Type: application/json`，标准 OpenAI Chat Completions 响应结构：

```json
{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1700000000,
  "model": "gpt-4",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! How can I help you?"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 20,
    "completion_tokens": 10,
    "total_tokens": 30
  }
}
```

**响应 (流式, `stream: true`)**:

`Content-Type: text/event-stream`，响应头额外包含 `Cache-Control: no-cache` 与 `Connection: keep-alive`。每个 chunk 为标准 OpenAI SSE 格式（`data: {...}\n\n`）。

**错误码**: `validation_error`(400) | `routing_error`(500) | `vendor_unavailable`(503) | `timeout`(504) | `protocol_error`(502) | `rate_limited`(429)

**curl 示例**:

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

流式请求示例：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": true
  }'
```

指定厂商路由提示：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -H "X-Vendor: mock-openai" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "Hi"}]}'
```

---

### POST /v1/embeddings

OpenAI Embeddings 兼容端点。仅支持 `type_ = "openai"` 的厂商，否则返回 `protocol_error`。

**认证**: 需要 Bearer Token

**请求体** (`Json<Value>`):

```json
{
  "model": "text-embedding-3-small",
  "input": "The food was delicious and the waiter was friendly."
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `model` | string | 是 | 模型 ID |
| `input` | string \| array | 是 | 待向量化的文本或文本数组 |

请求体先经 `embedding_request_to_openai` 转换后再转发给上游 OpenAI 兼容 API。

**响应**:

标准 OpenAI Embeddings 响应结构：

```json
{
  "id": "embd-abc123",
  "object": "list",
  "created": 1700000000,
  "model": "text-embedding-3-small",
  "data": [
    {
      "index": 0,
      "object": "embedding",
      "embedding": [0.0023, -0.0091, 0.0152, "..."]
    }
  ],
  "usage": {
    "prompt_tokens": 12,
    "total_tokens": 12
  }
}
```

**错误码**: `validation_error`(400) | `protocol_error`(502，含「Embeddings only supported for OpenAI vendors」) | `vendor_unavailable`(503) | `config_error`(500，API key env 未设置)

**curl 示例**:

```bash
curl -X POST http://localhost:8080/v1/embeddings \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "text-embedding-3-small",
    "input": "The food was delicious."
  }'
```

---

### POST /v1/messages

Anthropic Messages API 兼容端点。当目标厂商类型为 `anthropic` 时走零拷贝透传路径，否则经 `UnifiedRequest` 转换。

**认证**: 需要 Bearer Token

**请求头**:

| Header | 必填 | 说明 |
|---|---|---|
| `Authorization` | 是 | `Bearer <api_key>` |
| `Content-Type` | 是 | `application/json` |
| `anthropic-version` | 否 | 透传给上游 Anthropic API |

**请求体** (`Json<Value>`, Anthropic 兼容):

```json
{
  "model": "claude-3-opus",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "Hello, Claude"}
  ],
  "stream": false,
  "system": "You are a helpful assistant.",
  "temperature": 1.0,
  "top_p": 1.0
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `model` | string | 是 | 模型 ID |
| `max_tokens` | integer | 是 | 最大生成 token 数 |
| `messages` | array | 是 | 消息列表 |
| `stream` | boolean | 否 | 默认 `false` |

**响应 (非流式)**:

`Content-Type: application/json`，标准 Anthropic Messages 响应结构：

```json
{
  "id": "msg_abc123",
  "type": "message",
  "role": "assistant",
  "model": "claude-3-opus",
  "content": [
    {"type": "text", "text": "Hello! How can I help you today?"}
  ],
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 12,
    "output_tokens": 10
  }
}
```

**响应 (流式, `stream: true`)**:

`Content-Type: text/event-stream`，标准 Anthropic SSE 事件流（`event: ...\ndata: {...}\n\n`）。

**错误码**: `validation_error`(400) | `routing_error`(500) | `vendor_unavailable`(503) | `timeout`(504) | `protocol_error`(502) | `rate_limited`(429)

**curl 示例**:

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-3-opus",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

---

## 管理端点

所有 `/admin/*` 端点均要求 Bearer Token 认证，且受全局限流策略约束（`config.toml` 中 `[rate_limit.endpoints]` 的 `/admin/*` 规则）。

### GET /admin/logs

按条件查询请求日志（`request_logs` 表），按时间戳倒序返回。

**认证**: 需要 Bearer Token

**Query 参数**:

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `start_time` | integer (Unix 秒) | 否 | - | 起始时间戳 |
| `end_time` | integer (Unix 秒) | 否 | - | 结束时间戳（注：当前实现未在 SQL 中应用此过滤） |
| `vendor` | string | 否 | - | 实际厂商 ID 精确匹配 |
| `model` | string | 否 | - | 实际模型 ID 精确匹配 |
| `limit` | integer | 否 | `100` | 返回条数上限；`0` 表示不限 |

**响应**: `RequestLog` 数组

```json
[
  {
    "id": 1,
    "request_id": "550e8400-e29b-41d4-a716-446655440000",
    "timestamp": 1700000000,
    "endpoint": "/v1/chat/completions",
    "input_format": "openai",
    "abstract_model": "gpt-4",
    "messages": "[{\"role\":\"user\",\"content\":\"Hello\"}]",
    "actual_vendor": "mock-openai",
    "actual_model": "gpt-4",
    "routing_strategy": "Weighted",
    "status_code": 200,
    "response": "{...}",
    "error_message": null,
    "prompt_tokens": 1024,
    "completion_tokens": null,
    "total_tokens": null,
    "duration_ms": 523
  }
]
```

**错误码**: `database_error`(500)

**curl 示例**:

```bash
curl "http://localhost:8080/admin/logs?vendor=mock-openai&limit=10" \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/logs/search

按关键词模糊搜索日志的 `messages` 与 `error_message` 字段（`LIKE %keyword%`）。

**认证**: 需要 Bearer Token

**Query 参数**:

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `vendor` | string | 否 | `""` (空串) | 搜索关键词（字段名沿用 `vendor`，但实际用作搜索关键词） |
| `limit` | integer | 否 | `100` | 返回条数上限 |
| `start_time` / `end_time` / `model` | - | 否 | - | 接受但不参与搜索逻辑 |

> 注意：此端点复用 `LogQuery` 结构，`vendor` 字段被当作搜索关键词使用。

**响应**: `RequestLog` 数组（结构同 [GET /admin/logs](#get-adminlogs)）

**错误码**: `database_error`(500)

**curl 示例**:

```bash
curl "http://localhost:8080/admin/logs/search?vendor=Hello&limit=50" \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/stats/tokens

查询 Token 使用统计（`token_usage` 表，按日期与小时预聚合）。

**认证**: 需要 Bearer Token

**Query 参数**:

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `vendor` | string | 否 | - | 厂商过滤；未提供则返回全部 |
| `start_time` / `end_time` / `model` / `limit` | - | 否 | - | 接受但不参与查询逻辑 |

**响应**: `TokenUsage` 数组（按 `date DESC, hour DESC` 排序）

```json
[
  {
    "id": 1,
    "vendor": "mock-openai",
    "model": "gpt-4",
    "date": "2026-07-14",
    "hour": 10,
    "request_count": 42,
    "total_tokens": 12000,
    "prompt_tokens": 8000,
    "completion_tokens": 4000
  }
]
```

**错误码**: `database_error`(500)

**curl 示例**:

```bash
curl "http://localhost:8080/admin/stats/tokens?vendor=mock-openai" \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/metrics

返回全局指标快照（来自 `GlobalMetrics`，包含总请求数、错误率、平均延迟及各厂商细分指标）。

**认证**: 需要 Bearer Token

**Query 参数**: 无

**响应**: `MetricsSnapshot`

```json
{
  "total_requests": 1000,
  "successful_requests": 950,
  "failed_requests": 50,
  "error_rate": 0.05,
  "avg_duration_ms": 320.5,
  "vendors": [
    {
      "vendor": "mock-openai",
      "requests": 700,
      "successes": 680,
      "failures": 20,
      "total_duration_ns": 224000000000,
      "last_request_at": 1700000000,
      "last_error": "Connection reset"
    }
  ]
}
```

`vendors` 数组中每个元素的字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `vendor` | string | 厂商 ID |
| `requests` | integer | 总请求数 |
| `successes` | integer | 成功请求数 |
| `failures` | integer | 失败请求数 |
| `total_duration_ns` | integer | 累计耗时（纳秒） |
| `last_request_at` | integer \| null | 最近请求 Unix 时间戳 |
| `last_error` | string \| null | 最近错误描述 |

**错误码**: 无业务错误（仅可能的限流 429/403）

**curl 示例**:

```bash
curl http://localhost:8080/admin/metrics \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/vendors

返回所有已注册厂商的实时状态与队列长度。

**认证**: 需要 Bearer Token

**Query 参数**: 无

**响应**: 厂商状态数组

```json
[
  {
    "id": "mock-openai",
    "status": "Available",
    "queue_length": 0
  },
  {
    "id": "mock-anthropic",
    "status": "Busy",
    "queue_length": 3
  }
]
```

`status` 为 `VendorStatus` 枚举的 `Debug` 输出，可能值：`Available` | `Busy` | `Throttled` | `Unavailable`。

**错误码**: 无业务错误

**curl 示例**:

```bash
curl http://localhost:8080/admin/vendors \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/vendors/:id/status

返回指定厂商的实时状态与队列长度。

**认证**: 需要 Bearer Token

**Path 参数**:

| 参数 | 类型 | 说明 |
|---|---|---|
| `id` | string | 厂商 ID |

**响应**:

```json
{
  "id": "mock-openai",
  "status": "Available",
  "queue_length": 0
}
```

当厂商 ID 不存在时，`status` 返回 `"Unavailable"`。

**错误码**: 无业务错误

**curl 示例**:

```bash
curl http://localhost:8080/admin/vendors/mock-openai/status \
  -H "Authorization: Bearer $API_KEY"
```

---

### POST /admin/vendors/:id/reset

重置厂商限流状态。当前实现为 no-op（`limiteron::Governor` 自动管理令牌桶 refill），保留此端点以维持 API 兼容性。

**认证**: 需要 Bearer Token

**Path 参数**:

| 参数 | 类型 | 说明 |
|---|---|---|
| `id` | string | 厂商 ID（当前实现未使用） |

**请求体**: 无

**响应**:

```json
{
  "success": true,
  "message": "厂商限流状态由 limiteron Governor 自动管理"
}
```

**错误码**: 无业务错误

**curl 示例**:

```bash
curl -X POST http://localhost:8080/admin/vendors/mock-openai/reset \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/config

返回当前运行时配置（完整 `Config` 结构）。

**认证**: 需要 Bearer Token

**响应**: `Config`

```json
{
  "server": {
    "host": "0.0.0.0",
    "port": 8080
  },
  "database": {
    "path": "./data/llm_proxy.db",
    "pool_size": 5
  },
  "vendors": [
    {
      "id": "mock-openai",
      "name": "Mock OpenAI",
      "type_": "openai",
      "api_key_env": "MOCK_OPENAI_KEY",
      "api_base": "http://127.0.0.1:8001/v1",
      "enabled": true,
      "models": [
        {
          "model_id": "gpt-3.5-turbo",
          "abstract_id": null,
          "weight": 1.0,
          "enabled": true
        }
      ],
      "rate_limit": {
        "type": "token",
        "max_tokens": 10000000
      },
      "concurrency": {
        "requests_per_second": 100
      }
    }
  ],
  "global_model": {
    "enabled": true,
    "strategy": "weighted"
  }
}
```

`rate_limit` 为 tagged enum，支持三种形态：

```json
{"type": "time_window", "start": "00:00", "end": "06:00"}
{"type": "interval", "hours": 24}
{"type": "token", "max_tokens": 10000000}
```

**错误码**: 无业务错误

**curl 示例**:

```bash
curl http://localhost:8080/admin/config \
  -H "Authorization: Bearer $API_KEY"
```

---

### POST /admin/config

整体更新运行时配置（覆盖写）。

**认证**: 需要 Bearer Token

**请求体**: 完整的 `Config` 结构（同 [GET /admin/config](#get-adminconfig) 响应）

**响应**:

```json
{
  "success": true,
  "message": "Config updated"
}
```

**错误码**: `config_error`(500，更新失败或反序列化失败)

**curl 示例**:

```bash
curl -X POST http://localhost:8080/admin/config \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d @config.toml.json
```

---

### GET /admin/models

返回全局模型配置（当前实现返回包含单个 `global_model` 的数组，或空数组）。

**认证**: 需要 Bearer Token

**响应**: `GlobalModelConfig` 数组

```json
[
  {
    "enabled": true,
    "strategy": "weighted"
  }
]
```

当 `config.global_model` 为 `None` 时返回 `[]`。

**错误码**: 无业务错误

**curl 示例**:

```bash
curl http://localhost:8080/admin/models \
  -H "Authorization: Bearer $API_KEY"
```

---

### POST /admin/models/:id

更新全局模型配置。当前仅支持 `id == "global"`，其他 ID 返回 `config_error`。

**认证**: 需要 Bearer Token

**Path 参数**:

| 参数 | 类型 | 说明 |
|---|---|---|
| `id` | string | 必须为 `"global"` |

**请求体**: `GlobalModelConfig`

```json
{
  "enabled": true,
  "strategy": "weighted"
}
```

`strategy` 可选值：`weighted` | `fallback` | `priority`（见 `src/router/`）。

**响应**:

```json
{
  "success": true,
  "message": "Global model config updated"
}
```

**错误码**: `config_error`(500，当 `id != "global"` 时返回 `"Only 'global' model config is supported"`)

**curl 示例**:

```bash
curl -X POST http://localhost:8080/admin/models/global \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"enabled": true, "strategy": "weighted"}'
```

---

## 健康检查

### GET /health

返回服务运行状态。无需认证，且不受限流约束。

**认证**: 无需

**响应**:

```json
{
  "status": "ok",
  "version": "0.1.0",
  "timestamp": "2026-07-14T10:30:00+00:00"
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `status` | string | 固定 `"ok"` |
| `version` | string | 来自 `CARGO_PKG_VERSION` |
| `timestamp` | string | RFC 3339 格式的当前 UTC 时间 |

**错误码**: 无

**curl 示例**:

```bash
curl http://localhost:8080/health
```

---

## 附录：路由提示请求头

LLM 推理端点（`/v1/chat/completions`、`/v1/messages`）支持通过请求头传递路由提示，绕过默认路由决策：

| Header | 说明 |
|---|---|
| `X-Vendor` | 优先使用的厂商 ID |
| `X-Model` | 优先使用的模型 ID |

当任一请求头非空时，调用 `route_with_hints` 而非 `route`。两个端点中只有 `/v1/chat/completions` 显式读取这些请求头（见 `src/api/openai.rs`）；`/v1/messages` 虽接收 `HeaderMap` 但当前未使用（见 `src/api/anthropic.rs` 中 `_headers` 参数）。
