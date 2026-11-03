# Fluxweir 性能基线

> 首轮 Criterion 组件级基线（转换 / SSE 帧解析 / 路由决策路径 / 缓存经济学亲和），随 `roadmap-gap-closure` 与 `cache-economy-affinity` 变更建立。
> 数值为 `cargo bench` 实际输出誊录（mean = criterion 点估计中点值，吞吐 = 每迭代元素数 / 时间）；
> 回归对照请在同机、同 profile、同功能配置下复跑后与本页比较。

## 测量口径

| 项 | 值 |
|:---|:---|
| 机器 | AMD Ryzen 9 9950X 16-Core（16 逻辑核），WSL2（kernel `6.6.87.2-microsoft-standard-WSL2`） |
| 工具链 | rustc 1.97.1 / cargo 1.97.1 |
| 代码版本 | HEAD `bc5cc3b`（转换/SSE/路由组）/ `1f3089b` + 本修复批（affinity 组）；基线建立后代码持续演进（当前 HEAD `fbd6a3c`，2026-11-04），数值代表性随演进衰减，比较前请复跑 |
| 基准框架 | criterion 0.8（dev-dependency；关默认特性仅保留 `cargo_bench_support`，统计单线程） |
| 构建 profile | `cargo bench`（bench profile 继承 release：`opt-level=3` / `lto=true` / `codegen-units=1`） |
| 复跑命令 | `cargo bench --bench conversion` / `--bench sse` / `--bench routing` / `--bench affinity`（各基准均 `harness = false`） |
| fixture 基座 | `benches/common/mod.rs`（手写测试专用构造，不依赖网络与外部服务） |

## 转换 conversion

| 基准 | mean | 吞吐 | 覆盖路径 |
|:-----|-----:|-----:|:---------|
| `openai_normalize` | 275.98 ns | 3.62 Melem/s | OpenAI 流式 chunk 稳态归一化（id/created 逐帧重写） |
| `anthropic_request_to_external_with_cache_policy` | 5.5102 µs | 181.48 Kelem/s | 多轮 + system + tools 出站编码 + 缓存断点注入 |
| `anthropic_encode_chunk_tool_use_sequence` | 3.3679 µs | 1.19 Melem/s | tool_use 片段序列（首片段 → 参数分片 → 终止）流帧编码 |
| `inject_cache_control` | 3.4939 µs | 286.22 Kelem/s | 长多轮请求体三锚点（tools / system / last_user）注入纯函数 |

## SSE 帧解析 sse

| 基准 | mean | 吞吐 | 覆盖路径 |
|:-----|-----:|-----:|:---------|
| `openai_parse_sse_frame_to_chunk` | 1.6132 µs | 3.10 Melem/s | 尾段 5 帧（data: 前缀 + `[DONE]` 终止帧） |
| `anthropic_parse_sse_frame_to_chunk` | 1.4827 µs | 4.72 Melem/s | 尾段 7 帧（event:/data: 前缀 + message_stop 收束） |
| `restore_off_passthrough` | —（`masking` feature 门控组，首轮未记录） | | 掩码关闭时流式回程原样透传 |
| `restore_on_placeholder_stream` | —（`masking` feature 门控组，首轮未记录） | | 掩码开启时占位符流式还原 |

> 透传路径以帧解析为组件级代理：入站协议与上游一致时走零拷贝透传，流式内存热点在逐帧解析。`benches/sse.rs` 另含 `masking` feature 门控的 `sse_masking` 组（`restore_off_passthrough` / `restore_on_placeholder_stream`），随 `--features masking` 编译与运行。

## 路由决策 routing

| 基准 | mean | 吞吐 | 覆盖路径 |
|:-----|-----:|-----:|:---------|
| `route_weighted` | 3.4751 µs | 287.76 Kelem/s | 固定 5 vendor × 多权重 vendor 表加权决策（可选层全关；含 tokio `block_on` 进入开销） |

## 缓存经济学亲和 affinity

`cargo bench --bench affinity -- --warm-up-time 0.5 --measurement-time 2 --sample-size 30` 实测：

| 基准 | mean | 吞吐 | 覆盖路径 |
|:-----|-----:|-----:|:---------|
| `decide_auto` | 5.2446 ns | 190.67 Melem/s | 纯决策：模式分派 + 阈值比较（零 IO、零锁） |
| `worth_silent_cache` | 758.65 ps | 1318.13 Melem/s | 沉默缓存 worth 三档饱和加法 |
| `settle_auto_stick` | 888.75 ns | 1.13 Melem/s | settle 全路径（读锁快照 + 决策 + oxcache set + 粘性记账） |
| `settle_auto_below_threshold` | 80.382 ns | 12.44 Melem/s | settle 全路径（低于阈值：读锁 + 决策，无写入） |
| `settle_auto_with_sink` | 891.46 ns | 1.12 Melem/s | settle 全路径带落盘通道（`try_send` + 丢弃差值对账） |

> 结论：纯决策 5 ns 量级，热路径每次响应结算的固定开销可忽略；主要成本在亲和表写入（oxcache set，~0.8 µs）与粘性记账。带 sink 相对不带 sink 的增量在噪声内（891 ns vs 889 ns）——通道是 `try_send`，满即丢，不阻塞响应路径。`below_threshold` 只做决策与漂移计数（80 ns），即「不值得粘」的会话几乎零成本。

## 边界与说明

- **组件级口径**：不起完整 axum 服务、不建 mock 上游；HTTP 级端到端吞吐 / 延迟压测另行规划。
- **fixture 维护**：`benches/common` 与生产结构体字段耦合，协议结构演进时需同步维护。
- **CI 门禁**：首轮只人工记录，不在 CI 跑 bench（时长与资源权衡）；CI bench smoke 与基线回归告警登记于变更 Deferred，待本页积累两轮以上基线后重开评估。
- **跨机器局限**：数值含本机与 WSL2 口径，跨机器比较需注明环境差异。
