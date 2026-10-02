# 第二轮算法调研：Portkey Gateway / Higress AI 网关 / Kong AI Gateway

> 调研日期：2026-10-01
> 轨道：projects-gateway（算法层面，非功能清单）
> 对照对象：NexusFlow（Rust 2021 + Axum 0.8 单二进制网关）
>
> **证据纪律**：本轮所有算法结论都基于本会话**真实读取过的源码文件或文档页**，逐条给 URL。
> 源码获取通道：`codeload.github.com` tarball（Portkey）、`cdn.jsdelivr.net/gh/<owner>/<repo>@<branch>/<path>`（Higress/Kong，`api.github.com` 匿名配额 60/h 在本会话耗尽，`raw.githubusercontent.com` 对 Higress/Kong 仓库反复 SSL exit 35，故改走 jsdelivr；失败的文件如实标注）。
> 未能取得实现源码者（Kong AI Gateway 2.0 的 ai-proxy-advanced / ai-rate-limiting-advanced / balancer 为 Enterprise 闭源）只做**文档级**描述并明确标注。

---

## 0. 取证过程与失败项（如实记录）

| # | 目标 | 通道 | 结果 |
|---|---|---|---|
| 1 | Portkey `src/handlers/retryHandler.ts`（main） | raw.githubusercontent（重试 3 次） | ✅ 全文 220 行 |
| 2 | Portkey `src/globals.ts` | api.github.com contents（base64） | ✅ 常量 |
| 3 | Portkey `src/handlers/handlerUtils.ts` | raw | ✅ 1353 行（LB 选择逻辑 196-230、693-722） |
| 4 | Portkey 全仓库（main + `2.0.0` 分支） | codeload tarball | ✅ 两个 tarball 解压 |
| 5 | Portkey `src/utils/circuitBreaker.ts`、`services/cache/durable-objects/CircuitBreakerDO.ts`（2.0.0 分支） | tarball | ✅ 378 / 185 行 |
| 6 | Higress `ai-load-balancer/{global_least_request,prefix_cache,cluster_hash}/lb_policy.go`、`main.go`、`endpoint_metrics/lb_policy.go`、`endpoint_metrics/scheduling/scheduler.go` | jsdelivr | ✅ 全文 |
| 7 | Higress `ai-proxy/provider/{failover,retry}.go` | jsdelivr | ✅ 全文（failover.go 只读 1-135 与 340-560 行，其余按 grep 概览） |
| 8 | Higress `ai-token-ratelimit` 源码 | 猜文件名全 404，jsdelivr data API 403，github.com HTML 不可达 | ❌ 未取得（第一轮已读其 README，功能层面已知；本轮不据此报算法条目） |
| 9 | Kong `kong/runloop/balancer/{init,balancers,latency,least_connections,round_robin,consistent_hashing}.lua` | jsdelivr | ✅ 全文（latency 253 行、least_connections 199 行、consistent_hashing 228 行） |
| 10 | Kong `kong/plugins/ai-proxy*`、`rate-limiting-advanced` 源码 | jsdelivr | ❌ 404。master 开源仅 6 个 ai-* 插件（ai-prompt-decorator/guard/template、ai-proxy、ai-request/response-transformer），高级策略属 Enterprise 闭源 |
| 11 | Kong AI Gateway 文档：load-balancing、ai-rate-limiting-advanced | curl developer.konghq.com（SSR HTML，剥标签取正文） | ✅ 全文 |
| 12 | `api.github.com` contents/search API | — | ❌ 匿名配额耗尽（remaining: 0），仅前 3 次调用成功 |

---

## 一、Portkey Gateway（Portkey-AI/gateway）

### 1.1 开源版 LB 与策略执行（`src/handlers/handlerUtils.ts:660-790`，本会话已读）

- **LOADBALANCE = 线性扫描加权随机**：先把 `weight` 缺省补 1（695-698 行），求 `totalWeight`（699-702），抽 `randomWeight = Math.random() * totalWeight`（704），从数组头开始依次 `if randomWeight < provider.weight` 命中并减去，否则 `randomWeight -= provider.weight` 继续（705-722）。**无任何健康/延迟信号**，纯静态权重。
- **FALLBACK = targets 数组顺序线性尝试**，触发条件由 `strategy.onStatusCodes` 过滤（676-689：显式给了 codes 就看是否命中；没给就"响应 ok 即停"），或上游带 `x-portkey-gateway-exception: true` 头强制继续换路（684-685）。
- 结论：**Portkey 开源版 LB 算法弱于 NexusFlow 现有 3D 加权随机**，本轮不报。

### 1.2 重试：总时间预算 + 多头 Retry-After + 退避清零（`src/handlers/retryHandler.ts:65-220` + `src/globals.ts`，本会话已读）

- 退避本体来自 npm `async-retry`，**只传了 `retries: retryCount`、`randomize: false`**（retryHandler.ts:174-180）→ 用库默认参数：指数退避、factor=2、minTimeout=1000ms、**无 jitter**（NexusFlow 的 jitter 0.2 更好，不采纳这一点）。
- **总时间预算**（本会话未见同类机制）：`let remainingRetryTimeout = MAX_RETRY_LIMIT_MS`（84 行），`MAX_RETRY_LIMIT_MS = 60 * 1000`（globals.ts:5）。每次按上游 Retry-After 等待前判断：`retryAfter >= MAX_RETRY_LIMIT_MS || retryAfter > remainingRetryTimeout` → `retrySkipped = true; rateLimiter._timeouts = []; throw`（130-137 行），即"再等下去会超出 60s 预算就放弃重试"；否则 `remainingRetryTimeout -= retryAfter`（138 行）。
- **Retry-After 多头解析**（108-151）：按序探测 `POSSIBLE_RETRY_STATUS_HEADERS = ['retry-after-ms','x-ms-retry-after-ms','retry-after']`（globals.ts:7-11）；`retry-after` 按秒×1000，其余按毫秒（122-126 行）。
- **尊重 Retry-After 后清零退避**（139-148 行，机制上最值得抄）：把 `rateLimiter._timeouts` 重写为长度 `retryCount - attempt + 1` 的全 0 数组，再 `setTimeout(retryAfter)` 后抛出错误进入下一轮 —— 效果是"上游让等 30s 就等 30s，等完之后立刻重试，不再叠加指数退避"。
- **超时归一为 408**（4-50 行）：`AbortController` 超时转成 status=408 的合成 Response，使超时与 4xx 走同一套 retry/fallback 判定。

### 1.3 熔断：时间窗计数式（2.0.0 分支 `src/utils/circuitBreaker.ts`，本会话已读全文）

- 配置（8-14 行）：`failure_threshold`（次数）、`failure_threshold_percentage`（百分比）、`cooldown_interval`（ms，**强制下限 30s**，59-62 行）、`minimum_requests`（**默认 5**，135 行）、`failure_status_codes`（未配置时默认 `status >= 500`，305-313 行）。
- 判定（109-198 行）：`failure_pct = 100 * failure_count / (failure_count + success_count)`，但**样本数 `failure+success < minimum_requests` 时百分比按 0 计**（137-143 行）—— 冷启动不误熔；打开条件是 `failure_count >= threshold` **或** `failure_pct >= threshold_percentage`（157-164 行，二选一即触发）；`now - first_failure_time > cooldown_interval` → 直接 `delete` 该 path 记录（计数器整体清零，**无 half-open 探测态**）。
- **层级聚合**（203-257 行）：递归把子 target 的 `is_open` 向上汇总，"本级所有 targets 都 open → 本级 strategy 也 open"（243-245 行）；父级 `cb_config` 向子级继承（67-69 行）；CONDITIONAL 层显式丢弃熔断配置（84-87 行）。
- 状态存储：缓存键 `{circuit_breaker:${configId}}`，`configId = org:workspace:configSlug`（337-347 行），TTL 24h（179 行）；异常时**全闭 fail-open**（186-191 行）。
- `CircuitBreakerDO.ts`（Cloudflare Durable Objects 版，185 行）：把上面这套状态放进 DO 的单线程存储，`handleRecordFailure` 在首次失败时记 `first_failure_time` 并**清零 success_count**（95-99 行），`setCircuitBreakerData` 用 24h TTL + alarm 懒清理（70-82、172-184 行）—— 多边缘实例下计数原子的做法。

---

## 二、Higress AI 网关（higress-group/higress）

### 2.1 `global_least_request`：精确最小在途 + 平票蓄水池抽样 + 单点并发硬闸

源码 `plugins/wasm-go/extensions/ai-load-balancer/global_least_request/lb_policy.go`（299 行，本会话已读全文），核心是**一段原子 Lua**（21-96 行）：

1. 候选集 = Envoy 侧 `health_status == "Healthy"` 的 host（169-174 行）。
2. Lua 遍历候选：`if same_count_hits == 0 or count < current_count` 则替换并把 `same_count_hits = 1`；`elseif count == current_count` 则 `same_count_hits = same_count_hits + 1; if math.random(same_count_hits) == 1 then current_target = host end`（43-52 行）—— **严格最小值 + 最小值集合上的蓄水池抽样（reservoir sampling）**，即"最小的那 k 个 host 里等概率取一个"，比 power-of-two 的 p2c 更能压平队列。
3. `HINCRBY <host> 1` 记账（55 行），响应流结束 `HandleHttpStreamDone` 时 `HINCRBY -1`（293 行）—— 在途计数**随 SSE 长流存活**。
4. **单 host 并发硬闸**：选中后若 `current_count > maxRequestCount` → 直接 `SendHttpResponse(429, "Exceeded maximum request limit from ai-load-balancer.")`（241-249 行），**不是换下一个 host**。
5. 计数无 TTL；靠 `cleanInterval`（默认 60 分钟，128-132 行）周期任务把"已不在健康 host 列表里的字段"从 hash 里 `HDEL`（74-94 行），避免路由变更后残留。
6. 失败即退回 Envoy 默认策略：`ActionContinue`（264-268 行）；Lua 里 `math.randomseed(tonumber(KEYS[1]))` 用 `time.Now().UnixMicro()` 做种（184 行 + 29 行）。

### 2.2 `prefix_cache`：内容前缀最长匹配路由（XOR 增量键）

源码 `.../ai-load-balancer/prefix_cache/lb_policy.go`（305 行，本会话已读全文）：

1. **分段哈希**：遍历请求体 `messages`，拼 `role + ":" + content`，在**每条 user 消息之后或最后一条消息**处 `computeSHA1(rawStr)` 切一段，得到 `params = [sha1(段1), sha1(段2), ...]`（229-244、300-304 行）。
2. **增量键（radix trick）**：Lua 里 `hex_xor` 逐字节 XOR 相邻段哈希，累积键 `key_k = sha1(段1) ⊕ sha1(段2) ⊕ … ⊕ sha1(段k)`（96-114 行），因此"从对话开头到第 k 轮"只是一个 Redis key，**无需建树**即可 `EXISTS` 顺序探测。
3. **最长前缀匹配**：逐级 EXISTS，第一个命中的即当前最深前缀，`GET` 出 target，命中每一级 `EXPIRE key ttl` 续期（101-113 行）；命中 target 已不健康 → `break`（104-106 行）。
4. **未命中退化**：走同一 Redis hash 的 global least-request（含 50% 平票随机，118-136 行），并把剩余段路径 `SET key target` + `EXPIRE`（142-151 行）建立新的亲和。
5. `redisKeyTTL` 默认 1800s（181-185 行）；与 global_least_request 共用同一个在途计数 hash（22 行 RedisKeyFormat 相同）。

### 2.3 `cluster_hash`：权重和为 100 的槽位轮盘

源码 `.../ai-load-balancer/cluster_hash/lb_policy.go`（123 行，本会话已读全文）：权重必须和为 100（64-66 行），展开成 100 槽 `slots` 数组（68-73 行），`fnv32a(hash_header 默认 x-mse-consumer) % 100` 选 cluster（78-86 行），缺 hash 头直接 403（89-94 行）。简单但有效的"按权重确定性分流（金丝雀）"。

### 2.4 `endpoint_metrics`：滑窗份额限幅 + 推理感知 filter chain

源码 `.../ai-load-balancer/endpoint_metrics/lb_policy.go`（133 行）+ `endpoint_metrics/scheduling/scheduler.go`（224 行，vendor 自 kubernetes-sigs/gateway-api-inference-extension，头部为 The Kubernetes Authors 版权），本会话均已读全文：

- **调度主链**（scheduler.go 50-115 行）：`critical request` 谓词 → `low queueing filter` → `affinity LoRA`/`can accept LoRA` → `queueAndKVCacheFilter`（least queuing → least KV cache percent）；critical 失败则走 `sheddableRequestFilter`——**存在某 pod 队列为空且 KV cache < 0.8 则视为有余量，否则直接返回 error 丢弃该可丢弃请求**（98-114 行，注释明写 "drop the request to make room for critical requests"）。硬编码阈值：`kvCacheThreshold = 0.8`、`queueThresholdCritical = 5`、`queueingThresholdLoRA = 50`（41-47 行）。
- **最终选择是随机而非取最优**（131-138 行）：filter 链产出候选集后 `rand.Intn(len(pods))`。
- **用户自定义指标的近优带宽**（167-221 行）：`MetricPolicyLeast` 保留 `metric ∈ [min, min + (max-min)/N]` 的全部 pod（N=候选数），`Most` 对称；即不追唯一最优，而是在最优值附近留带宽，避免最优 pod 被瞬时打满。
- **滑窗份额限幅**（lb_policy.go 97-113 行）：调度器选出 pod 后，用 `SlidingWindow[string]`（默认 size=100 条、window=60s，41-55 行）统计该 endpoint 近窗命中次数占比 `currentRate = count/size`；`currentRate > rate_limit`（默认 1.0=不限制）→ **从其他 host 里随机换一个**；无论最终选谁，都 `Push(finalAddress)` 记账。

### 2.5 `ai-proxy` failover：key 级熔断 + 主动探活 + 多副本租约

源码 `plugins/wasm-go/extensions/ai-proxy/provider/failover.go`（27KB，本会话读了 1-135 与 340-560 行）及 `retry.go`（183 行，全文）：

- **配置与默认值**（84-112 行）：`failureThreshold=3`、`successThreshold=1`、`healthCheckInterval=5000ms`、`healthCheckTimeout=5000ms`、`healthCheckModel`、`cooldownDuration`、`failoverOnStatus` 正则列表（默认 `["4.*","5.*"]`）。
- **下线/上线判据**（349-403 行）：某 apiToken 失败计数 ≥ failureThreshold → 移入 unavailable 并记 `unavailableSince`；健康检查成功计数 ≥ successThreshold → 移回可用列表，两个计数都归零。**失败计数按 token 分开记**（key 级隔离）。
- **恢复双通道**（156-224 行 tick 函数）：`now - unavailableSince >= cooldownDuration` → 直接放回（167-180 行，不需要探活成功）；否则用 `healthCheckModel` 向该 token 发**真实探活请求**（264-288 行构造），成功走 `handleAvailableApiToken`。
- **多副本去重**（292-341 行）：`tryAcquireOrRenewLease(vmID)` 用 Wasm shared data 的 CAS 选主，只有持租约的 VM 跑健康检查（156-158 行 `c.isFailoverEnabled() && c.tryAcquireOrRenewLease(vmID)`），避免 N 个 worker 同时探活打爆上游。
- **无锁并发更新**（413-446、537-559 行）：所有 shared data 修改走 `for attempt = 1..10` 的 CAS 重试循环（`casMaxRetries = 10`，70 行）。
- **重试时轮换 token**（retry.go 120-183 行）：`removeApiTokenFromRetryList` 剔除刚失败的 token → `GetRandomToken` 从剩余中随机取；**没有退避 sleep**，maxRetries 默认 1，retryTimeout 默认 30s，`retryOnStatus` 默认 `4.*|5.*` 正则。

---

## 三、Kong AI Gateway

### 3.1 开源 balancer 的四种算法（`kong/runloop/balancer/*.lua`，本会话已读全文）

注册表在 `balancers.lua:111-121`：`consistent-hashing` / `least-connections` / `round-robin` / `latency`（latency 是 Enterprise 向 OSS 暴露的实现文件）。

**（1）`latency.lua` —— Peak EWMA（源自 Twitter Finagle PeakEwma）**

- **时间常数衰减**（34-41 行）：`td = now - last_touched_at; weight = exp(-td / DECAY_TIME)`，`DECAY_TIME = 10` 秒；`ewma = ewma * weight + rtt * (1 - weight)`。等价于 **α = 1 - exp(-Δt/10)**：10s 前 α≈0.095（平滑抗噪），久未采样的节点 α→1（新样本立即主导）。
- **RTT 口径**（120-133 行）：`rtt = upstream_connect_time + upstream_response_time`，请求结束后才 `get_or_update_ewma(..., true)` 落盘；选择时用 `get_or_update_ewma(..., false)` 只算不改。
- **评分与采样**（136-147、183-212 行）：`score = ewma / address.weight`，从候选里取 score 最低者；`PICK_SET_SIZE = 2`（27 行）。
- **Slow start**（47-68、71-103 行）：新 address 的 EWMA **初始化为当前所有可用 address 的 EWMA 均值**（不是 0），避免新节点因"历史延迟 0"被打满；`afterHostUpdate` 顺带清掉不可用 address 的条目。
- **重试隔离**（150-163 行）：`handle.failedAddresses` 弱表记录本次请求已失败的 address，选择时过滤。

**（2）`least_connections.lua` —— (inflight+1)/weight 的最小堆**

- 评分：`(addr.connectionCount + 1) / addr.weight`（27、43 行），最小堆 `binaryheap.minUnique()` 取堆顶（23-28、95 行）。注释解释了 `+1` 的用意（39-42 行）：**没有 +1 时任何 0 连接节点都会被优先选中（哪怕权重极低），加 1 后先按权重比例分配再谈最少连接**。
- 计数：选中时 `updateConnectionCount(+1)`，请求结束 `releaseHandleAddress(-1)`（126-131、46-51 行）—— 与 NexusFlow"并发票据随流存活"同思路，差的是评分函数。
- 重试：失败 address 进弱表过滤；全部失败后清空 `failedAddresses` 重新来一轮（98-104 行）。

**（3）`consistent_hashing.lua` —— Ketama 风格加权虚拟节点环**

- 环大小默认/最小 1000 点，最大 2^32（24-26、214-217 行，`upstream.slots` 可调）。
- 每 address 虚拟节点数 `entries = floor((weight/totalWeight) * targets_count * 160)`，至少 1（86-91 行，`SERVER_POINTS = 160`）；虚拟节点名 `"ip:port i"`，`index = xxhash32(name) % points`；**碰撞则 entries+1 继续往后放**（95-113 行，"move the problem forward"）。
- host/address 先按字典序排序再建环（44-66 行），保证环的构建与加入顺序无关、重启可复现。
- 查找：从 hash 位置**逆时针（index 递减、0 回绕到 points）**找第一个 available address（160-190 行），跳过不可用与已禁用地址。

**（4）`round_robin.lua`**：加权平滑轮转，Kong 默认 balancer 算法（未细读）。

### 3.2 AI Gateway 2.0 文档级算法（Enterprise 闭源，仅文档描述）

来源：`https://developer.konghq.com/ai-gateway/load-balancing/`（curl SSR HTML 取正文，本会话已读）。

- 8 种算法：`Round-robin (weighted)` / `Consistent-hashing`（`hash_on_header`，默认 `X-Kong-LLM-Request-ID`）/ `Least-connections` / **`Lowest-usage`**（`tokens_count_strategy` ∈ prompt / completion / total(默认) / **cost** / **LLM-accuracy score**，选"实测资源消耗最低"的模型）/ **`Lowest-latency`**（`latency_strategy` ∈ **`tpot`（默认，time-per-output-token）** / `e2e`；原文："uses peak EWMA … to track latency from TCP connect through body response. Metrics decay over time"）/ `Semantic`（prompt 与模型描述的向量相似度，`distance_metric` + `threshold`，需向量库；同描述多 target 时 round-robin 兜底、weight 影响兜底顺序）/ `Priority`（**优先组**全拿流量，该组全不可用才降下一组，组内按 weight）。
- 重试/回退：`retries`（次数）、`failover_criteria`（error / timeout / http_429 / http_500 等可枚举）、connect/read/write timeout 三段分离。

来源：`https://developer.konghq.com/ai-gateway/policies/ai-rate-limiting-advanced/`（本会话已读）：

- `window_type`：**`sliding`（默认，"Weighs the current window against the previous one to produce a dynamically calculated rate"）/ `fixed` / `calendar`**。calendar 按 IANA 时区对齐真实日历边界（`period: month|week`、`month_day`、`week_start_day`），429 响应头 `X-AI-RateLimit-Reset/Retry-After` 指向下一个日历边界而非 now+window_size。
- 多 limit 多窗口叠加（如 `{limit:100, window_size:60}` 与 `{limit:1000, window_size:3600}`）；`partition_by: true` 让同一策略按模型分桶；`tokens_count_strategy` 同上。

---

## 四、对照 NexusFlow 基线的差距清单（只列 missing / improvement）

| # | 算法 | 来源 | gapType | 相对基线的差异 |
|---|---|---|---|---|
| 1 | Peak EWMA 时间常数衰减（α=1-e^(-Δt/τ)，τ=10s） | Kong `latency.lua:34-41` | improvement | 基线是固定 α=0.2 |
| 2 | Slow start：EWMA 以"现有节点均值"初始化新节点 | Kong `latency.lua:47-68` | missing | 基线无新节点 EWMA 冷启动策略 |
| 3 | (inflight+1)/weight 最小堆 least-conn | Kong `least_connections.lua:27-43` | missing | 基线 queue 只是权重乘子，非评分选择 |
| 4 | Ketama 加权虚拟节点一致性哈希环 | Kong `consistent_hashing.lua:74-121` | missing | 基线亲和是 TTL 表 + 前缀哈希兜底，无权重感知环 |
| 5 | 重试总时间预算（60s）+ Retry-After 后退避清零 | Portkey `retryHandler.ts:84-148` | improvement | 基线有指数退避+jitter+Retry-After 覆盖，但无总预算与清零 |
| 6 | 熔断失败率百分比阈值 + 最小样本数 + 层级聚合 | Portkey 2.0 `circuitBreaker.ts:135-164, 243-245` | improvement | 基线 limiteron::circuit 分类计数，未提失败率/样本门槛 |
| 7 | global least request（最小值+蓄水池平票）+ 单点并发硬闸 429 | Higress `global_least_request/lb_policy.go:31-96, 241-249` | missing | 基线无严格 least-request 采样 |
| 8 | 内容前缀 LPM 路由（SHA1 分段 + XOR 增量键） | Higress `prefix_cache/lb_policy.go:93-151` | improvement | 基线亲和是会话 ID 级 |
| 9 | 滑窗份额限幅（超占比改选） | Higress `endpoint_metrics/lb_policy.go:97-113` | improvement | 基线份额计数只用于告警，不干预路由 |
| 10 | 推理感知优先级 filter chain + 过载丢弃 | Higress `scheduling/scheduler.go:50-114` | missing | 基线无请求优先级与主动丢弃 |
| 11 | key 级 failover + 主动探活 half-open + 多副本租约 | Higress `failover.go:139-224, 292-341, 349-403` | missing | 基线熔断粒度是 vendor 级 |
| 12 | Lowest-usage（token/成本/准确率）与 TPOT 延迟口径 | Kong AI LB 文档 | missing | 基线 EWMA 只看请求总延迟，路由不看 token/成本 |
| 13 | 加权滑动窗口 + 日历对齐窗口 | Kong ai-rate-limiting-advanced 文档 | improvement | 基线 Token 限流是固定窗口 |
