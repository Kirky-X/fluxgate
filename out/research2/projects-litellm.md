# LiteLLM 与 OpenRouter 路由算法专项调研（第二轮）

**调研日期**：2026-10-01
**任务**：只挖「算法层面」——怎么算分、怎么采样、怎么判定、窗口多长、阈值怎么动；只报 NexusFlow 缺失的算法（missing）与已有算法的更优变体（improvement）。
**第一轮去重**：已读 `out/research/projects-litellm.md`（功能面：五种策略名、参数名、cooldown 触发条件概述）与 `out/research/papers-routing.md`。第一轮记的是"LiteLLM 有 latency-based 策略、cooldown 5s/50% 失败率、least-busy"这类**是什么**；本轮下沉到**怎么算的**（源码行级）。

## 证据获取方式（诚实说明）

- `raw.githubusercontent.com` 与 `api.github.com`（contents API）在本次会话中先先后被拒：`raw` 返回 `curl: (35) Recv failure: Connection reset by peer`，`api.github.com` 返回 `{"message":"API rate limit exceeded for 111.29.179.185"}`。**实际改用 jsDelivr CDN 镜像** `https://cdn.jsdelivr.net/gh/BerriAI/litellm@main/litellm/...`（全部文件下载成功，共 13 个文件 / 约 19k 行，本地路径 `/tmp/litellm/`）。下文 `文件:行号` 均指该镜像下载的 `main` 分支内容。
- OpenRouter 文档用 WebFetch 抓取成功（`web_reader` MCP 限流 429，未使用）。
- **未做**：任何代码编译或测试运行（本轮是只读调研，无 build/无 test）。

## 一、LiteLLM 源码级算法

### 1.1 simple-shuffle：四级权重回退链 + max 归一化轮盘赌
`litellm/router_strategy/simple_shuffle.py:43-67`

```python
weight_sets = chain(
    (_scoped_weights(...),),                                   # ① 请求级权重表
    tuple(_metric_weight(d, m) for d in healthy_deployments)   # ② weight ③ rpm ④ tpm
    for metric in ("weight", "rpm", "tpm")
)
for weights in weight_sets:
    largest = max(weights, default=0.0)
    if largest <= 0: continue                                  # 全 0 则试下一级
    normalized = tuple(w / largest for w in weights)            # 归一化用 max 不是 sum
    if sum(normalized) <= 0: continue
    selected = random.choices(healthy_deployments, weights=normalized)[0]
    return selected
return random.choice(healthy_deployments)                       # ⑤ 全空兜底均匀随机
```

- ① 请求级权重来自 `_scoped_weights`：读 `request_kwargs["_router_weights"][model][deployment_id]`（`simple_shuffle.py:27-40`），即**单个请求可指定每部署权重**，粒度到 model_group × deployment_id。
- 归一化除以 **max** 而非 sum（`:61`）——比例与除 sum 等价，只是防浮点上溢；`rpm`/`tpm` 直接当权重用，语义是"容量越大分越多"。
- 采样是 `random.choices`（轮盘赌/累积权重），不是无放回轮盘。
- 对比 NexusFlow：`compute_weight`（`src/router/router.rs:560-588`）只有配置侧 `model.weight` 一个手工权重，rpm/tpm 容量信号要人工折算进 `window_factor`。**改进方向**：让权重来源也可按"未配置→rpm 容量自动权重"回退，零配置接入新 vendor。

### 1.2 least-busy：在途请求计数（不是排队长度）
`litellm/router_strategy/least_busy.py`

- 计数器 key：`f"{model_group}_request_count:{deployment_id}"`（`:47-48`），TTL `IN_FLIGHT_COUNT_TTL_SECONDS = 3600`（`:14`）。
- 增减时机（`:122-151`）：`log_pre_api_call` → **+1**（发出请求前）；`log_success_event` / `log_failure_event` → **-1**（成功失败都减）。所以计的是**在途（已发出未结束）**请求数，不是队列长度。
- 0 下限钳位（`:193-197, 211-215`）：`_increment` 后 `if local < 0: set_cache(key, 0)`；Redis 侧用 `increment_with_floor`。防漂移为负导致永久欠债。
- 双层计数（`:183-199`）：本地 in-memory（`local_only=True`）+ Redis 共享；**Redis 读失败降级为本 worker 计数**（`:159-166`，只打 warning 不抛）。
- 选择是严格 argmin（`_least_busy`，`:86-91`）：`healthy_deployments[min(range(n), key=counts[i])]`——平票取列表首个，**不做随机打散**。
- 与 NexusFlow 对比：NexusFlow 的 `queue_factor`（`router.rs:614-616`）基于 `queue_len`（本网关排队数），在途中的上游调用不计入。LLM 出站是长连接慢请求，"排队数=0 但 20 个在途"的场景会误判为空闲。`src/vendor/manager.rs:433` 附近已有并发路径，**在途计数加一个 `AtomicI64` per vendor 即可**，成本极低。

### 1.3 冷却判定式（v2）：失败率 + 最小样本 + 单部署组保护
`litellm/router_utils/cooldown_handlers.py:330-424`（`_should_cooldown_deployment`），默认值在 `litellm/constants.py:87-151`。

判定前置于 `_should_run_cooldown_logic`（`:271-327`）：`disable_cooldowns` / `time_to_cooldown≈0` / `_is_cooldown_required()==False` / 部署在 `provider_default_deployment_ids` → 不跑。

**第一层：状态码分类**（`_is_cooldown_required`，`:218-268`）
- 含 `APIConnectionError` 字样 → 不冷却
- 429 / 401 / 408 / 404 → 冷却
- 其余 4xx（400/403/…）→ **不冷却**（客户端的错，不算部署的锅）
- 5xx 与其他 → 进入第二层失败率判定
- 判定函数本身抛异常 → `except: return True`（fail-safe 到冷却）

**第二层：失败率判定**（BASE CASE，`:376-416`，仅当未设 allowed_fails）
```
num_succ, num_fail = 当分钟成功数, 当分钟失败数        # track_deployment_metrics.py:78-113
total  = succ + fail
pct    = fail/total  (total>0 时)
if   status==429 and not 单部署组:                     → 冷却
elif pct == 1.0  and total >= 1000:                    → 冷却   # 单部署组专用
elif pct > 0.5  and total >= 5     and not 单部署组:     → 冷却
elif not litellm._should_retry(status):                → 冷却   # 不可重试错误
else: 不冷却
```
默认常量（`constants.py`）：`DEFAULT_FAILURE_THRESHOLD_PERCENT=0.5`、`DEFAULT_FAILURE_THRESHOLD_MINIMUM_REQUESTS=5`、`SINGLE_DEPLOYMENT_TRAFFIC_FAILURE_THRESHOLD=1000`（全部 `os.getenv` 可覆盖）。

**分钟窗口实现**（`litellm/router_utils/router_callbacks/track_deployment_metrics.py:45-113`）：key `{deployment_id}:successes` / `{deployment_id}:fails`，`increment_cache(local_only=True, value=1, ttl=60)`——**TTL=60s 自然过期的滚动分钟窗**（非对齐自然分），且**只在本 worker 内存**（跨实例各自判定）。

**对照 NexusFlow**：`CircuitBreakerConfig::new(5, 3, Duration::from_secs(30))`（`src/vendor/manager.rs:115-123`）= 固定 30s 窗口、连续 5 次失败开路。缺的是"比例判定 + 最小样本保护"：低流量下 5 次失败全挂（5/5=100%）当前会开路，比例式会因 `total<5` 或单部署组的 1000 门槛而不误杀；高流量下慢性 30% 失败当前永远开不了路（连续计数达不到 5）。**这是明确的淘汰触发式改进，不是新功能。**

### 1.4 allowed_fails：按异常类型分桶 + TTL=冷却时长的增量计数
`cooldown_handlers.py:76-87`（`_EXCEPTION_POLICY_FIELDS` 九类异常）、`:561-609`（`should_cooldown_based_on_allowed_fails_policy`）、`:612-622`

- 异常分类表（**注意顺序**：ContentPolicyViolation 继承 BadRequest，必须先判）：ContentPolicyViolationError / BadRequestError / AuthenticationError / Timeout / RateLimitError / InternalServerError / ServiceUnavailableError / BadGatewayError / NotFoundError，每类可设独立阈值（`{ErrorType}AllowedFails`）。
- 计数：`base_key = f"deployment:{deployment}:allowed_fails"`，按异常类型时分桶为 `...:{ExceptionTypeName}`（`:604-605`），`cache.increment_cache(key, 1, ttl=cooldown_time)`（`:606-608`）——**计数器的 TTL 就是冷却时长**，即"冷却窗口内累计失败次数"。
- 判定：`updated_fails > allowed_fails` → 冷却。默认 `DEFAULT_ALLOWED_FAILS=3`、`DEFAULT_COOLDOWN_TIME_SECONDS=5`（`constants.py:91,93`）。
- 三层覆盖序：deployment 级 `model_info.allowed_fails` / `allowed_fails_policy` > router 级 > 默认。
- 部署级字段**必须放 `model_info` 不能放 `litellm_params`**（`:107-114` 注释）：后者会整体拷进上游请求体造成泄漏。

### 1.5 冷却存储：显式剩余时间 + 整组最短冷却
`litellm/router_utils/cooldown_cache.py`

- 冷却条目是结构化值 `{exception_received(脱敏50字符前缀), status_code, timestamp, cooldown_time}`（`:26-31, 84-89`），key `deployment:{id}:cooldown`（`:132-133`），写入 TTL=cooldown_time（`:121-125`）。
- **到期不靠缓存 TTL 判定，而是读时显式算**（`_corrected_active_cooldown`，`:135-157`）：`remaining = timestamp + cooldown_time - now`，`remaining<=0` → 删除并视为恢复；同时校正内存 TTL（DualCache 从 Redis 提升条目时用默认 600s TTL，纠正为 `min(remaining, 60s)`，`:36, 152-156`）。这样读侧能准确知道剩余冷却秒数。
- `get_min_cooldown`（`:206-223`）：取组内各部署 `cooldown_time` 的最小值，随 `RouterRateLimitError` 一起抛给调用方（`router.py:14252-14264`）——"全组冷却时告诉客户端/上游等多久再试"。
- 冷却存储用**独立 DualCache**，Redis 批读间隔单独配（`default_redis_batch_cache_expiry`，`:49-52`），让兄弟副本能及时看到彼此写入的冷却，且不被路由缓存的其他 key 挤出内存层。

### 1.6 全冷却 fail-open 安全网
`litellm/router.py:13067-13074`

```python
if not healthy_deployments and self.enable_health_check_routing and self.allowed_fails_policy is not None:
    healthy_deployments = _pre_cooldown_deployments   # 绕过冷却过滤，宁可试已冷却的部署
```
注释明确：只有当冷却是**主动健康检查**驱动（配了 allowed_fails_policy）时才允许；真实请求失败导致的冷却**不允许绕过**。这是"整组熔断后是否降级可用"的显式策略开关。

### 1.7 latency-based：统计口径与选择策略（两段式）
`litellm/router_strategy/lowest_latency.py`

**参数默认**（`:27-31`）：`ttl=3600`（1h）、`lowest_latency_buffer=0`、`max_latency_list_size=10`、`ttft_percentile=None`。

**写侧（`:61-178` 同步 / `:243-368` 异步）**
- 非流式样本 = **每 token 延迟**：`final_value = 响应总秒数 / completion_tokens`（`:126-130`，`safe_divide_seconds` 遇 0/None 退化为总秒数）。
- 流式样本 = **TTFT**：`completion_start_time - start_time`（`:110-112`），单独存 `time_to_first_token_seconds[]`。
- 环形 FIFO：`len(list) < 10` 则 append，否则 `list[1:] + [new]`（`:144-147`）。
- **Timeout 失败写 1000.0 秒惩罚样本**（`:193-228`，仅 Timeout，其它失败不写）。
- 结构：`{model_group}_map: {deployment_id: {latency[], ttft[], "{Y-m-d}-{H}-{M}": {tpm, rpm}}}`，整体 TTL=ttl（`:172-174`）。

**读侧（`_get_available_deployments`，`:370-498`）**
1. 未用过的部署初始化 `latency:[0]`（`:400-404`）——**新部署延迟 0，必被优先探索**。
2. `random.sample` 打乱全部部署（`:412-415`），防全 0 时固定选首个。
3. 限额过滤：`当分钟 tpm + 本次输入 token > 部署 tpm 限` 或 `当分钟 rpm+1 > rpm 限` → 剔除（`:471-476`）。
4. 取值：流式请求且配了 `ttft_percentile` → **nearest-rank 百分位** `sorted[ceil(n*p)-1]`（`:40-43`）；否则均值（流式用 TTFT 列表均值，非流式用 latency 列表均值）（`:447-457`）。
5. 排序 → `lowest = sorted[0]` → `buffer = lowest_latency_buffer × lowest` → 候选 = `latency ≤ lowest + buffer` → **候选内均匀随机**（`:482-494`）。默认 buffer=0 即严格最快；文档推荐 0.5 防打爆最快的那个。

**对照 NexusFlow**：`record_request(vendor, success, duration)`（`src/utils/metrics.rs:130,158-166`）把**请求总时长**直接喂进 EWMA(α=0.2)，流式与非流式、长回复与短回复混在同一序列；`latency_factor = ref/(ref+ewma_ms)`（`router.rs:592-598`）是有界线性因子，没有候选集概念。两点改进：(a) 统计口径分道（流式记 TTFT、非流式记 per-token 归一化）——当前流式的 TTFT 与非流式的总时长互相污染 EWMA；(b) 选择策略从"线性因子加权随机"补一段"min+buffer 候选集内均匀随机"，给最快者留并行余量。

### 1.8 usage-based-v2：自身累计当分钟用量 + 输入 token 预扣
`litellm/router_strategy/lowest_tpm_rpm_v2.py:340-465`

- 计数 key：`{deployment_id}:{model}:tpm:{HH-MM}` / `...:rpm:{HH-MM}`（`:454-465`）——**对齐自然分钟（UTC HH-MM 字符串）**，TTL 覆盖该分钟。
- 过滤式（`:380-385`）：`item_tpm + input_tokens > 部署 tpm 限额` 或 `rpm[item] + 1 >= 部署 rpm 限额` → 剔除。**把本次请求的预估输入 token 先加进用量再比限额**（预扣）。
- 选择：`argmin 当分钟 tpm`，**平票的全部进候选再 `random.choice`**（`:386-390, 449-450`）。
- 读侧批量：`PrefetchedUsage`（`:467-497`）一次批读覆盖多个策略共享的读请求。

**对照 NexusFlow**：`window_factor`（`router.rs:623-636`）用的是**供应商侧窗口余额**（FixedWindowLimiter 的 used/limit），网关自身对某 vendor 的当分钟消耗没有进入权重。LiteLLM 的补充维度是"按本模型名计价的当分钟累计"——同一 vendor 上不同模型组互不干扰的用量账。

### 1.9 access_windows：跨午夜 / 时区 / team 豁免的多窗口
`litellm/router_utils/access_windows.py:35-75`

```python
def is_window_active(window, now):
    local = now.astimezone(ZoneInfo(window.timezone)).time()   # 每窗口自带时区
    if window.start < window.end:
        return window.start <= local < window.end               # 普通窗口
    return local >= window.start or local < window.end          # 跨午夜：22:00-06:00
```
- 每部署可配多个窗口（`access_windows` 列表），窗口内可列 `team_ids`：请求 team 命中则**豁免**（`:58-59`），否则该部署被过滤（`filter_reserved_deployments`，`:63-75`）。
- 语义是**硬过滤**（进候选集前剔除），不是 NexusFlow 那种"窗口因子加权"。

**对照 NexusFlow**：`AdvancedRateLimiter::TimeWindow` 判定是 `now < *start || now >= *end`（`src/vendor/manager.rs:1276-1282`）——**跨午夜配置（start=22:00,end=06:00）在凌晨 2 点会判成"窗口外"**，且窗口固定用 `chrono::Local` 无时区配置能力、无多窗口、无 team 豁免。这是本轮找到的 NexusFlow 里最实的一个逻辑缺口。

### 1.10 tag / budget routing：过滤器而非选择器
- **预算**（`litellm/router_strategy/budget_limiter.py:1-8, 130-353`）：docstring 直言"Note: This is a filter, like tag-routing"——接受健康部署后**剔除超预算者**，与任何负载均衡策略正交。三层检查序（`_filter_out_deployments_above_budget`，`:229-288`）：provider 预算 → deployment 预算 → tag 预算，任一层超即剔除。spend key `provider_spend:{provider}:{duration}` / `deployment_spend:{id}:{duration}` / `tag_spend:{tag}:{duration}`（`:242, 263, 275`）；窗口起点 key + spend key TTL=窗口时长；内存即时更新 + Redis pipeline 每 1s 批同步（`DEFAULT_REDIS_SYNC_INTERVAL`，`:48`）。**全组超预算 → 抛错**（`:197-200`）。
- **tag 语法**（`tag_based_routing.py:218-222`）：`&tag`=required（AND 子集）、裸 tag=positive、`!tag`=excluded。决策序 `_resolve_constraint_only_pool`（`:352-377`）：有 required → 先排除再取 required 子集；无 required → 从带 `default` 标记的池里排除。
- **tag fail-open 的安全设计**（`:256-349`）：结果为空时，仅当部署声明 `allow_fail_open: true` **且**未知 required tag 不是"隐藏答案"的原因才降级——`_unknown_required_tag_hides_an_answer`（`:256-277`）会先剔除组内从未出现过的编造 tag 重算，若露出非空答案则说明是调用方 tag 打错，禁止 fail-open 掩盖。继承约束用**交集而非差集**（`_trusted_only_pool`，`:291-320`），调用方无法靠重复提交来取消 key/team 策略带来的约束。

**为何不单列为 finding**：这两条对 NexusFlow（内部网关、单租户、无虚拟密钥体系）适用性低——没有多租户 key/team 层级，预算过滤需要先有每请求成本计算（第一轮 P1-8 已列），tag 路由的 fail-open 安全语义在无继承策略的场景不成立。算法机制记录于此备查。

### 1.11 候选过滤管道与 weighted failover
`litellm/router.py:12989-13131`（`async_get_healthy_deployments`）——完整决策序：
```
_common_checks(pattern 匹配) → team 过滤 → web_search 过滤
→ 主动健康检查过滤(在冷却前) → 冷却过滤(+fail-open 安全网) → blocked(手动暂停)
→ callback 过滤(budget 等) → pre_call_checks(上下文窗口，按 input_token_count)
→ tag 路由 → routing plugin 收窄(复杂度/auto/adaptive/quality 路由)
→ order 分层(取最低 order 层) → _excluded_deployment_ids(weighted failover 排除集)
→ _retry_skipped_deployment_ids(本请求已拒绝过的部署)
→ 策略选择器(simple-shuffle 时直接 simple_shuffle)
```
- **weighted failover**（`router.py:7183-7258`，默认关闭、仅 simple-shuffle、仅异步入口）：从异常取 `failed_deployment_id`；排除集存在 request metadata `_failover_excluded_ids` **跨跳累积**；剩余 = 全部 − 当前冷却 − 已排除；为空则不跑；然后调 `run_async_fallback` 在组内重挑。
- **排队调度**（`litellm/scheduler.py:17-96`）：每 model_group 一个 heapq 优先队列，priority 0-255（小值优先）；**无健康部署时只有队首请求可"等"，其余 poll 返回 False**（`:73-85`）；有健康部署则全部放行。轮询间隔 `DEFAULT_POLLING_INTERVAL=0.03s`（`constants.py:501`）。

**对照 NexusFlow**：第一轮已实施"排除集 + route_excluding 重调"（`src/api/mod.rs:114-162`，注释明说"weighted_select 对剩余候选天然重归一化"，`max_total_attempts` 默认 8 防爆炸），与 LiteLLM weighted failover 同构，此处不重复报告。

## 二、OpenRouter 文档级算法

### 2.1 默认负载均衡：价格平方倒数加权 + 30 秒故障窗口降权
`https://openrouter.ai/docs/features/provider-routing`（WebFetch 实抓）

1. 先剔除"最近 30 秒有明显故障"的 provider（原文 "not seen significant outages in the last 30 seconds"）；
2. 在稳定 provider 中按 **价格平方的倒数**加权随机（inverse square of the price）；
3. 其余 provider 作 fallback 排在后面。

官方算例：A=\$1/M、B=\$2、C=\$3，B 近期故障 → 请求路由到 A，**A 被选中的概率是 C 的 9 倍**（1/1² : 1/3² = 1 : 1/9 → 9:1）；A 失败→试 C；C 失败→最后才试 B。
注：该页未给出"明显故障"的判定式（无阈值/无统计量定义），只有 30 秒窗口。

### 2.2 sort / order 与 partition
- `sort`: `"price"` | `"throughput"` | `"latency"`（字符串）；对象形式 `{by, partition}`，`partition`: `"model"`（默认，先按模型分组，主模型端点永远先试）| `"none"`（跨模型全局排序）。
- **设了 `sort` 或 `order` 即禁用负载均衡**，改为顺序尝试。
- 快捷后缀：`:nitro` = throughput 排序 + 允许 priority service tier；`:floor` = price 排序 + 允许 flex tier。

### 2.3 性能百分位软降权（滚动 5 分钟窗口）
同页。`preferred_min_throughput` / `preferred_max_latency`（tokens/sec、秒），支持 p50/p75/p90/p99 百分位；**设置多个百分位时必须全部满足才进入 preferred 组**（例：`preferred_min_throughput: {p90: 50}` = 90% 请求 >50 tok/s）。
- **软降权**：不满足只是 "moved to the end of the list"，请求仍会执行；对比 `max_price` 不满足则**请求不执行**（硬过滤）。
- 统计窗口：页面表述为滚动 5 分钟窗口的百分位统计。

### 2.4 fallback 边界：pre-stream 静默换路 / mid-stream 停止
`https://openrouter.ai/docs/api-reference/errors`（WebFetch 实抓）
- **任何 token 送达客户端之前**失败：启用 fallback 路由时 "Silently retry with a different provider endpoint"；若所有 provider 都失败，HTTP 状态仍是 200，最后一个错误放在 body 里返回。
- **一旦部分答案已到达客户端**（HTTP 200 已发出）："Failover also stops once part of the answer has reached you"。
- 重试分类（配合 Retry-After 秒）：429、503，以及 `error.metadata.limit_source == "openrouter_in_flight_budget"` 的 402；无 Retry-After 的 402 不重试。
- 请求级错误直接返回客户端：400/401/402/403/408/429/502/503；运行中错误走 200 + body（流式为 `finish_reason: "error"` 的 SSE chunk）。
- 页面**未**给出重试次数上限、超时数值、hedging 机制。

**对照 NexusFlow**：非流式走 `execute_request_with_fallback` 换路链（`src/api/mod.rs:114-162`）；但**流式路径没有任何换路**——`execute_passthrough_stream_request` 失败直接 `return Err`（`src/api/openai.rs:62-85`，注释"流后续错误由 metrics 记账"）。也就是说首字节前的 5xx/429 在流式请求上直接暴露给客户端，而 OpenRouter 在同一情形会静默换 provider。补齐时应明确边界：首字节前可换路（HTTP 200 尚未提交），首字节后禁止换路（已发内容不能重发）。

## 三、筛选结论（只保留 missing / improvement）

| # | 算法 | 来源 | gapType | 落地代价 |
|---|------|------|---------|---------|
| 1 | 冷却失败率判定式（pct>0.5 & 样本≥5 & 单部署组 1000 门槛 & 429 立即） | LiteLLM cooldown_handlers.py:376-416 | improvement | low |
| 2 | allowed_fails 按异常类型分桶 + TTL=冷却时长的增量计数 | LiteLLM cooldown_handlers.py:76-87, 561-609 | improvement | medium |
| 3 | 冷却条目显式剩余时间 + 整组 min_cooldown 等待提示 | LiteLLM cooldown_cache.py:135-157, 206-223 | improvement | low |
| 4 | least-busy 在途计数（pre-call +1 / settle -1 / 0 钳位 / 降级本地） | LiteLLM least_busy.py:122-217 | improvement | low |
| 5 | 延迟统计口径分道（per-token 归一化 / TTFT / 环形 10 样本 / timeout 惩罚样本） | LiteLLM lowest_latency.py:110-159, 193-228 | improvement | medium |
| 6 | min+buffer 候选集内均匀随机 | LiteLLM lowest_latency.py:482-494 | improvement | low |
| 7 | simple-shuffle 四级权重回退（请求级权重→weight→rpm→tpm→均匀） | LiteLLM simple_shuffle.py:43-67 | improvement | low |
| 8 | usage-based v2：按模型计价的当分钟累计 + 输入 token 预扣 + argmin 平票随机 | LiteLLM lowest_tpm_rpm_v2.py:340-465 | improvement | medium |
| 9 | access_windows 跨午夜/时区/多窗口/team 豁免 | LiteLLM access_windows.py:35-75 | improvement | low |
| 10 | 全冷却 fail-open 安全网（仅健康检查驱动的冷却可绕过） | LiteLLM router.py:13067-13074 | improvement | low |
| 11 | 价格平方倒数加权 + 30s 故障窗口降权 | OpenRouter provider-routing | **missing**（NexusFlow 权重无成本维度，更无非线性定价） | medium |
| 12 | 性能百分位软降权（5 分钟滚动 p50-p99，多阈值全满足才 preferred，不满足移末尾） | OpenRouter provider-routing | improvement | medium |
| 13 | pre-stream 静默换路 / mid-stream 停止 + 429/503 重试分类 | OpenRouter errors | improvement（NexusFlow 流式无换路） | medium |

**未报告**：预算/tag 过滤（1.10，无多租户 key/team 层级，适用性低）；weighted failover 与候选过滤管道（1.11，第一轮已实施同构）；OpenRouter quantization/data_collection/zdr 等过滤字段（面向其平台特性，网关自建场景不适用）。

## 四、与第一轮的差异声明

第一轮 `out/research/projects-litellm.md` 已报（且已实施）：优先级分层重试、失败排除集重归一化、重试状态码配置化、部署级冷却、会话亲和、cost-based routing、多策略可插拔、虚拟密钥、响应缓存。本轮全部**未重复**，只补算法层：新判定式（新）、统计口径与窗口（新）、候选集采样策略（新）、成本进入权重的具体函数形式（第一轮只说"成本作为第四维"）。