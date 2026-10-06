# NexusFlow REST API 文档

**文档基线**: v0.1.0（以 Cargo.toml 为准）
**Base URL**: `http://0.0.0.0:8080`（默认配置，实际以 `[server]` 段 host/port 为准）
**内容类型**: `application/json`（流式响应为 `text/event-stream`）
**内容协商**: 错误消息文案支持中英文（服务端 i18n，跟随 `NEXUSFLOW_LANG`/`LANG` 环境变量，检测链为 `NEXUSFLOW_LANG` → `LC_ALL` → `LC_MESSAGES` → `LANG`）

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
  - [PUT /admin/models/:id](#put-adminmodelsid)
  - [DELETE /admin/models/:id](#delete-adminmodelsid)
  - [POST /admin/models/:id](#post-adminmodelsid)
- [健康检查](#健康检查)
  - [GET /api/v1/health](#get-apiv1health)
- [附录：路由提示请求头](#附录路由提示请求头)

---

## 认证

除健康检查与根路径外，所有端点均要求认证。

| 项 | 值 |
|---|---|
| 认证方式 | Bearer Token (API Key) |
| 请求头 | `Authorization: Bearer <api_key>` |
| 备用请求头 | `x-api-key: <api_key>` |
| 配置位置 | `config.toml` 的 `[auth]` 段（`api_keys` 存 SHA256 hex 摘要，明文不落盘） |
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

HTTP 状态码：`401 Unauthorized`（无效 key 时 `message` 为 `"Invalid API key"`）。

> 认证中间件的放行路径为代码事实（`src/middleware/auth.rs`）：`/health`、`/api/v1/health`、`/` 三个路径显式放行。其中 `/api/v1/health` 是健康检查的实际注册路径（见[健康检查](#健康检查)）；`/health` 裸路径未注册路由（请求会 404），但保留在白名单中。

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
| `error.message` | string | 错误详细描述，走 i18n 本地化轨道（跟随 locale 输出中/英文案） |
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
| `all_vendors_throttled` | 429 Too Many Requests + `Retry-After` 头 | Fallback 换路后所有候选厂商仍被限流时抛出（需 `[routing].retry_after_hint` 开启；默认关闭时保持原错误语义不产生本码）；`Retry-After` 值取各候选厂商限流窗口重置剩余秒数的最小值 |
| `config_error` | 500 Internal Server Error | 配置错误（如 API key 环境变量未设置） |
| `database_error` | 500 Internal Server Error | 数据库操作失败、IO 错误 |
| `routing_error` | 500 Internal Server Error | 路由决策失败（无可用厂商等） |

> **限流与准入（网关侧，独立于上述错误码；absorb-sibling-libs G5 起 tower 化）**：
>
> - 入站限流中间件由两层组成：`[[rate_limit.rules]]` 端点规则表（`path` 前缀匹配、按租户分桶）+ limiteron tower 全局规则层（一规则一 `KeyedRateLimitLayer`，按租户摘要分桶；空规则时为共享 TokenBucket(100, 10/s) 兜底）。
> - 端点规则超限 → `429 Too Many Requests` + `Retry-After` 头（重置秒数），body 为 JSON，`error.message` 文案随 locale（zh `"已超过速率限制"` / en `"Rate limit exceeded"`）。
> - 全局规则超速 → `429 Too Many Requests` + `Retry-After` 头（窗口剩余秒），文案同上；旧 `Governor` 路径的无头 429 与 `403` 封禁分支已删除（规则 ban 恒为 None）。
> - 限流白名单路径（不进入限流检查）：`/health`、`/healthz`、`/ready`、`/api/v1/health`、`/readyz`、`/metrics`、`/api-docs/*`、`/swagger-ui/*`。
> - 每次响应（含 429）均回传 **`X-Request-Id`** 响应头：透传客户端所带 ID，未带时网关生成（`req-` 前缀）；同请求 tracing span 携带该 ID（absorb-sibling-libs G8）。
> - **准入层**（拥塞感知优先级准入，`src/middleware/admission.rs`）：`[admission].enabled`（默认 `false`）开启后，网关并发占用率（Σ 在途 / Σ 并发上限）达到 `congestion_threshold`（默认 `0.9`）时，优先级低于当前在途最小档位的请求被直接卸载（shed），返回 `429 Too Many Requests` + `Retry-After: 1`，文案随 locale（zh `"准入被拒绝（拥塞）"`）。请求档位（dedicated/guaranteed/elastic/spot）由 `[auth].keys` 子表按 API key 声明，未声明的 key 缺省 `elastic`。健康检查路径永不卸载。

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
| `X-Session-Id` | 否 | 会话亲和键；缺失时以 `messages[0]` 前缀哈希兜底（见[附录](#附录路由提示请求头)） |
| `X-Weight` | 否 | 请求级权重（仅接受有限正数）；生效需服务端 `[routing].allow_request_weight` 开启 |

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

**错误码**: `validation_error`(400) | `routing_error`(500) | `vendor_unavailable`(503) | `timeout`(504) | `protocol_error`(502) | `rate_limited`(429)；`[routing].retry_after_hint` 开启且全部候选被限流时为 `all_vendors_throttled`(429 + `Retry-After` 头)

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
| `X-Vendor` | 否 | 路由提示：优先使用的厂商 ID |
| `X-Model` | 否 | 路由提示：优先使用的模型 ID |
| `X-Session-Id` | 否 | 会话亲和键；缺失时以 `messages[0]` 前缀哈希兜底 |
| `X-Weight` | 否 | 请求级权重（仅接受有限正数）；生效需 `[routing].allow_request_weight` 开启 |

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

**错误码**: `validation_error`(400) | `routing_error`(500) | `vendor_unavailable`(503) | `timeout`(504) | `protocol_error`(502) | `rate_limited`(429)；`[routing].retry_after_hint` 开启且全部候选被限流时为 `all_vendors_throttled`(429 + `Retry-After` 头)

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

所有 `/admin/*` 端点均要求 Bearer Token 认证，且受入站限流策略约束（`config.toml` 中 `[[rate_limit.rules]]` 的 `path` 前缀规则可覆盖 `/admin/*`；未配置时走 limiteron Governor 内置默认兜底）。

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
    "duration_ms": 523,
    "cost_usd": null
  }
]
```

> `cost_usd` 字段当前恒为 `null`：请求路径构造日志时未写入成本（成本计算仅进内存指标，见 [GET /admin/stats/tokens](#get-adminstatstokens) 注记）。

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
    "completion_tokens": 4000,
    "cost_usd_sum": 0.0
  }
]
```

> `cost_usd_sum` 字段当前恒为 `0.0`（成本不持久化）：API 层写入预聚合器时成本实参恒传 `0.0`，按 `[pricing]` 单价计算出的 USD 成本仅累计进内存指标，未进入 `token_usage` 表。

**错误码**: `database_error`(500)

**curl 示例**:

```bash
curl "http://localhost:8080/admin/stats/tokens?vendor=mock-openai" \
  -H "Authorization: Bearer $API_KEY"
```

---

### GET /admin/metrics

返回全局指标快照（来自 `GlobalMetrics`，包含请求计数、错误率、平均延迟、各厂商细分指标、会话亲和命中率与流量分布熵），并附加日志背压丢弃计数。

**认证**: 需要 Bearer Token

**Query 参数**: 无

**响应**: `MetricsSnapshot`（另加 `dropped_logs` 字段）

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
      "last_error": "Connection reset",
      "ewma_latency_ms": 320.0,
      "ewma_error_rate": 0.02,
      "ewma_samples": 700,
      "ewma_ttft_ms": 210.0,
      "ewma_e2e_ms": 318.0,
      "ewma_queue_ms": 2.5,
      "ttft_samples": 350,
      "e2e_samples": 690,
      "queue_samples": 690
    }
  ],
  "affinity_hits": 420,
  "affinity_misses": 580,
  "affinity_hit_rate": 0.42,
  "top1_share": 0.7,
  "share_entropy": 0.88,
  "dropped_logs": 0
}
```

顶层字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `total_requests` | integer | 总请求数 |
| `successful_requests` | integer | 成功请求数 |
| `failed_requests` | integer | 失败请求数 |
| `error_rate` | number | 错误率 |
| `avg_duration_ms` | number | 平均耗时（毫秒） |
| `vendors` | array | 各厂商细分指标 |
| `affinity_hits` / `affinity_misses` | integer | 会话亲和命中 / 未命中次数 |
| `affinity_hit_rate` | number | 亲和命中率（[0,1]，无样本为 0.0） |
| `top1_share` | number | 滑窗 top-1 厂商流量占比（[0,1]） |
| `share_entropy` | number | 流量份额 Shannon 熵（归一 [0,1]） |
| `dropped_logs` | integer | 日志通道满被丢弃的日志条数 |

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
| `ewma_latency_ms` | number | EWMA 延迟（毫秒，e2e 口径） |
| `ewma_error_rate` | number | EWMA 错误率（[0,1]） |
| `ewma_samples` | integer | EWMA 样本数（0 = 尚无观测） |
| `ewma_ttft_ms` | number | 流式 TTFT EWMA（毫秒，上游响应头近似） |
| `ewma_e2e_ms` | number | 非流式端到端 EWMA（毫秒，含排队） |
| `ewma_queue_ms` | number | 排队段 EWMA（毫秒） |
| `ttft_samples` / `e2e_samples` / `queue_samples` | integer | 三道各自的样本数 |

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
    "queue_length": 0,
    "key_pool": {
      "envs": ["MOCK_OPENAI_KEY"],
      "size": 1,
      "current_index": 0,
      "fingerprints": ["a1b2c3d4e5f60718"]
    }
  },
  {
    "id": "mock-anthropic",
    "status": "Busy",
    "queue_length": 3,
    "key_pool": null
  }
]
```

`status` 为 `VendorStatus` 枚举的 `Debug` 输出，可能值：`Available` | `Busy` | `Throttled` | `Unavailable`。

`key_pool` 为厂商 key 池观测信息（`KeyPoolInfo`：env 名列表 / 解析成功的 key 数量 / 当前游标下标 / 各 key 的 16 位十六进制指纹，**绝不暴露 key 原文**）；厂商无 key 池信息时为 `null`。

状态判定为实时计算：厂商被限流窗口挡住时表现为 `Throttled`；熔断器（`[resilience]` 特性）处于 Open 且探测窗口未到时表现为 `Unavailable`（探测窗口已过则放行探测流量）；厂商 ID 不存在或 `enabled = false` 同样表现为 `Unavailable`。

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
  "queue_length": 0,
  "key_pool": {
    "envs": ["MOCK_OPENAI_KEY"],
    "size": 1,
    "current_index": 0,
    "fingerprints": ["a1b2c3d4e5f60718"]
  }
}
```

当厂商 ID 不存在时，`status` 返回 `"Unavailable"`。状态判定语义同 [GET /admin/vendors](#get-adminvendors)；`key_pool` 字段语义同上。

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

返回当前运行时配置（完整 `Config` 结构，共 15 个顶层段：`server`、`database`、`vendors`、`global_model`、`pricing`、`model_catalog`、`auth`、`cors`、`retry`、`routing`、`cache`、`rate_limit`、`resilience`、`admission`、`grpc`；其中 `model_catalog` 为空目录、`grpc` 段缺省时在响应中省略 / 序列化为 `null`）。

**认证**: 需要 Bearer Token

**响应**: `Config`（下例为代表性节选，`routing` 等字段较多的段仅列常用项，以实际返回为准）

```json
{
  "server": {
    "host": "0.0.0.0",
    "port": 8080
  },
  "database": {
    "path": "./data/llm_proxy.db",
    "pool_size": 10
  },
  "vendors": [
    {
      "id": "mock-openai",
      "name": "Mock OpenAI",
      "type_": "openai",
      "api_key_env": "MOCK_OPENAI_KEY",
      "api_base": "http://127.0.0.1:8001/v1",
      "enabled": true,
      "priority": 1,
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
        "max_tokens": 10000000,
        "window_hours": 24,
        "sliding": false
      },
      "concurrency": {
        "requests_per_second": 100,
        "max_concurrent_requests": null,
        "token_weighted": false,
        "token_ticket_min": 1,
        "token_ticket_max": 16384,
        "token_ticket_default": 4096
      },
      "timeout_ms": 60000,
      "stream_ttft_ms": 60000,
      "stream_idle_ms": 30000
    }
  ],
  "global_model": {
    "enabled": true,
    "strategy": "weighted"
  },
  "model_catalog": [
    {
      "model_id": "claude-sonnet-4-20250514",
      "context_window": 200000,
      "input_price": 3.0,
      "output_price": 15.0,
      "cache_read_price": 0.3,
      "cache_creation_price": 3.75,
      "currency": "usd",
      "max_output_tokens": 8192,
      "supports_tools": true,
      "supports_thinking": true
    }
  ],
  "pricing": [
    {
      "vendor_id": "mock-openai",
      "model_id": "gpt-3.5-turbo",
      "input_per_1k_tokens": 0.0005,
      "output_per_1k_tokens": 0.0015
    }
  ],
  "auth": {
    "enabled": true,
    "api_keys": ["<sha256-hex>"],
    "header": {
      "name": "Authorization",
      "prefix": "Bearer"
    },
    "keys": [
      {
        "hash": "<sha256-hex>",
        "tier": "elastic"
      }
    ]
  },
  "cors": {
    "enabled": true,
    "allowed_origins": ["*"],
    "allowed_methods": ["GET", "POST", "OPTIONS"],
    "allowed_headers": ["*"],
    "allow_credentials": false,
    "max_age": 86400
  },
  "retry": {
    "max_retries": 3,
    "retryable_status_codes": [429, 500, 502, 503],
    "initial_delay_ms": 1000,
    "factor": 2.0,
    "max_delay_ms": 16000,
    "jitter": 0.2,
    "spread_jitter": 1.0,
    "max_total_attempts": 8,
    "retry_budget_secs": 0,
    "amplification_gate": false
  },
  "routing": {
    "w_model": 0.4,
    "w_queue": 0.3,
    "w_window": 0.3,
    "w_latency": 0.0,
    "w_error": 0.0,
    "allow_request_weight": false,
    "retry_after_hint": false
  },
  "cache": {
    "enabled": false,
    "ttl_secs": 300
  },
  "rate_limit": {
    "rules": []
  },
  "resilience": {
    "cover_all_paths": false
  },
  "admission": {
    "enabled": false,
    "congestion_threshold": 0.9,
    "alpha_slo": 2.0,
    "alpha_burst": 1.0,
    "alpha_debt": 4.0
  },
  "grpc": null
}
```

`vendors[].rate_limit` 为 tagged enum，支持三种形态：

```json
{"type": "time_window", "start": "00:00", "end": "06:00"}
{"type": "interval", "hours": 24, "sliding": false}
{"type": "token", "max_tokens": 10000000, "window_hours": 24, "sliding": false}
```

> `[auth]`、`[cors]`、`[grpc]` 三段在配置文件中缺省时序列化为 `null`；`pricing` 缺省为空数组 `[]`；`model_catalog` 为空目录时整体省略（`skip_serializing_if`）。vendor 条目配置了多 key 池时含 `api_key_envs` 数组（只存环境变量名，空时省略）。

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

**请求体**: 完整的 `Config` 结构（同 [GET /admin/config](#get-adminconfig) 响应，15 个顶层段）

**响应**:

```json
{
  "success": true,
  "message": "配置已更新；vendors/pricing/auth 立即生效",
  "restart_required": [],
  "applies_to_new_instances": []
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `success` | boolean | 固定 `true` |
| `message` | string | 更新结果描述（随 locale） |
| `restart_required` | array | 需重启才能生效的段名列表 |
| `applies_to_new_instances` | array | 仅对新建 vendor 实例生效的配置键列表（当前仅 `resilience.circuit_open_timeout_ms`——vendor 实例按 id 惰性创建后进程级缓存，熔断窗口变更不对既有实例生效） |

> 生效语义：`server` 或 `cors` 段变更时，`restart_required` 包含对应段名（`"server"` / `"cors"`），`message` 会提示这些段需重启后生效——两段在建站期被固化（axum Router 的 layer 在构造时编译），运行期不可热换；`resilience.circuit_open_timeout_ms` 变更时列入 `applies_to_new_instances`（对新建 vendor 实例生效）；其余段（vendors/pricing/auth 等）热生效。`[rate_limit]` 段在 Router 构造期消费，热更新不生效（当前实现未列入 `restart_required` 提示，重启后生效）。

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

返回模型能力目录（`[[model_catalog]]` 段）的条目列表。

> **BREAKING（T006）**：响应形状由 `GlobalModelConfig` 数组（原 global_model
> 单例包装）变更为 `ModelCatalogEntry` 数组——全局模型策略改经
> [POST /admin/models/:id](#post-adminmodelsid)（`id = "global"`）或
> [GET /admin/config](#get-adminconfig) 读写，不再并入本列表。

**认证**: 需要 Bearer Token

**响应**: `ModelCatalogEntry` 数组（十一字段，字段对齐 agentstem `ModelEntry`；定价单位 =「currency 计价，每 1M tokens」）

```json
[
  {
    "model_id": "claude-sonnet-4-20250514",
    "context_window": 200000,
    "input_price": 3.0,
    "output_price": 15.0,
    "cache_read_price": 0.3,
    "cache_creation_price": 3.75,
    "currency": "usd",
    "max_output_tokens": 8192,
    "supports_tools": true,
    "supports_thinking": true
  }
]
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `model_id` | string | 模型全名（目录主键，全局唯一） |
| `context_window` | u64 | 上下文窗口（tokens），登记条目必须 > 0 |
| `input_price` / `output_price` | f64 | 输入/输出单价（currency，每 1M tokens） |
| `cache_read_price` / `cache_creation_price` | f64 | 缓存读/写单价（同单位；无独立计价填 0） |
| `currency` | string | `usd` \| `cny`（四价共用币种） |
| `max_output_tokens` | u64 \| null | 最大输出 tokens（null = 厂商未公布） |
| `supports_tools` / `supports_thinking` | bool | 能力位 |
| `cache_breakpoints` | object | Anthropic 自动缓存断点注入策略（可选，serde default；缺省 = 用内置默认——启用 / 上限 4 / 锚序 tools→system→last_user）；未配置时该字段在响应中**整体省略**（`skip_serializing_if`） |

条目数上限 1024；未登记模型的能力查询回落 per-flavor 保守默认（openai 128k / anthropic 200k / ollama 4096，见 `config::model_catalog` 文档）。

**错误码**: 无业务错误

**curl 示例**:

```bash
curl http://localhost:8080/admin/models \
  -H "Authorization: Bearer $API_KEY"
```

---

### PUT /admin/models/:id

upsert 单条目录条目（T006）：条目存在则整体替换，不存在则追加。原子 RMW（读-改-校验-落盘在写锁内完成），校验规则（唯一性 / 非零窗口 / 价格有限非负 / 条目上限 / `model_id` 非空白）与配置加载同源，两路同拒。

**认证**: 需要 Bearer Token

**Path 参数**:

| 参数 | 类型 | 说明 |
|---|---|---|
| `id` | string | 模型全名（**权威标识**——body 中 `model_id` 被覆写为该值） |

**请求体**: `ModelCatalogEntry`（同 [GET](#get-adminmodels) 条目形状；`model_id` 以 path 为准）

**响应**:

```json
{
  "success": true,
  "message": "model claude-sonnet-4-20250514 upserted into the catalog"
}
```

**错误码**: `validation_error`(400，`id == "global"` 保留键 / 校验失败) · `config_error`(500，落盘失败)

**curl 示例**:

```bash
curl -X PUT http://localhost:8080/admin/models/claude-sonnet-4-20250514 \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model_id":"claude-sonnet-4-20250514","context_window":200000,"input_price":3.0,"output_price":15.0,"cache_read_price":0.3,"cache_creation_price":3.75,"currency":"usd","max_output_tokens":8192,"supports_tools":true,"supports_thinking":true}'
```

---

### DELETE /admin/models/:id

目录下条（T006）。原子 RMW；条目不存在时显性拒绝（禁止静默成功）。

**认证**: 需要 Bearer Token

**Path 参数**:

| 参数 | 类型 | 说明 |
|---|---|---|
| `id` | string | 要移除的模型全名 |

**响应**:

```json
{
  "success": true,
  "message": "model claude-sonnet-4-20250514 removed from the catalog"
}
```

**错误码**: `validation_error`(400，条目不存在或 `id == "global"` 保留键) · `config_error`(500，落盘失败)

**curl 示例**:

```bash
curl -X DELETE http://localhost:8080/admin/models/claude-sonnet-4-20250514 \
  -H "Authorization: Bearer $API_KEY"
```

---

### POST /admin/models/:id

更新全局模型策略段（`global_model`）。当前仅支持 `id == "global"`，其他 ID 返回 `config_error`。

> **T006**：目录条目管理请用 [PUT](#put-adminmodelsid) / [DELETE](#delete-adminmodelsid) `/admin/models/:id`；`"global"` 为策略段保留键，PUT/DELETE 该键返回 `validation_error`(400)。

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

### GET /api/v1/health

返回服务运行状态。无需认证，且不受限流约束。

> **路径说明**：实际注册路径是 `/api/v1/health`——端点经 sdforge `#[forge(version = "v1", path = "/health")]` 宏注册，版本号自动加为路径前缀（见 `src/api/health.rs`）。裸路径 `/health` **未注册路由**（请求返回 404），但保留在认证白名单与限流白名单中。

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
curl http://localhost:8080/api/v1/health
```

### GET /readyz

就绪探针：聚合 DB 池、`oxcache` 后端、vendor 管理器三项真实组件检查（absorb-sibling-libs G7，只读句柄，不打上游）。全绿 `200 {"status":"ready"}`，任一失败 `503 {"status":"unavailable"}`。**认证**: 无需。**限流**: 豁免。建议仅内网抓取（未认证端点）。

### GET /metrics

Prometheus 文本指标出口（`Content-Type: text/plain; version=0.0.4`），RED 维度（`sdforge_http_requests_total` / `sdforge_http_request_duration_seconds`，路由标签归一）。**认证**: 无需。**限流**: 豁免。建议仅内网抓取。

### 文档面（需 `[docs].enabled = true`，默认关闭）

OpenAPI 文档面（absorb-sibling-libs G11）：`GET /api-docs/openapi.json`（主 REST 端点 + 管理面 + 健康面路径）与 Swagger UI（`/swagger-ui/`）。默认配置下两路由不存在（404）；开关打开后可达，免认证免限流（开关本身即访问门）。

```toml
[docs]
enabled = true
```

---

## 附录：路由提示请求头

LLM 推理端点（`/v1/chat/completions` 与 `/v1/messages` **两个端点均读取**路由提示请求头，见 `src/api/openai.rs` 与 `src/api/anthropic.rs` 中的 `RoutingHints::from_headers` 调用）支持通过请求头影响路由决策：

| Header | 说明 | 生效条件 |
|---|---|---|
| `X-Vendor` | 优先使用的厂商 ID（直连路由，不查厂商实时状态过滤） | 非空即生效 |
| `X-Model` | 优先使用的模型 ID | 非空即生效 |
| `X-Session-Id` | 会话亲和键——同键请求粘住同一成功厂商 | 非空即生效；缺失时自动兜底（见下） |
| `X-Weight` | 请求级权重（浮点数） | 仅接受**有限正数**（非数值 / 负数 / NaN / Infinity 一律忽略）；且需服务端 `[routing].allow_request_weight` 开启（默认 `false`，关闭时忽略） |

**X-Session-Id 兜底规则**：请求未携带 `X-Session-Id` 时，以 `serde_json::to_string(messages[0])` 的前 2KB（UTF-8 字符边界安全截断）计算 SHA256，hex 编码后截取前 16 字符作为会话键——同前缀开头的对话自动粘住同一厂商；请求体无 `messages` 时无兜底键，该请求不参与亲和。

**X-Vendor/X-Model 提示语义**：任一提示头非空时走提示路由（`route_with_hints`），否则走默认路由决策（权重/亲和等策略）。提示仅是"优先"语义，目标厂商执行失败时仍会进入换路 fallback（resilience 特性开启时）。

---

## 相关文档

[用户指南](./USER_GUIDE.md) · [架构文档](./ARCHITECTURE.md) · [FAQ](./FAQ.md) · [README](../README.md)
