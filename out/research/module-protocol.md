# 协议转换模块研读笔记（src/protocol/）

> 调研日期：2026-09-30。范围：`src/protocol/`（mod.rs / unified.rs / openai_converter.rs / anthropic_converter.rs / embedding_converter.rs，共 1932 行）。
> 所有结论均基于本目录源码通读，并追到了 api 层（`src/api/openai.rs`、`src/api/anthropic.rs`、`src/api/sse.rs`）与 vendor 层（`src/vendor/manager.rs`）的实际调用点；每条结论附 file:line 证据。

---

## 1. 模块职责

`src/protocol/` 是 NexusFlow 的协议中立层：在 OpenAI Chat Completions 与 Anthropic Messages 两种线格式之间，经由一个统一中间表示（UnifiedRequest/UnifiedResponse）做双向转换，并对外提供"同协议直通（透传）"的判定接口。

- `mod.rs:21-25`：`Protocol` 枚举只有 `OpenAI | Anthropic` 两个变体。
- `mod.rs:27-68`：`ProtocolConverter` trait，约定 5 个方向性转换：`to_unified`（入站→IR）、`request_to_external`（IR→出站请求）、`parse_response`（出站响应→IR）、`response_to_external`（IR→出站响应）、`stream_chunk_to_external`（IR 流块→出站流块）；另有 3 个带默认实现的透传方法（`can_passthrough` / `passthrough_request` / `passthrough_response`，mod.rs:48-67）。
- 两个实现均为零字段 unit struct：`OpenAIConverter`（openai_converter.rs:9）、`AnthropicConverter`（anthropic_converter.rs:9），无状态、天然 Send+Sync。
- `embedding_converter.rs:9-52`：Embedding 请求转换（OpenAI 格式自转，仅请求方向）；`embedding_converter.rs:55-113`：Embedding 响应→`UnifiedEmbeddingResponse`。

模块内**没有任何** `#[cfg(test)]` 单元测试；测试全部在 `tests/protocol_test.rs`（60 个 `fn test_`，grep 实测计数 60）。AGENTS.md 标注的"7 tests"与实际不符。

## 2. 数据流（实路，非设计意图）

API 层两条对称的入口（`/v1/chat/completions` 与 `/v1/messages`）各自按 vendor 类型分四条路径：

```
入站 Json<Value>（axum Json 提取器已完成一次完整反序列化）
  │
  ├─ 同协议非流式：透传路径
  │    body.clone() → execute_passthrough_request → builder.json(&body) 再序列化 → response.json() → 原样 Json 返回
  │    （openai.rs:52-108；anthropic.rs:36-92）
  │
  ├─ 同协议流式：透传路径
  │    body.clone() → execute_passthrough_stream_request → bytes_stream 原样回吐（无重帧）
  │    （openai.rs:56-88；anthropic.rs:40-72；manager.rs:779-819）
  │
  ├─ 跨协议非流式：全量 IR 转换
  │    to_unified(body.clone()) → execute_request（进队列/限流/重试/CB）
  │      → request_to_external → HTTP → response.json() → parse_response → response_to_external
  │    （openai.rs:112/166-172；anthropic.rs:96/134-140；manager.rs:338-369, 474-561）
  │
  └─ 跨协议流式：★未转换，上游字节流直接转发★
       execute_stream_request 同样只做 request_to_external，
       返回 response.bytes_stream() 原始字节（manager.rs:626-683，具体 682 行）
       - openai.rs:121-137：把每个网络字节块包一层 "data: <chunk>\n\n" 后回吐
       - anthropic.rs:104-107：字节流原样回吐，连重帧都没有
```

关键事实：**`stream_chunk_to_external` 与 `StreamChunk` IR 在生产路径上零调用**。全仓 grep `stream_chunk_to_external` 只命中 trait 定义（mod.rs:42-45）与两个实现（openai_converter.rs:308、anthropic_converter.rs:345），无任何调用方；`StreamChunk` 仅被 tests/protocol_test.rs:1070-1124 的 serde 测试引用。即：跨协议流式转换是"设计了 IR 但没接线"的状态。

同样，`can_passthrough` / `passthrough_request` / `passthrough_response` 三个 trait 方法（mod.rs:48-67）也无任何调用方——真正的透传判定是 api 层的字符串硬编码：`decision.vendor.type_ == "openai"`（openai.rs:52）、`== "anthropic"`（anthropic.rs:36）。

Embedding 路径（openai.rs:237-344）**完全绕过** vendor manager：`reqwest::Client::new()` 现建客户端（openai.rs:281），无限流、无队列、无重试、无熔断；`embedding_response_from_openai`（embedding_converter.rs:55）同样只有测试调用，端点直接把上游 `response_json` 原样返回（openai.rs:344）。

## 3. 关键数据结构（unified.rs）

| 结构 | 位置 | 说明 |
|---|---|---|
| `UnifiedRequest` | unified.rs:8-36 | 手写扁平字段：model/messages/temperature/max_tokens/stream/stop/top_p/penalties/logit_bias/user/tools/tool_choice。**没有** top_k、n、response_format、seed、logprobs、metadata 的容身之处——这些字段在任何转换路径上都只能被丢弃 |
| `Role` | unified.rs:58-68 | system/user/assistant/tool 四值 |
| `MessageContent` | unified.rs:185-200 | `#[serde(untagged)]` 五变体：Text / MultiModal(Vec<ContentBlock>) / ToolCalls(Vec<ToolCall>) / ToolResult(ToolResult) / TextWithToolCalls。untagged 意味着 serde 匹配按尝试顺序，且 TextWithToolCalls 是唯一 struct 变体却标了 `rename_all = "camelCase"`（unified.rs:195），与全库 snake_case 风格不一致 |
| `ContentBlock` | unified.rs:209-220 | Text / Image / 以及一个 `Type(String)` 兜底变体。实际解析走手写函数，不走 serde；兜底变体在序列化侧被映射成 `{}` 或 `{"type":"unknown"}`（openai_converter.rs:501、anthropic_converter.rs:493） |
| `ImageSource` | unified.rs:223-229 | {type_, media_type, data}——OpenAI 的 URL 也被塞进 `data`（openai_converter.rs:460-465），回程再拼 `data:{media_type};base64,{data}`（openai_converter.rs:497），**URL 走一轮回程会被错误加上 base64 前缀** |
| `ToolCallFunction.arguments` | unified.rs:162-167 | 参数以 JSON **字符串**保存（沿 OpenAI 约定）；Anthropic 回程需 `serde_json::from_str` 再解析（anthropic_converter.rs:504, 524），解析失败静默落 `Null` |
| `StreamChunk` | unified.rs:274-292 | 内部 tag=`type` 的三变体 IR：content/usage/heartbeat。与 OpenAI、Anthropic 的真实 SSE 格式都不同构，是纯内部抽象 |
| `Choice` | unified.rs:251-260 | message/finish_reason/logprobs；logprobs 在 OpenAI parse_response 中恒置 None（openai_converter.rs:259） |
| `TokenUsage` | unified.rs:263-271 | prompt/completion/total |

工具 schema 表示（`Tool`/`ToolFunction`/`ToolParameters`/`ToolParameterProperty`，unified.rs:70-142）是**单层扁平化**的：每个 property 只保留 type/description/enum 三个标量字段，JSON Schema 的嵌套结构（嵌套 object、array+items、anyOf、additionalProperties 等）无处安放，转换即丢失（证据见 §5.8）。

## 4. 关键算法与转换逻辑

### 4.1 解析风格：手写字段抽取，而非 serde derive
两个 `to_unified` 都是逐字段 `body.get(..).and_then(..)` 手工抽取（openai_converter.rs:33-103；anthropic_converter.rs:33-114），配合大量 `filter_map` 的宽松解析。效果：
- 缺 model/messages 报 `Validation{field}`（openai_converter.rs:34-50）；OpenAI 侧未知 role 报错（openai_converter.rs:359-364），Anthropic 侧未知 role 静默默认 `user`（anthropic_converter.rs:418-425）——两个转换器严格性不对称。
- 数值一律 `f64 → as f32`（openai_converter.rs:67-68 等）、`u64 → as u32`（openai_converter.rs:71-72），静默截断。
- `model` 强制 `.to_lowercase()`（openai_converter.rs:40、anthropic_converter.rs:40）——大小写敏感的模型 ID 会被改写。

### 4.2 消息内容形状机（核心 switch）
`message_to_openai`（openai_converter.rs:476-556）与 `message_to_anthropic`（anthropic_converter.rs:469-542）按 `MessageContent` 五变体分别产出两种线格式：
- IR ToolResult → OpenAI：`{"role":"tool","content":<str>,"tool_call_id":..}`（openai_converter.rs:522-525 + 551-553）；→ Anthropic：`[{"type":"tool_result","tool_call_id":..,"content":..}]` 且 role 强制改 "user"（anthropic_converter.rs:509-513, 535）。
- IR ToolCalls → OpenAI：content 位置直接放 tool_calls 数组（openai_converter.rs:506-521，**这不是合法 OpenAI 消息形状**——OpenAI 的 tool_calls 是消息顶层字段）；→ Anthropic：`[{"type":"tool_use","id","name","input":<解析后的对象>}]`（anthropic_converter.rs:498-508）。
- TextWithToolCalls → OpenAI：文本块与 tool_call 块混进同一个 content 数组（openai_converter.rs:526-542，同样不合 OpenAI 规范）；→ Anthropic：text 块 + tool_use 块合并数组（anthropic_converter.rs:514-528，这个形状倒是接近 Anthropic 真实格式）。

### 4.3 工具 schema 扁平化
`parse_openai_tool`（openai_converter.rs:604-682）与 `parse_anthropic_tool`（anthropic_converter.rs:545-622）是近乎逐行复制的两份实现（~140 行重复），均只读 `properties` 一层的 type/description/enum；`tool_to_openai`（openai_converter.rs:685-724）与 `tool_to_anthropic`（anthropic_converter.rs:625-661）同样互为镜像。property 缺 "type" 时整条 property 被 `?` 静默丢弃（openai_converter.rs:624）。

### 4.4 零拷贝透传：判定与实现
- 判定：api 层字符串比较 vendor 类型（openai.rs:52、anthropic.rs:36），不经过 trait。
- 实现：`execute_passthrough_request`（manager.rs:687-776）——请求体是 `Value`，经 `builder.json(&request_body)` 重新序列化发送；响应 `response.json()` 反序列化成 `Value` 后原样 `Json(response)` 返回。token 统计只认 `usage.total_tokens`（manager.rs:769-773）。
- **实质是"零转换"而非"零拷贝"**：完整链路仍是 axum Json 提取器反序列化一次 + `body.clone()` 深拷贝一次（openai.rs:59/93、anthropic.rs:43/77）+ 发送前再序列化一次 + 响应反序列化一次。真正意义上的零拷贝（raw bytes 直通）只在同协议**流式**透传路径上成立（bytes_stream 原样回吐，manager.rs:818）。
- trait 上的 `passthrough_request`/`passthrough_response`（mod.rs:54-67，`Ok(body)` 恒等函数）是这层语义的文档化残留，无调用方。

### 4.5 流式处理方式（回答重点问题：逐块还是全量缓冲？）
**都不是——是"不解析、直接转发字节块"**：
- 上游侧：`execute_stream_request` 不解析任何 SSE 事件，`response.bytes_stream()` 按 TCP/HTTP2 分块原样交还（manager.rs:682）；无事件边界缓冲、无 `[DONE]` 处理、无重试/熔断/token 记账（对照非流式路径 manager.rs:474-561 的重试循环）。
- 下游侧两种处理：openai 转换路径把**每个网络字节块当作一个完整 SSE 事件**重新包帧 `data: {chunk}\n\n`（openai.rs:124-126 + sse.rs:17-22），并用 `String::from_utf8_lossy` 解码（openai.rs:124）——字节块边界由网络决定，与 SSE 事件边界、UTF-8 字符边界都对不齐；上游如果本来就是 SSE 文本（含内部换行的 `event:`/`data:` 多行），会被整体塞进一个 `data:` 行，帧结构被破坏。anthropic 转换路径则连重帧都没有（anthropic.rs:104-107）。
- `StreamChunk`/`stream_chunk_to_external` 这套"逐事件转换"的 IR **从未接入**（§2 已证）。且即便接入，其输出本身也不构成合法流：Anthropic 侧只发 `content_block_delta`（无 `message_start`/`content_block_start`/`content_block_stop`/`message_stop`，无 `index`，还把 `delta` 与 `content_block` 两个字段塞进同一事件，anthropic_converter.rs:389-400）；OpenAI 侧 chunk 的 id 是伪造的 `chunk-{index}`、model 恒为 "unknown"（openai_converter.rs:315-318）；工具调用 delta 在两侧都退化为空文本（openai_converter.rs:568 `ToolCalls(_) => json!({"content":""})`；anthropic_converter.rs:380 同样）。

## 5. 发现的问题与短板（按影响排序）

### P0-1 跨协议流式不转换，且存在帧错位与 UTF-8 劈裂
证据：manager.rs:682（bytes_stream 原样返回）、openai.rs:121-137（按网络块重帧 + `from_utf8_lossy`）、anthropic.rs:104-107（原样转发）、`stream_chunk_to_external` 无调用方（mod.rs:42-45 仅定义）。
影响：OpenAI 客户端指向 Anthropic vendor（或反之）时，收到的是对方协议的原始事件（可能还被二次包帧破坏）；多字节字符跨块时 `from_utf8_lossy` 产生 U+FFFD。跨协议 + stream 的组合在当前代码里必然产出错误协议的响应。

### P0-2 工具调用跨协议链路四处断裂
1. 入站 Anthropic 消息里的 `tool_use`/`tool_result` 块被 `parse_anthropic_block` 静默丢弃（只认 text/image，anthropic_converter.rs:447-467，配合 430-434 的 filter_map）——Anthropic 客户端多轮工具对话经转换路径后历史全丢。
2. 入站 OpenAI assistant 消息 `content` 为字符串且带顶层 `tool_calls` 时，`parse_openai_message` 在 `Some(Value::String)` 分支提前返回 `Text(s)`，顶层 tool_calls 不再看（openai_converter.rs:366-384；顶层 tool_calls 只在 `None | Some(Value::Null)` 分支处理，426-439）——"先说话再调工具"的历史丢工具调用。
3. `OpenAIConverter::parse_response` 只解析字符串 content，不解析响应里的 `tool_calls`（openai_converter.rs:228-248）——Anthropic 客户端 → OpenAI vendor 的工具调用响应在 IR 阶段就被抹掉。
4. 字段名错位：Anthropic `tool_result` 块的关联字段官方为 `tool_use_id`，代码写出的是 `tool_call_id`（anthropic_converter.rs:509-513）；`tool_choice` 双向都按 OpenAI 形状处理（入站只认 `obj["function"]`，anthropic_converter.rs:60-79，真实的 Anthropic `{"type":"tool","name":..}` 会落成 Strategy("auto")；出站 Specific 映射成 `{"type":"function","function":{..}}`，anthropic_converter.rs:136-144，而非 Anthropic 的 `{"type":"tool","name":..}`；Strategy 序列化为裸字符串而非 `{"type":"auto"}`，anthropic_converter.rs:137-138）。

### P0-3 Anthropic 顶层 system 字段双向丢失
`AnthropicConverter::to_unified` 从不读 `body["system"]`（全仓 grep `get("system")` 零命中；字段清单 anthropic_converter.rs:81-113），system prompt 在 Anthropic 入站方向被整段丢弃；反向 `message_to_anthropic` 把 Role::System 写成 messages 数组内的 `{"role":"system"}`（anthropic_converter.rs:531-536），且从不写顶层 `system` 参数——与它自己宣称产出的 Anthropic Messages 格式不一致（Anthropic 规范中 system 是顶层参数而非消息角色）。

### P1-4 stop_reason / finish_reason 不做词汇映射
Anthropic 的 `end_turn`/`max_tokens`/`tool_use`/`stop_sequence` 与 OpenAI 的 `stop`/`length`/`tool_calls` 互不翻译，双向原样透传（anthropic_converter.rs:179 出站、307-310 入站）。跨协议客户端会看到 foreign 词汇（OpenAI 客户端收到 `finish_reason:"end_turn"`，Anthropic 客户端收到 `stop_reason:"stop"`）。

### P1-5 静默丢数据的多处兜底
- 纯文本多块响应只取第一块：anthropic_converter.rs:286-298（`arr[0]`），而含 tool_use 的路径却是全量 join（261-265, 273）——同一函数内不一致。
- 未知 content 块：OpenAI 出站写 `{}`（openai_converter.rs:501）、Anthropic 出站写 `{"type":"unknown"}`（anthropic_converter.rs:493）——不报错、不留痕。
- OpenAI `stop` 只接受数组形式，字符串形式的 stop 静默丢弃（openai_converter.rs:77-85）。
- Anthropic 入站 `top_p` 不解析（top_p/frequency_penalty/presence_penalty/logit_bias/user 恒空，anthropic_converter.rs:106-110），出站也不写 top_p。
- `Choice.logprobs` 恒 None（openai_converter.rs:259）。

### P1-6 "零拷贝"语义名不副实 + trait 抽象脱节
见 §4.4/§2：判定逻辑硬编码在 api 层（openai.rs:52、anthropic.rs:36），trait 三个透传方法零调用（mod.rs:48-67）；非流式透传路径仍有 2 次反序列化 + 2 次序列化 + 每请求 2 次 `body.clone()` 深拷贝（openai.rs:59/93；anthropic.rs:43/77——clone 只是为了日志复用）。此外透传路径与非流式转换路径的横切能力不一致：透传路径无限流队列（直接 acquire，manager.rs:723）、无重试、无熔断、metrics 只在非流式透传记录（manager.rs:697-704，流式两个入口都不记）。

### P1-7 流式路径未做事件边界缓冲（逐网络块 ≠ 逐事件）
openai.rs:124-126 把任意字节块当一个 SSE 事件；正确做法需要按 `\n\n` 分帧缓冲。anthropic.rs 转换路径甚至不缓冲。这是 P0-1 的机制根因，单列是因为即使不做协议转换（同格式不同 vendor 的网关），帧错位 bug 也存在。

### P2-8 工具 schema 只支持一层，嵌套即丢
`ToolParameterProperty` 只有 type/description/enum（unified.rs:131-142）；解析侧 property 内其他键全被丢弃（openai_converter.rs:619-640、anthropic_converter.rs:567-590），property 缺 type 时整条丢弃（openai_converter.rs:624 的 `?`）。现代工具定义普遍含嵌套 object/array schema，跨协议转换后工具签名退化。

### P2-9 双份镜像实现，~140 行重复
`parse_openai_tool` ↔ `parse_anthropic_tool`（openai_converter.rs:604-682 ↔ anthropic_converter.rs:545-622）、`tool_to_openai` ↔ `tool_to_anthropic`（openai_converter.rs:685-724 ↔ anthropic_converter.rs:625-661）、`ToolParameters` 构造逻辑两处全同。加第三个协议时成本翻倍。

### P2-10 模型名强制小写
openai_converter.rs:40、anthropic_converter.rs:40：`.to_lowercase()` 无条件执行。对大小写敏感的模型标识（部分开源模型命名如 `Meta-Llama-…`、网关型 ID）会改写语义，且没有任何地方说明这是刻意归一化。

### P2-11 Embedding 转换器半死 + 端点旁路
`embedding_response_from_openai`/`UnifiedEmbeddingResponse` 仅测试引用（grep 全仓，§2）；实际端点原样回传上游 JSON（openai.rs:344）；请求转换后仍走完整序列化/反序列化（openai.rs:270-284），而 embedding 本是 OpenAI→OpenAI 的同格式场景，最该透传；且该端点自建 `reqwest::Client::new()`（openai.rs:281），绕过限流/队列/重试/熔断/metrics 全套设施，client 不复用连接池。入站 input 数组中的非字符串项（如 token 数组）被 `filter_map(as_str)` 静默剔除（embedding_converter.rs:22-30），非字符串非数组输入静默变空数组（28-30）而非报错。

### P2-12 统一 IR 缺字段导致的能力上限
UnifiedRequest 无 n / top_k / response_format / seed / metadata / logprobs 请求侧字段（unified.rs:8-36），ContentBlock 无 audio/file 等模态（unified.rs:209-220），`ImageSource` 把 OpenAI 的 URL 塞进 `data` 字段并在回程错误地加 base64 前缀（openai_converter.rs:460-465 → 494-499）。这些都属结构性缺失，扩字段即动 IR。

### P3-13 其他小项
- `protocol` feature 门控形同虚设：lib.rs:8 `pub mod protocol;` 无 `#[cfg(feature)]`，Cargo.toml:23 `protocol = []` 空特性（被 database 特性依赖，Cargo.toml:20，但并未 gate 任何代码）。
- `message_to_openai` 对 ToolCalls/TextWithToolCalls 产出的 content 数组形状不是 OpenAI 规范形状（§4.2，openai_converter.rs:506-521, 526-542）——即使上游兼容，也属于协议形状错误。
- 转换错误信息只有字段名（`NexusFlowError::Validation{field}`，error.rs:36），无上下文（原始值/路径），排障困难。
- OpenAIConverter/AnthropicConverter 选择逻辑在 manager 内两处 match 重复（manager.rs:346-354、656-664）。
- `async_trait` 用于纯同步 trait（mod.rs:27），无意义开销（每方法多一层 Box 装箱的虚表调用说明，虽然本例全是同步签名，async_trait 实际不改变签名，属多余依赖标注）。

## 6. 优化切入点（供后续调研排序）

1. **补全跨协议流式管线**（对应 P0-1/P1-7）：在 `execute_stream_request` 返回处（manager.rs:626-683）接一层 SSE 事件边界缓冲（按 `\n\n` 分帧 + 跨块 byte buffer），再接 `StreamChunk` 解析与 `stream_chunk_to_external`——IR 已存在，缺的是"上游 SSE → StreamChunk"的反向解析器和接线。这是收益最大的一块。
2. **修工具调用四断点**（P0-2）：`parse_anthropic_block` 支持 tool_use/tool_result；`parse_openai_message` 在 String 分支合并顶层 tool_calls → `TextWithToolCalls`；`parse_response` 补 tool_calls 解析；`tool_call_id`→`tool_use_id`、tool_choice 双向按 Anthropic 形状。
3. **system prompt 顶层面字段**（P0-3）：`to_unified` 读 `body["system"]` 转为 Role::System 消息或 IR 新增 system 字段；`message_to_anthropic` 把 System 消息提升到顶层 `system` 参数。
4. **词汇映射表**（P1-4）：stop_reason ↔ finish_reason 的静态映射。
5. **透传语义收敛**（P1-6）：让 `can_passthrough` 真正被调用（按 `(input_protocol, vendor_output_protocol)` 查表），api 层删字符串硬编码；非流式透传考虑 `Body`/raw bytes 直通（避免 Json 提取 + clone + 重序列化），或至少去掉日志复用导致的深拷贝（`Arc<Value>` 或先序列化日志串）。
6. **工具 schema 去重 + 支持嵌套**（P2-8/9）：抽公共 schema 树（直接以 `serde_json::Value` 或轻量递归结构保存 parameters），OpenAI/Anthropic 只做包皮差异。
7. **Embedding 端点回归 manager**（P2-11）：走 vendor manager 获得限流/重试/metrics，或至少复用共享 reqwest::Client；响应侧要么用 `UnifiedEmbeddingResponse` 要么删掉死代码。
8. **停止小写化模型名**或仅对已知名录归一化（P2-10）。

## 7. 已核实为"未实现/未运行"的项（避免后续误判）

- 跨协议流式转换：未接线（无任何上游 SSE→StreamChunk 解析代码）。
- `stream_chunk_to_external`、`can_passthrough`、`passthrough_request`、`passthrough_response`、`embedding_response_from_openai`：生产路径零调用（grep 全仓核实），仅测试触达。
- `execute_stream_request` 路径无重试、无熔断、无 token 记账、无 metrics（manager.rs:626-683 与 697-704 对照）。
- 模块内无单元测试；集成测试 60 个集中在 tests/protocol_test.rs，覆盖的是转换函数本身的快乐路径（工具、tool_result、tool_choice、embedding、IR serde roundtrip），未覆盖上述任何一个断裂点（无跨协议 stream 测试、无 system 字段测试、无 stop_reason 映射测试）。
