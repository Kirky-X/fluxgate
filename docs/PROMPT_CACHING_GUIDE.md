# Prompt 前缀缓存工程规范

> 面向接入 NexusFlow 的调用方：厂商端前缀缓存是**字节级精确匹配**，前缀一个字不同即全价重算。本指南描述如何最大化缓存命中、降低 input 成本（主流厂商缓存命中价约为全价的 1/10~1/2）。

## 规则

1. **system prompt 逐字节稳定**：内容、字段顺序、空格都参与哈希。不要拼接时间戳、请求 ID、随机数。
2. **动态内容后置**：把变化的部分（用户输入、检索结果）放在消息尾部，稳定部分（指令、few-shot、工具定义）放在头部。
3. **多轮对话保持前缀追加式**：第 N 轮的 messages 必须以第 N-1 轮的完整 messages 为前缀，只追加不改动。
4. **工具定义放头部且不重排**：tools 序列化顺序参与前缀。
5. **同一任务流固定会话键**：`X-Session-Id` + 网关前缀亲和（`[routing.prefix_affinity]`）让同前缀请求黏住同一厂商。

## 网关侧配套

- `[routing.prefix_affinity].enabled = true`：同前缀请求黏住 ≤2 家厂商（PoTC 双哈希，兼顾负载分散）。
- `[routing] price_factor_mode = "cache_expected"`：选路自动偏向缓存定价友好的厂商。
- 命中观测：`/admin/metrics` 的 `cache_read_tokens_sum / cache_input_tokens_sum`。

## 历史版本

- 记录变更历史、生成时间、参数配置。
- 输出报告（结论、数据支撑、置信度）。

## 实测结论（2026-10-15，真实厂商验证）

- **Zhipu 直连（glm-4.5-flash，免费档）**：同前缀重复请求 1340 token 中命中 1330（**99.3%**），延迟 1136ms → 568-883ms；实测到一次缓存逐出后自动重建（命中归零再恢复）。回包 `usage.prompt_tokens_details.cached_tokens` 逐请求可观测——O2 观测链路对它有效。
- **聚合器类厂商（如 Agnes apihub）**：回包带 `cached_tokens` 但**重复前缀命中恒为 256/1497（17%）不增长**——聚合器内部二次负载均衡把请求散射到它自己的上游实例，网关层亲和控制不到。**缓存敏感流量应优先直连厂商官方端点**，聚合器只作溢出/备用。
- 接入 Zhipu 类 `/v4` 路径厂商：`[[vendor]]` 配 `chat_path = "/chat/completions"`（跳过网关 `/v1` 自动拼接），无需本地转发垫片。
