# 第二轮算法专项调研：Bifrost / vLLM production-stack router / Cloudflare AI Gateway

> 调研日期：2026-10-01。**本轮只做算法层**（怎么算分、怎么采样、怎么判定、窗口多长、阈值怎么动），不做功能清单。
> 与第一轮 `out/research/projects-perf.md` 的分工：第一轮读 Bifrost/Kong/OpenRouter 的**文档层功能**（重试分类、虚拟 Key、预算、语义缓存开关、复杂度路由），本轮读三个项目的**源码与设计文档**，把机制写到"能照着实现"的程度。第一轮已覆盖且本轮核实无新增算法含量的（Bifrost 静态加权随机、Cloudflare 精确缓存键形态等）不进入 finding 清单，仅在笔记中记录。
> 全部结论以本会话真实打开的源码/文档为据，URL 与 `file:line` 见每节。

## 0. 方法与网络事实（如实记录）

- `api.github.com` 匿名限额在本会话中途耗尽（`API rate limit exceeded for 111.29.179.185.`），`raw.githubusercontent.com` 直连全程 SSL 失败（`curl` exit 35 / code 000，WebFetch `ECONNRESET`）。
- 最终通道：**jsDelivr CDN 代理 GitHub 仓库文件** `https://cdn.jsdelivr.net/gh/<owner>/<repo>@<ref>/<path>` 全部成功（见下文各文件下载记录）。源码缓存在 `.research2_cache/`（本轮临时文件，可删）。
- 文档侧：`docs.getbifrost.ai/*.md` 与 `developers.cloudflare.com/**/index.md` 均 curl 成功；WebFetch 对这两个域报 "Reasoning is mandatory"（服务端故障），故全部改用 curl。
- **未跑**：论文检索脚本（本轮任务重点是三个项目的算法实现，论文轨非必需；第一轮论文证据已在 `out/research/papers-*.md`）。**未做**：任何 NexusFlow 侧的编译/测试（本轮无代码改动）。

---

## 1. Bifrost（maximhq/bifrost）

### 1.1 会话亲和：两层绑定 + 身份散列 + SetNX 收敛（源码级）

来源：`core/sessionaffinity.go`（463 行，本会话完整读取，缓存于 `.research2_cache/sessionaffinity.go`）
证据 URL：https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/core/sessionaffinity.go

**（1）两层状态，键各自独立**（`sessionaffinity.go:22-30`）
- `SessionStateKindRoute = "route"`：会话的路由绑定 = `(provider, model)` 二元组。
- `SessionStateKindKey = "key"`：该 provider+model 内部的具体 API key 绑定。
- `SessionStateKey`（`:389-397`）= `"session:v2:" + kind + ":" + hex(SHA256(vkID ‖ 0x00 ‖ userID ‖ 0x00 ‖ sessionID ‖ 0x00 ‖ parts...))`。源码注释写得很直白（`:16-20`）："session id is chosen by the caller, so two callers can send the same one; every piece of session state is therefore keyed under who the request's grant identity attributes it to, the virtual key and user, or under the deployment as a whole when neither is known"——**每个组件都把 session 状态键在"身份 ∥ session"之下**，0 字节分隔防止拼接歧义，且键长有界、不携带调用方原文。

**（2）写入三态：settle / replace / forget**（`:234-265`）
```
settle(key, value, followed, ttl):
  followed == ""  → kv.SetNXWithTTL(key, value, ttl)   // 首绑定：仅在无绑定时落盘
  followed != ""  → replace（SetWithTTL 覆盖+续期）
forget(key)      → kv.Delete
```
注释（`:230-233`）：跟随无绑定的请求用 **set-if-not-exists**，使"一个会话的并行首请求收敛到最先完成的那个"；跟随了绑定且成功的请求刷新它并迁移到实际服务者。

**（3）读取：链重排而非替换，且要求 provider+model 双匹配**（`ResolveRoute`，`:69-136`）
- 显式指定了 provider 的请求**不重排**（`:82-84`：点名 provider 就是要它，不是要"上次服务的"）。
- 在 fallback 链中找 `route.Provider == bound.Provider && route.Model == bound.Model`（`:103-105`）。注释给了具体反例：跟随 provider 单独匹配会把绑定在 `vertex/opus-4-8` 的会话挪到 `vertex/opus-4-7`（比 primary 更旧的模型）并留在那里。
- 命中 → 把该 route 提到链首（`:130-134`：新链 = `[chain[at]] + chain[:at] + chain[at+1:]`）；链里没有 → **删除绑定**并按路由原决定走（`:106-119`，理由：链是本请求可用范围，链外绑定是 stale，删掉让下一个被服务的请求重新绑定，会话自然收敛）。
- key 级（`ResolveKey`，`:142-171`）：候选 key < 2 直接不做（省一次 KV 读）；**fallback provider 不使用 key 绑定**（`:146-148`，fallback 之所以被用是 primary 不可用，它用什么 key 由结果决定）；绑定 key 已不在 eligible → 删除并正常选。

**（4）观察：取消 vs 超时分开处理**（`Observe`，`:183-228`；`cancelledByCaller`，`:249-251`）
- `outcome.Served == nil && cancelledByCaller(err)`（error type == `RequestCancelled`）→ **保留全部绑定**：调用方自己放弃，与 provider 无关。
- 超时（`RequestTimedOut`）按普通失败处理：删除跟随的 route 绑定与 key 绑定（`:196-205`）。
- fallback 成功时 route 绑定迁移到 fallback（fallback 也是"服务者"），key 绑定在 fallback provider 上强制 `replace`（`:220-223`：fallback 拿的 key 比旧绑定新）。
- 绑定 TTL 来自请求级 `BifrostContextKeySessionTTL`，缺省回落 `schemas.DefaultSessionStickyTTL`（`:431-440`）。**该常量的数值本会话未定位到定义文件（jsDelivr 逐文件 grep 未命中），第一轮文档页记录默认 1h——数值以文档为准、源码未验证，不做二次断言。**

**对照 NexusFlow**（本会话实读）：
- `src/router/affinity.rs:48-50` 键为 `affinity:{session_key}`——**无租户/身份维度**；`:64-73` `bind` 是无条件覆盖写（**无 SetNX 首绑定收敛**）；`src/router/router.rs:346-376` `affinity_boost` 命中后**直接 return 该 vendor**（硬选，完全绕过 queue/EWMA 权重），命中但不在合格集合才 unbind。取消/超时未区分。
→ 产出 F4（键隔离）、F5（SetNX + 三态写入 + 取消/超时区分）、F8（model 配对）。

### 1.2 加权随机 key 选择：无算法含量（记录，不报）

来源：`core/keyselectors/weightedrandom.go`（36 行，完整读取）
证据 URL：https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/core/keyselectors/weightedrandom.go

```go
totalWeight += int(key.Weight * 100)      // 浮点权重×100 取整
if totalWeight == 0 { return keys[rand.Intn(len(keys))] }   // 全零→均匀
randomValue := rand.Intn(totalWeight)
for _, key := range keys { currentWeight += int(key.Weight*100); if randomValue < currentWeight { return key, nil } }
return keys[0], nil                       // 兜底
```
线性累加扫描、权重静态、无动态调权。**NexusFlow 的多维加权随机严格优于它，不报。**

### 1.3 语义缓存：两级匹配 + 惰性过期 + 异步物理删

来源：`plugins/semanticcache/search.go`（457 行）、`plugins/semanticcache/utils.go`（1012 行，仅读关键段）
证据 URL：https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/plugins/semanticcache/search.go 、 https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/plugins/semanticcache/utils.go

**（1）直查路径 = 确定性 UUIDv5，O(1) 点查**（`search.go:23-50, 216-239`）
- `requestHash = xxhash.Sum64(MarshalDeeplySorted({input: 归一化输入, params}))`（`:200-210`）。
- `directCacheID = uuid.NewSHA1(ns, MarshalDeeplySorted({cache_key, request_hash, params_hash, provider?, model?}))`（`:216-239`）。所有过滤器（cacheKey、provider、model、requestHash、paramsHash）都编码进 ID，因此 `store.GetChunk(ns, directCacheID)` 一次点查即可，不需要向量检索。

**（2）语义路径：top-1 + 严格过滤**（`:54-108`）
- `store.GetNearest(embedding, strictFilters, selectFields, threshold, 1)`——**k=1**。
- `strictFilters`（`:87-97`）：`cache_key` 相等、`params_hash` 相等、插件命名空间标记；`cache_by_provider`/`cache_by_model` 开启时再追加 provider/model 相等。即"语义相似但采样参数不同"不共享缓存。
- 阈值可**每请求覆盖**（`:77-84`，读 context `CacheThresholdKey`，非 float64 时回落配置值）。

**（3）归一化与确定性序列化**（`utils.go`）
- `normalizeText`（`:122-124`）= `strings.ToLower(strings.TrimSpace(text))`——小写化 + trim 提高命中率（注意：对代码类 prompt 会损失区分度，这是取舍）。
- `MarshalDeeplySorted` 保证 Go map 键序确定性（`utils.go:80-90` 注释：plain `json.Marshal` 不保证 map 顺序）。
- 集合型参数（modalities、stop sequences 等）排序后哈希，且"未设置"与"空数组"哈希一致（`putSortedSetIfNonEmpty`，`:114-120`）。
- 对话历史门控：`isConversationHistoryThresholdExceeded`（`:995-1012`）——消息数 > `conversation_history_threshold` 直接不走缓存。

**（4）淘汰：惰性过期 + 异步物理删除**（`search.go:248-271, 302-325`）
- 命中后检查 `expires_at`：过期 → 起一个 `context.WithTimeout(5s)` 的后台 goroutine `store.Delete`，**本次当 miss 继续回源**；`expires_at` 解析失败也当 miss（保守）。
- 异步删除挂 `writersWg`，`WaitForPendingOperations`/Cleanup 会等它，避免与 namespace 拆除竞态。

**（5）嵌入 fail-open**（`:60-65`）：嵌入调用失败返回 error，上层放行到上游 LLM（注释："silent skip — provider misconfig or transient embedding errors fall through to the upstream LLM call"）。

**（6）流式命中重放**（`:349-408`）：不预解码全部 chunk（否则首块前 O(N) 延迟），逐块反序列化逐块发送，每次 send 挂 `ctx.Done()` 防消费者丢弃导致 goroutine 泄漏；畸形块 log+skip 而非截断。

**对照 NexusFlow**：精确缓存已实现（`src/api/response_cache.rs:7-13`：键 `resp:{api_key_sha256}:{vendor_id}:{model_id}:{sha256(canonical_body)}`，完整 body 入键 ⇒ 采样参数天然入键，**Bifrost 的 `params_hash` 与之同构且更弱，不报**）。语义缓存为暂缓项 → 产出 F11（淘汰）、F12（阈值与嵌入成本记账）。

### 1.4 Prompt cache 断点注入：双门 + 四标记预算（写侧，区别于读侧匹配）

来源：`core/providers/utils/promptcache.go`（15KB，读取 `:19-160` 及结构索引）
证据 URL：https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/core/providers/utils/promptcache.go

- **两道门都要过**（`:28-33`）：①运维显式 opt-in（`AutoInject` 或 `InjectionPoints` 非空）——注释："Bifrost never invents a breakpoint by default — the four-marker budget is scarce and spending one is a cost decision that belongs to them"（Anthropic 最多 4 个 cache breakpoint 是稀缺预算）；②模型能力门 `ResolveModelCaps().SupportsPromptCaching(...)`——隐式缓存端点返回 false，永不发送会被拒/被忽略的 marker。这使该函数可以对所有 provider 无条件调用。
- **默认策略 = 第一个可缓存块**（`responsesInjectionTargets` 注释，`:255`）：agent 循环的稳定前缀；另一策略按配置的 injection points 定位（`matchMessageIndices` 按 role 匹配消息下标）。
- **客户端已自带任一 marker → 完全不碰**（`:79-81, 119-121`：客户端发言优先，注入只服务沉默客户端）。
- **copy-on-write**：被标记的 message 深拷贝后再挂 marker（`:86-97`），注释点名 in-place 变更曾把 provider 特定缓存设置泄漏进 fallback 链复用的请求（PR #6181）。
- 每请求 header 可双向覆盖 `auto_inject`（on/off），但**不能凭空 opt-in**（`:54-66`：未配置 prompt_cache 的 provider = 运维没表达意见，header 不得替其花掉 checkpoint 或改变计费档）。

**对照 NexusFlow**：无此机制（跨协议网关不回写上游缓存标记）。归类为"prefix cache 相关"，但严格说属**写侧**而非任务重点的"匹配算法"，本轮只在笔记记录，不进 finding（避免凑数）。

### 1.5 企业版熔断器：响应头信号 + 冷却时长取自响应头

来源：`docs/enterprise/circuit-breaker.md`（9179 字节，本会话 curl 全文读取，缓存 `.research2_cache/bf_cb.md`）
证据 URL：https://docs.getbifrost.ai/enterprise/circuit-breaker.md （经 https://docs.getbifrost.ai/llms.txt 索引定位；`features/plugins/circuit-breaker.md` 现只返回 200 字节壳页）

- **触发源是 HTTP 响应头**（Key Features 表原文）："Trips on HTTP response headers returned by the provider — **no latency heuristics or error-rate windows to tune**"。
- 每策略针对一个 `primary_provider + primary_model`（per-model failover）。
- **信号匹配三模式**（Signals 表）：仅 `header_name`=存在即跳闸；`header_name+header_value`=值相等（大小写不敏感）；`header_name+header_contains`=子串包含。`operator` ∈ {OR, AND} 组合多信号。
- **per-key 子电路**：默认一个共享电路；设 `primary_key_ids: [uuid...]` 后每个 key 独立状态，**主电路仅当列出的全部 key 都跳闸才开**（原文："Until that point, healthy keys continue to receive traffic while degraded keys are excluded"）。
- **冷却时长动态**：`default_cooldown` 默认 `"30s"`（Go duration）；`cooldown_header`（如 `retry-after-ms`，单位毫秒）存在且可解析时用其值，缺失/不可解析回落 `default_cooldown`。
- 钩子：pre-request 检查是否 open（open 则直接改投 fallback，不联系原目标）；post-response 评估响应头。冷却到期后**下一个请求即探针**，信号不再出现则保持关闭。
- 典型场景：Azure PTU 容量耗尽时响应头 `X-Ms-Is-Spilled-Over: true` → 打开电路 30s → 期间流量透明改投 PAYG 部署。

**对照 NexusFlow**：`src/vendor/resilience` 的 `ErrorClassifier` 按状态码/网络错误分类计数（`src/vendor/resilience/error_map.rs:115`），**只看得见失败，看不见"200 但已降级"**。→ 产出 F6。

### 1.6 内部 LRU：generation 计数防止 in-flight 值"复活"（记录，不报为 finding）

来源：`framework/lrucache/lrucache.go`（15KB，仅读结构注释与关键函数索引）
证据 URL：https://cdn.jsdelivr.net/gh/maximhq/bifrost@main/framework/lrucache/lrucache.go
- 包注释（`:1-31`）：容量 LRU + **generation 计数器**——每次显式 evict/flush 递增；`setIfGeneration` 只在 generation 未变时写入，防止"被驱逐的值在 in-flight fill 完成后又装回"；**容量驱逐不递增 generation**（丢弃的值仍然有效）；`indexKey` 绑定允许按外部标识驱逐。
- 这是 Bifrost 内部框架缓存（single-flight fill + Loader 返回 indexKey），不是 LLM 语义缓存。NexusFlow 缓存由 oxcache 托管、拿不到 generation 钩子，**不报**。

---

## 2. vLLM production-stack router

全部源码经 jsDelivr 下载（`610 total files` 的仓库树经 api.github.com 列目录，证据 URL 见各节）。核心文件：`src/vllm_router/routers/routing_logic.py`（1230 行，全文读取）、`src/vllm_router/prefix/hashtrie.py`（105 行，全文）、`src/vllm_router/stats/request_stats.py`（327 行，全文）、`src/vllm_router/experimental/semantic_cache/*`。

### 2.1 LoadAware 评分式（本轮最重要的调度算法）

来源：`routing_logic.py:481-756`（`LoadAwareRouter`）；设计文档 `docs/source/use_cases/loadaware-routing.rst`（6852 字节，全文）
证据 URL：https://cdn.jsdelivr.net/gh/vllm-project/production-stack@main/src/vllm_router/routers/routing_logic.py 、 https://cdn.jsdelivr.net/gh/vllm-project/production-stack@main/docs/source/use_cases/loadaware-routing.rst

**评分式（`routing_logic.py:481-491` 原文注释 + `.rst:32-40`）**
```
score(i) = matched_tokens(i) / prompt_tokens  −  β · relative_load(i)
relative_load(i) = (load(i) − mean_load) / max(1, mean_load)
load(i) = in_prefill_requests(i) + in_decoding_requests(i)     # :526-536
路由到 argmax score
```
**每条设计理由都能照抄实现**（`.rst:42-48`）：
1. **缓存收益归一化为比例**（0→1 的"已缓存 fraction"），不是原始 token 数——"so one `beta` means the same policy for a 500-token and a 4000-token prompt"（两个分数量纲一致）。
2. **负载相对炉队均值归一化**：绝对在途数无界（取决于请求率、prompt 长度、GPU），"the same `beta` would be a different policy on every deployment"；`relative_load = 0.0` 是平均、`+1.0` 是两倍均值；**分母钳在 1**——"keeps a near-idle fleet from amplifying one in-flight request into a large relative load and thrashing on noise"（`routing_logic.py:545-547`）。
3. **每个 endpoint 都打分**（不只缓存持有者）：没缓存的 benefit=0——这才让"cold but idle beats warm but loaded"可表达。
4. **不套 `kv_aware_threshold`**：kvaware 需要阈值带是因为它无法在小命中与其他方案间权衡，argmax 不需要（"a small match simply loses to load"）。

**实现细节**
- `score_endpoint`（`:558-569`）：`benefit = min(matched_tokens, prompt_tokens) / max(prompt_tokens, 1)`——`min()` 钳制防止匹配被上取整到 token 库 chunk 边界后"假满命中"反超真满命中。
- `select_url`（`:589-619`）：纯函数、无 I/O；`sorted(endpoints, key=url)` 后严格大于才替换，**平局按 URL 字典序**保证可复现。
- β 解析（`:65-90`）：显式参数 > `LOADAWARE_BETA` 环境变量 > 默认 `1.0`；非数值记 warning 回落默认。默认语义（`.rst:125`）："an endpoint sitting 100% above fleet-average load is docked one full cache hit's worth of preference"。调参方向：β=0.25 偏缓存（长共享前缀、prefill 贵、容忍排队）；β=2.0 偏负载；β→∞ 逼近 least-loaded。
- **freshness 选择**（`:526-536` 注释）：负载用 `request_stats`（事件驱动）而非 `engine_stats`（scrape 滞后）；`route_request` 显式标注 `engine_stats` "Unused: it is scrape-lagged"。
- **phantom holder 防护**（`matched_tokens_by_url` `:571-587` + `instance_map_is_stale` `:621-636`）：引擎重启会在控制器 `kv_pool` 留下死 instance_id，其"命中"是幽灵（重启清空了缓存）；记分只认 live id；桥表陈旧时"Missing the latter silently degenerates placement to least-loaded for the life of the router"。
- 无任何缓存持有者 ⇒ 回落 `fallback_url`（`:692-706`）：有 session_id 走哈希环，否则最低 QPS。

**对照 NexusFlow** → 产出 F1（亲和软化为评分项）、F2（相对负载归一化）。

### 2.2 Session 路由：一致性哈希环（无状态亲和）

来源：`routing_logic.py:230-281`
- `uhashring.HashRing`，`_update_hash_ring`（`:118-137`）每次请求前把 endpoint 集合 diff 进环（移除下线节点、加入新节点）。
- 有 session_id ⇒ `hash_ring.get_node(session_id)`；**无 session_id ⇒ `_qps_routing`**（`:94-116`：选 QPS 最低者；某引擎没请求记录则直接选它）。
- 纯函数映射：**无存储、无 TTL、无续期/解绑逻辑**，路由器重启亲和即恢复；节点增减只重映射约 1/N 的键。

**对照 NexusFlow**（TTL 表）→ F9。

### 2.3 前缀亲和：128 字符块 HashTrie

来源：`prefix/hashtrie.py`（全文）
证据 URL：https://cdn.jsdelivr.net/gh/vllm-project/production-stack@main/src/vllm_router/prefix/hashtrie.py

- **粒度**：`chunk_size = 128` **字符**（不是 token）——`_chunk_and_hash`（`:46-57`）对每块 `xxhash.xxh64(chunk).intdigest()`，trie 的边是块哈希整数。
- **节点**：`TrieNode{children: dict, endpoints: set, lock: asyncio.Lock}`（`:25-33`）——每节点一把锁，保证单协程访问。
- **insert**（`:59-75`）：根节点也记 endpoint，然后逐块下探建子节点，**每个经过的节点都把 endpoint 加入其集合**（⇒ 节点集合 = 服务过以该前缀开头的全部实例）。
- **longest_prefix_match**（`:77-104`）：逐块下探；每层取 `endpoints ∩ selected_endpoints`（**候选集随深度收缩**），交集为空即停（保证选中的实例确实服务过这一整段前缀）；`match_length` 按 128 累加；返回 `(match_length, selected_endpoints)`。
- **调用方 PrefixAwareRouter**（`routing_logic.py:759-850`）：多命中时 `random.choice`（`:846`）；**冷启动陷阱已被踩过并修**（`:835-844` 注释：`prefix_min_match_length > 0` 时若不把未达阈值的请求也 insert，trie 永远为空、亲和永不激活——修复是"回落 QPS 路由但仍 insert"）；**类注释明说"we assume that there is no eviction of prefix cache"**（`:764-765`）——trie 只增不减，重启清零。

**对照 NexusFlow**（"前缀哈希兜底"= 整体哈希当 session key）→ F3。

### 2.4 请求级负载统计：事件驱动分阶段计数 + 惰性滑窗

来源：`stats/request_stats.py`（全文）
- 状态机（`:144-249`）：`on_new_request` → `in_prefill += 1` 并记 qps 点；`on_request_response`（首 token）→ `in_prefill -= 1`、`in_decoding += 1`、记 TTFT；`on_request_complete` → 若见过首 token 则 `in_decoding -= 1`，否则（prefill 阶段失败）`in_prefill -= 1`，并记端到端延迟；`on_request_swapped` 计换出数。
- `MovingAverageMonitor`（`:57-102`）：`deque` 存 (timestamp, value)，`update` 追加并弹出早于 `timestamp − sliding_window_size` 的点；`update_no_value` 供**读取时惰性推进窗口**（避免无请求时监控停摆）。
- 口径注释（`:110-113`）："QPS is calculated based on the number of **arrived** requests in the sliding window, but the inter_token_latency and ttft are calculated based on the number of **completed** requests"。
- `load_penalty` 只用 `in_prefill + in_decoding`（routing_logic.py:526-536），**TTFT/ITL 不进评分**。

**对照 NexusFlow**（单一 EWMA 延迟口径、queue 是本地队列深度）→ F10。

### 2.5 语义缓存（实验级，代码质量一般，如实标注）

来源：`experimental/semantic_cache/semantic_cache.py`（354 行）、`db_adapters/faiss_adapter.py`（136 行）
- 嵌入模型 `all-MiniLM-L6-v2`（384 维），messages 拼成 `"role: content"` 空格连接单串（`semantic_cache.py:60-75`）。
- **默认相似度阈值 0.95**（`:28, 323`）。
- FAISS `IndexFlatIP`（内积；`faiss_adapter.py:42`）——注意 sentence-transformers `encode` 默认**不归一化**，故内积≠余弦；本实现按内积阈值 0.95 使用（源码如此，如实记录）。
- `search`（`:72-100`）：`index.search(embedding, 1)` top-1，score ≥ threshold **且 metadata.model 完全相等**才命中。**无 TTL、无淘汰、无租户隔离**（metadata 只有 request/response/model/usage）。
- `store` 每次 `add` 后**全量 `save_index()` 写盘**（`:132`）——实验代码的性能缺陷。
- **两段式 API**：`initiate_search`/`complete_search`、`initiate_store`/`complete_store`（`:164-313`）——嵌入计算与搜索/存储解耦，嵌入可在需要结果之前算好。

**对照 NexusFlow**（语义缓存暂缓）→ F12。

### 2.6 失败重路由（记录，无算法含量）

来源：`services/request_service/request.py:630-655`
```python
max_attempts = router.max_instance_failover_reroute_attempts + 1   # 默认 0 → 不重试
for attempt in range(max_attempts):
    if attempt > 0:
        remaining = [ep for ep in endpoints if ep.url not in error_urls]   # 排除失败实例
        server_url = router.route_request(remaining, ...)                  # 重跑一遍路由
```
无退避、无抖动（与 Bifrost 的指数退避+jitter 形成对照）。不报。

---

## 3. Cloudflare AI Gateway

全部经 `https://developers.cloudflare.com/ai-gateway/llms.txt` 索引发现页面路径，curl `<page>/index.md` 获取。**Cloudflare 为闭源托管服务，只能读文档，无源码。**

### 3.1 缓存键与 TTL

来源：https://developers.cloudflare.com/ai-gateway/features/caching/index.md （Last updated Sep 30, 2026，10617 字节）
- **缓存键 = SHA-256( 拼接 ‖ )**，拼接项五元组：`Provider` ‖ `Endpoint`(API 路径) ‖ `Model` ‖ **Provider 认证头（Authorization bearer token）** ‖ `完整请求体`。
- 完全精确匹配："Any difference in the body — including messages, tools, or model parameters — will result in a separate cache entry"。
- TTL：per-gateway `cache_ttl`；per-request 头 `cf-aig-cache-ttl`（**最小 60 秒、最大 1 个月**）、`cf-aig-skip-cache`、`cf-aig-cache-key`（自定义键，附带把单请求 opt-in 进缓存）。
- 命中指示响应头 `cf-aig-cache-status: HIT|MISS`。默认关闭。
- **明文声明尚无语义检索**："We plan on adding semantic search for caching in the future"（截至文档更新日 2026-09-30）——更正第一轮报告中把 Cloudflare 与"语义缓存同类实现"并列表述。

**对照 NexusFlow**：`src/api/response_cache.rs:7-13` 已是 `resp:{api_key_sha256}:{vendor_id}:{model_id}:{sha256(canonical_body)}`，与该形态同构且更强（vendor+model 显式入键），**不报**。TTL 上下界（60s / 1 月）是配置约束，可顺手对齐，不单列 finding。

### 3.2 限流：fixed vs sliding 二选一

来源：https://developers.cloudflare.com/ai-gateway/features/rate-limiting/index.md
- `rate_limiting_limit` + `rate_limiting_interval` + `rate_limiting_technique ∈ {fixed, sliding}`。
- 文档给的例子（10 次/10 分钟，起点 12:00）：fixed 窗口 12:00-12:10、12:10-12:20 各独立 ⇒ 12:09 发 10 次 + 12:11 发 10 次**共 20 次全放行**；sliding 则"more than ten requests in the last ten minutes"⇒ 后 10 次被拒。
- 超出返回 429。全局生效，不区分租户/模型。

**对照 NexusFlow**：出站 Token/Interval 为固定窗口（CAS + 惰性重置），入站为 GCRA（本身平滑）→ F13（仅出站 Token 窗口）。

### 3.3 超时与重试：TTFT 语义超时 + 末次重试无限等待

来源：https://developers.cloudflare.com/ai-gateway/configuration/request-handling/index.md （Last updated Sep 14, 2026）
- **超时以"响应的第一部分返回"为准**："The timeout is based on when the first part of the response comes back. As long as the first part of the response returns within the specified timeframe — such as when streaming a response — your gateway will wait for the response."
- per-request 头 `cf-aig-request-timeout`（毫秒）。
- **重试**：最多 **5** 次；可配 `cf-aig-max-attempts`、`cf-aig-retry-delay`（毫秒，**最大 60 秒**）、`cf-aig-backoff ∈ {constant, linear, exponential}`。
- **末次重试无限等待**："On the final retry attempt, your gateway will wait until the request completes, regardless of how long it takes."

**对照 NexusFlow**（本会话实读源码）：
- `src/vendor/manager.rs:150` `reqwest::Client::new()`——**出站 HTTP 零超时**（无 connect/read/total timeout）。
- `src/vendor/manager.rs:773-778` 非流式 `timeout(Duration::from_secs(60), response_rx)`——硬编码 60s，**且从入队时刻开始计时（含本地排队时间）**，不可按 vendor/请求配置。
- 无"首字节超时"概念：流式与非流式共用同一超时模型。
→ F7。

### 3.4 回退与动态路由（无算法含量，记录）

- 回退：https://developers.cloudflare.com/ai-gateway/configuration/fallbacks/index.md —— Universal Endpoint 请求体传 fallback 数组，按序尝试，失败即换下一个；响应头 `cf-aig-step` 指示哪一步成功处理。与 NexusFlow FallbackChain 同构，不报。
- 动态路由：https://developers.cloudflare.com/ai-gateway/features/dynamic-routing/index.md —— 有 `Conditional`（表达式引用 body/headers/metadata）、`Percentage`（"Routes requests **probabilistically** across multiple outputs"——**文档未给采样公式或权重语义**）、`Rate Limit`/`Budget Limit`（超限切 fallback）节点，路由版本化可回滚。Percentage 节点算法未公开，**不报**。

---

## 4. NexusFlow 侧现状核对（本会话实读，用于判定 gap）

| 位置 | 现状 | 对应 finding |
|---|---|---|
| `src/router/router.rs:346-376` | `affinity_boost` 命中即 `return` 该 vendor（硬选，绕过 queue/EWMA） | F1 |
| `src/router/router.rs` 权重公式 | `queue_factor` 绝对队列深度（`manager.rs:739-743` 读 `instance.queue.len()`） | F2 |
| `src/router/affinity.rs:48-50` | 键 `affinity:{session_key}`，无租户维度 | F4 |
| `src/router/affinity.rs:64-73` | `bind` 无条件覆盖写；无取消/超时区分 | F5、F8 |
| `src/router/affinity.rs` | 单值 vendor_id，无 model 配对 | F8 |
| `src/router/affinity.rs:54-79` | TTL 表：lookup 续期、bind 覆盖、unbind 删除 | F9 |
| `src/vendor/resilience/error_map.rs` | ErrorClassifier 按状态码/网络错误分类 | F6 |
| `src/vendor/manager.rs:150 / :773-778` | Client::new() 无超时；非流式硬编码 60s（含排队） | F7 |
| `src/utils/metrics.rs` + `vendor/manager.rs` | 延迟 EWMA 单口径 | F10 |
| `src/api/response_cache.rs` | 精确缓存（oxcache TTL 托管），无惰性过期删除钩子可见 | F11 |
| 语义缓存 | 暂缓（等日志数据） | F12 |
| `src/vendor/rate_limiter.rs` Token 窗口 | 固定窗口 + CAS + 惰性重置 | F13 |

---

## 5. Finding 清单（13 条）

| ID | 名称 | gapType | 来源 | effort |
|----|------|---------|------|--------|
| F1 | 亲和软化：亲和收益 − β·相对负载的可调交换率 | improvement | production-stack LoadAwareRouter | medium |
| F2 | 负载维度均值中心化相对化（无量纲 + max(1,mean) 钳制） | improvement | production-stack loadaware | low |
| F3 | 块级哈希前缀 trie 的渐进式前缀亲和 | missing | production-stack HashTrie | medium |
| F4 | 亲和键的租户/身份散列 | improvement | Bifrost SessionStateKey | low |
| F5 | 首绑定 SetNX 收敛 + follow/settle/forget 三态 + 取消/超时区分 | improvement | Bifrost Observe/settle | low |
| F6 | 响应头信号熔断 + 冷却时长取自响应头 + per-key 子电路 | missing | Bifrost Enterprise CB | medium |
| F7 | TTFT 语义超时 + 末次重试无限等待 + per-vendor 可配超时 | missing | Cloudflare request-handling | medium |
| F8 | 亲和绑定携带 (provider, model) 配对 | missing | Bifrost ResolveRoute | low |
| F9 | 一致性哈希环无状态亲和（失败用扰动重试替代解绑） | improvement | production-stack SessionRouter | medium |
| F10 | TTFT / decode 双口径延迟 + 分阶段在途计数 | improvement | production-stack request_stats | medium |
| F11 | 惰性过期 + 异步物理删除的缓存清理 | improvement | Bifrost buildResponseFromResult | low |
| F12 | 语义缓存阈值保守默认（0.95 vs 0.8）与嵌入成本记账 | improvement | production-stack / Bifrost | low |
| F13 | 出站 Token 固定窗口 → 滑动窗口 | improvement | Cloudflare rate-limiting | low |

## 6. 本轮明确"没找到算法含量"的项目（诚实记录，避免下轮重复）

- Bifrost `keyselectors/weightedrandom.go`：静态权重线性扫描，NexusFlow 更强。
- Cloudflare 精确缓存键 / 回退链 / 动态路由 Percentage 节点：与 NexusFlow 现有能力同构或算法未公开。
- production-stack 失败重路由：无可配退避（默认 0 次），无借鉴价值。
- Bifrost `framework/lrucache`：内部框架缓存，NexusFlow 缓存由 oxcache 托管拿不到 generation 钩子。
- Bifrost prompt cache 断点注入：写侧机制，非本轮"匹配算法"重点，仅笔记记录。
