# 来源核实 — 建议 3（入站限流：config.toml [rate_limit] 段被丢弃，Governor 跑库内置默认规则）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（三个来源全部属实，引用一致；gap 本地代码行号全部命中）**

## 来源 1：Kong AI Gateway (ai-rate-limiting-advanced) — ✅ 属实

- 命令：WebFetch `https://developer.konghq.com/ai-gateway/policies/ai-rate-limiting-advanced/`
- 多 limit 多窗口叠加：`config.policies[].limits[]` 可配多条限制（文档示例同屏给 60s/100 与 3600s/1000 两窗口），原文 "If more than one limit is set, the AI Rate Limiting Advanced Policy returns multiple time limit headers"。
- partition_by 按模型分桶：示例 `- type: model, partition_by: true, values: [gpt-4o]`；文档明确 "you must set the `config.policies.match.partition_by` field to `true`, otherwise the policy is not enforced"。
- 匹配维度：consumer / consumer group / IP / header / path / model / provider（AND 组合）。
- 与建议引用的"多 limit 多窗口叠加 + partition_by 按模型分桶"吻合；其 per-endpoint/path 粒度确实是建议要对齐的能力基准。

## 来源 2：Cloudflare AI Gateway (rate-limiting) — ✅ 属实

- 命令：WebFetch `https://developers.cloudflare.com/ai-gateway/features/rate-limiting/`
- fixed vs sliding 对外语义逐字命中："You can also select if you would like a fixed or sliding rate limiting technique"；固定窗口 "no more than x requests in a ten minute window" vs 滑动窗口 "no more than x requests in the last ten minutes"（文档以 12:09/12:11 各 10 请求示例两算法差异）。
- 超限行为："the server will respond with a 429 Too Many Requests status code and your request will not be processed"。

## 来源 3：limiteron（本地兄弟库 /home/dev/projects/limiteron）— ✅ 属实

NexusFlow Cargo.toml:60 path 依赖，直接读源码：

- `src/governor.rs:1664` `pub async fn apply_config(&self, new_config: FlowControlConfig) -> Result<ConfigApplyReport, LimiteronError>` 精确命中；回滚语义：:1671 `new_config.validate().map_err(...)` 校验失败即返回，:1673-1675 注释"预构建（任何失败都在换装前发生，旧配置不受影响）"→ 校验失败旧配置原样保留 ✅
- `src/governor.rs:1674-1677` 注释"热更新路径不重建自定义匹配器注册表（registry 属构造期 builder 注入；含 Custom 匹配器的配置请走进程重启或经注册表预注册）"精确命中 ✅
- `src/governor.rs:578-606` `Governor::new()` 内置兜底规则：`Matcher::User { user_ids: vec!["*"] }` + `LimiterConfig::TokenBucket { capacity: 100, refill_rate: 10 }`，action Reject ✅ 精确命中
- `src/config/types/rule.rs:64-87` `pub enum Matcher` 六变体 User/Ip/Geo/ApiVersion/Device/Custom，无 path matcher ✅ 精确命中
- `src/matchers/engine.rs:311-318` `MatchCondition::User` 评估：`context.get_header("X-User-Id")` 存在时 `contains(user_id) || contains("*")`，无头时仅 `contains("*")` ✅ 命中（分支体 :313-319）
- 租户原语：`src/tenant/config.rs:120` `Namespace::qualify_key` ✅ 精确命中；`src/tenant/resolver.rs:83-88` `HeaderTenantResolver`（建议 :84，命中）

## gap 本地代码引用 — ✅ 全部命中（本会话逐一读取核实）

- config.toml:39-44 `[rate_limit]` enabled=true / requests_per_second=100 / burst_size=200 / window_seconds=60；:47-56 三个 `[[rate_limit.endpoints]]`（/v1/chat/completions 1000 rpm、/v1/embeddings 500 rpm、/admin/* 100 rpm）——建议 :39-55 命中。
- src/config/mod.rs:464-489 `pub struct Config`：顶层字段共 10 个（server/database/vendors/global_model/pricing/auth/cors/retry/routing/cache），**无 rate_limit**；derive 行（:463）无 `deny_unknown_fields` → serde 默认忽略未知字段，"[rate_limit] 段被 serde 静默丢弃"成立（建议 :464-488 命中）。
- src/api/router_builder.rs:62 `let governor = Arc::new(Governor::new().await.expect("Governor init failed"))` 精确命中——请求路径实际用库内置兜底配置。
- router_builder.rs:27-47 `rate_limit_middleware`：:33-34 仅 `.with_path(&path).with_method(...)` 透传给 RequestContext，无端点表自筛——"Governor 规则匹配不到 path"成立。
- src/infrastructure/di.rs:29-51 `fn minimal_flow_config()`（建议写 "di.rs:29-51"，实际路径 src/infrastructure/di.rs，行号命中；Ip["0.0.0.0/0"] + TokenBucket 100/10）；:89 `kit.set_config(minimal_flow_config())` 注入 kit；main.rs:87-88 `build_kit_with_config` + `verify_kit` 后 kit 无后续消费（if 块内即结束）——"minimal_flow_config 从未被请求路径消费"成立。
- "全站 10 rps 单桶"：Governor::new() 兜底规则 TokenBucket refill_rate=10、Matcher::User["*"]（无 X-User-Id 头的请求全部命中 "*"）→ 与 gap 结论一致。

## 无法复核项（如实说明）

- gap 中"本会话临时探针实测（tests/zz_rl_probe.rs，跑完已删除）：加载后 Config 顶层 key 无 rate_limit"为既往声明，探针文件不存在，本次未重放。但其结论由静态事实独立支撑：Config struct 无该字段（mod.rs:464-489 实证）+ serde 无 deny_unknown_fields 时默认忽略未知字段；"评审独立复核"同样无法重放。
- 本次未运行 cargo 测试或编译；核实范围为 sources 真实性 + 代码行号引用，均已在本次会话执行完成。
