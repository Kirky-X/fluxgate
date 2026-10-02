# 路由调度模块研读笔记（src/router/）

> 调研日期：2026-09-30。范围：`src/router/mod.rs`（7 行）、`src/router/router.rs`（450 行），并追查其全部数据源（`src/config/manager.rs`、`src/vendor/manager.rs`、`src/vendor/rate_limiter.rs`、`src/vendor/queue.rs`、`src/vendor/status.rs`、`src/config/mod.rs`）与调用方（`src/api/openai.rs`、`src/api/anthropic.rs`）。
>
> **验证状态**：`cargo test --lib router` 已尝试运行但**编译失败**——兄弟库为路径依赖（`Cargo.toml:39-57` 指向 `../dbnexus` 等），本环境 `/home/dev/projects/` 下只有 `NexusFlow/alphalloy/garrison/janvi`，`dbnexus` 等库不存在。报错：`failed to read /home/dev/projects/dbnexus/Cargo.toml (os error 2)`。因此本文所有结论均来自源码静态通读 + grep 交叉验证，**没有运行时验证**。

---

## 1. 模块职责

`Router`（`src/router/router.rs:25-28`）是一个无状态（仅持有两个 `Arc`）的路由决策器：把客户端请求里的 model 字符串解析成一个具体目标 `(VendorConfig, ModelConfig)`，即 `RoutingDecision`（router.rs:11-15，附带 `RoutingStrategy` 标记：`Direct / Abstract / Global`，router.rs:18-22）。

对外两个入口：
- `route(abstract_model)` — router.rs:38-62，常规解析链；
- `route_with_hints(model, preferred_vendor, preferred_model)` — router.rs:65-92，响应 `X-Vendor` / `X-Model` 请求头提示（`api/openai.rs:41-48` 由 `RoutingHints::from_headers` 提取后调用）。

三种解析策略（优先级从高到低）：

| 顺序 | 策略 | 触发条件 | 实现 | 候选来源 | 健康过滤 |
|------|------|----------|------|----------|----------|
| 1 | Abstract | 任意输入 | `try_abstract_route` router.rs:171-190 | `config.resolve_model()` 全表匹配 abstract_id **或 model_id** | enabled + `get_status()==Available` |
| 2 | Direct | 输入含 `.`（`vendor.model`） | `try_direct_route` router.rs:141-169 | 按 vendor id + model id 精确匹配 | **仅 enabled，无健康检查** |
| 3 | Global | 输入为 `"auto"` | `try_global_route` router.rs:192-220 | 全部 enabled vendor × enabled model | enabled + `get_status()==Available` |
| — | Vendor(hints) | `X-Vendor` 头 | `try_vendor_route` router.rs:94-139 | 指定 vendor 内按 model 提示匹配，否则取第一个 enabled model | enabled + `get_status()==Available` |

策略间是"尝试链"而非"fallback 链"：前一级返回 `None` 才试下一级；`route()` 末尾统一报 `NexusFlowError::Validation`（router.rs:59-61）。**project 文档（AGENTS.md）声称的 "weighted / fallback / priority" 三种策略中，fallback 和 priority 在 router/ 内不存在**（见 §5.7）。

## 2. 数据流（一次请求的完整路径）

```
客户端请求 (model="gpt-4o")
  → api 层: openai.rs:33-48 / anthropic.rs:33
      state.router.route(model).await        [或 route_with_hints]
  → Router::route (router.rs:38)
      ① lowercase (router.rs:39)
      ② try_abstract_route (router.rs:43)
           config.resolve_model()            [config/manager.rs:60-89，O(V×M) 扫描，深克隆每个命中]
           .filter(enabled && get_status==Available)   [router.rs:176-183]
      ③ (失败才试) try_direct_route (router.rs:48)    [splitn('.', 2) router.rs:142]
      ④ (仅 "auto") try_global_route (router.rs:53-57) [遍历全部 vendor×model，全克隆]
      ⑤ weighted_select (router.rs:222-277)
           对每个候选 calculate_weight (router.rs:279-283):
               vendor_manager.get_queue_length(id)   [vendor/manager.rs:585-590]
               vendor_manager.get_window_info(id)    [vendor/manager.rs:870-931]
               compute_weight = w×0.4 + queue×0.3 + window×0.3
           加权随机选中
  → RoutingDecision{vendor, model, strategy} 返回 api 层
  → api 层按 vendor.type_ 选路:
       同协议 → execute_passthrough_request / _stream_  (openai.rs:63/91, anthropic.rs:43/71)
       异协议 → execute_request (队列 worker 路径)
```

## 3. 关键算法与数据结构

### 3.1 3D 权重公式（router.rs:286-300）

```rust
weight = model.weight × 0.4 + queue_factor × 0.3 + window_factor × 0.3
```
非有限或 ≤0 时归 0（router.rs:295-299）。`model.weight` 来自 `ModelConfig.weight`（config/mod.rs:72-73，默认 1.0，注释约定 0.0–2.0），所以权重理论上限 1.4（weight=2, queue=1, window=1）。

### 3.2 三个因子的取数来源与时效性

**queue_factor**（router.rs:303-305）：`1 - min(len/100, 1)`。
- 取数：`VendorManager::get_queue_length`（vendor/manager.rs:585-590）→ `QueueManager.get(id)`（vendor/queue.rs:66-68）。
- **时效性问题见 §5.1：这个数据源是空的，因子恒为 1.0。**

**window_factor**（router.rs:312-325）：
- `None`（无窗口信息/无实例）→ 0.5 中性；预算耗尽（`!has_budget()`）→ 0.0；窗口 300s 内结束 → `budget% × 0.5` 惩罚；否则 → `budget%`。
- 取数：`VendorManager::get_window_info`（vendor/manager.rs:870-931），按 `AdvancedRateLimiter` 变体现场换算：
  - `Token{max,used}` → `(Some(max), used, None)`，budget = 1 - used/max（vendor/manager.rs:874-875, 916-921；used 为 `AtomicU64 Relaxed` 读）；
  - `TimeWindow{start,end}` → 窗口内给 `window_end`（用 chrono Local 时间现场算剩余，vendor/manager.rs:876-895），**max=None → budget 恒 1.0**，窗外 `(None,None)` → factor 反而 1.0（见 §5.10）；
  - `Interval` → `window_end = now + (interval - elapsed)`（vendor/manager.rs:896-912），max=None → budget 恒 1.0；
  - `None` 变体 → `(None, 0, None)` → factor 0.5。
- 时效性：读取时现场计算（chrono/Instant），**读是新鲜的；但 used_tokens 的写入稀疏**（见 §5.4）。

**健康过滤 `get_status`**（vendor/manager.rs:563-583）：
1. config 里 vendor 不存在或 `!enabled` → `Unavailable`；
2. instances 无条目 → **`Available`**（manager.rs:579，见 §5.9）；
3. `advanced_limiter.check()` 为 false → `Throttled`（manager.rs:571-575）。`check()`（rate_limiter.rs:128-142）：TimeWindow 看当前时刻是否在窗内；**Interval 恒 true**（rate_limiter.rs:134）；Token 看 `used < max`；None 恒 true。check 是纯读，无副作用，不消耗预算。
4. 否则返回 `VendorState.status`（vendor/status.rs:35-37，AtomicU8 Relaxed）——**生产代码无任何写入方**（见 §5.2）。

### 3.3 加权随机选择（router.rs:222-277）

- 每候选算权重 → `Vec<(f64, (VendorConfig, ModelConfig))>`（router.rs:229-235）；
- 总权重 ≤0 → `rng.gen_range(0..len)` 均匀随机兜底（router.rs:239-246）；
- 否则 `target = rng.gen::<f64>() * total`，**线性累加扫描** `cumulative >= target` 选中（router.rs:248-260），O(n) 时间、无预处理结构；
- 兜底返回最后一个（router.rs:263-276）。
- 随机源：`rand::thread_rng()`（router.rs:227，rand 0.8，Cargo.toml:69），线程本地，无锁。
- 权重为 0 的候选（如预算耗尽但没被 status 过滤掉的）不会被命中（cumulative 不前进），但**仍占位参与克隆与求和**，不会被剔除。

### 3.4 候选解析：resolve_model（config/manager.rs:60-89）

- 全量双循环扫 `vendors × models`，比较 `abstract_id.unwrap_or(model_id).to_lowercase() == 输入`（config/manager.rs:76-82）——**model_id 本身也作为匹配键**；
- 每个命中深克隆 `(VendorConfig, ModelConfig)`；VendorConfig 含 `models: Vec<ModelConfig>`（config/mod.rs:44-57），是 O(该 vendor 全部模型) 的深拷贝；
- 无索引。`ConfigManager` 明明有现成的 `get_all_model_mappings()` 索引构建器（config/manager.rs:109-137），但**路由不用它**，只有别处（推测 admin 展示用）。

### 3.5 配套数据结构

- `ConfigManager`：`Arc<RwLock<Config>>`（std 锁，config/manager.rs:11-13），每次访问拿读锁 + 克隆所需部分；`update_config` 先写文件再换内存（config/manager.rs:30-50），**不重建 vendor 实例**（§5.9）。
- `VendorManager.instances`：`RwLock<HashMap<String, Arc<VendorInstance>>>`（vendor/manager.rs:117），只在启动时 `initialize()`（main.rs:42 → manager.rs:195-217）填充。
- `VendorQueue`：async-channel bounded(100)（vendor/queue.rs:21-24，容量硬编码 manager.rs:226），`len()` 现成可用（queue.rs:39-41）——但路由没读它。
- `WindowInfo`（vendor/manager.rs:31-39）+ 三个纯函数 `is_ending_soon`（<300s，manager.rs:43-50）/`has_budget`（manager.rs:53-59）/`budget_percentage`（clamp≥0，manager.rs:62-71）。

### 3.6 测试覆盖（略读）

- 单元测试（router.rs:327-450，12 个）：只覆盖三个纯函数 `compute_weight / queue_factor_from_len / window_factor_from_info` 的数值边界；**没有任何测试触及 weighted_select 的分布、Router 的策略链、或与 VendorManager 的集成**。
- 集成测试（tests/router_test.rs，11 个 tokio test）：覆盖 exact/abstract/大小写/加权/禁用过滤/并发路由等——同样用假 VendorManager 层面，不涉真实 limiter。

## 4. 时效性专项回答：「路由决策用的指标是否滞后」

结论分三层：

1. **读是实时的**：queue/window/status 都在决策那一刻读取（router.rs:280-282、manager.rs:568-577），没有缓存 TTL，不存在"采样周期滞后"。
2. **写是稀疏的，导致读到的值本身滞后于真实负载**：
   - `used_tokens` 只在两条非流式成功路径累加（queue worker 成功后 manager.rs:307-310；passthrough 成功后 manager.rs:769-773）；**流式路径（execute_stream_request manager.rs:626-683、execute_passthrough_stream_request manager.rs:779-819）完全不记账**；失败请求也不记账（无论成败都该计 tokens 的语义另说，至少成功流式漏了）。
   - `VendorState.status` 生产环境永远是初始值 `Available`（§5.2）。
   - `GlobalMetrics`（请求数/延迟/错误率，utils/metrics.rs，vendor 持有于 manager.rs:122）**根本不参与权重计算**——calculate_weight 只用 queue+window 两路输入（router.rs:279-283）。
3. **结构性缺失**：熔断器状态（CircuitBreaker Open，manager.rs:124-125/172-179）不进入路由过滤或权重——Open 的 vendor 在路由眼里是 `Available`，选中后到 execute 才失败（manager.rs:459-464）。路由对故障没有反馈回路，这不是"滞后"而是"断开"。

## 5. 发现的问题（按影响排序）

### 5.1 【高·正确性】queue_factor 是死因子，恒为 1.0 —— 3D 权重实际只有 2D
- 证据：`get_queue_length` 读 `QueueManager`（vendor/manager.rs:585-590）；`QueueManager` 全代码库只有 `new()`（空 HashMap，queue.rs:60-64）和 `get()`（queue.rs:66-68），**没有任何 insert/注册方法被调用**（grep `queue_manager` 全库仅 4 处：manager.rs:118/144/586/862）。真实请求队列是 `VendorInstance.queue`（manager.rs:77，`execute_request` 在 manager.rs:616 投递），路由从未读它。
- 后果：每家 vendor 的 queue_factor 恒等于 `1 - min(0/100,1) = 1.0`，权重公式退化为 `w×0.4 + 0.3 + window×0.3`，0.3 的常数项对所有候选同值，不产生区分度。AGENTS.md 宣称的 3D 调度名不副实。`get_all_status`（manager.rs:862）读的同样是空表，/admin 页面队列长度恒 0。

### 5.2 【高·正确性】VendorState 状态机无生产写入方，健康过滤名存实亡
- 证据：`mark_busy / mark_available / mark_throttled / mark_unavailable / update_status / inc_error / reset_error` 的调用方 grep 全库为**零**（仅定义于 vendor/status.rs 与 vendor/manager.rs:821-855 的转发壳）。
- 后果：`instance.state.get_status()` 恒 `Available`；router.rs:105/181 的健康检查实际只剩 `advanced_limiter.check()`。对 `Interval` 配置的 vendor check 恒 true（rate_limiter.rs:134）、对 `None` 配置恒 true——即这两类 vendor **任何状态下都会被路由**（包括上游持续 5xx 时，因为没有任何代码把失败反馈进状态或权重）。

### 5.3 【高·可用性】熔断器（CircuitBreaker）与路由完全脱节
- 证据：CB 只在 `execute_http_with_retry` 被包装使用（manager.rs:445-465）；`get_circuit_state`（manager.rs:172-179）仅供观测/测试。路由过滤链（router.rs:176-183）不查 CB。
- 后果：熔断 Open 的 vendor 继续以满权重参与加权随机，流量照常选中→在 execute 处统一失败。对上层表现为"随机部分请求失败"而非"自动避开故障 vendor"。重试逻辑（500 固定 1s、最多 5 次，manager.rs:525-542）会进一步放大故障期延迟。

### 5.4 【高·正确性】Token 限流的 used_tokens 永不自动重置；流式流量不记账
- 永不重置：后台 worker 每 60s 调 `should_reset()→reset()`（manager.rs:203-214），但 `should_reset` 对 `Token` 变体走 `_ => false`（rate_limiter.rs:179-193）——`Token.last_reset` 字段从未参与比较。累计消费达 `max_tokens` 后 `check()` 永久 false（rate_limiter.rs:135-139）→ `get_status` 恒 `Throttled` → **vendor 被路由永久排除，直至进程重启**。
- 流式不记账：`consume_tokens` 仅 manager.rs:307-310（队列 worker 非流式）与 manager.rs:769-773（passthrough 非流式）调用；`execute_stream_request`（manager.rs:626-683）与 `execute_passthrough_stream_request`（manager.rs:779-819）无任何记账。流式为主的负载下 window_factor 长期高估预算 → 持续过量路由到该 vendor，可能打爆上游真实配额。

### 5.5 【中·一致性】try_direct_route 无健康/限流检查
- 证据：只查 `vendor.enabled`（router.rs:150-152），对比 `try_abstract_route`（router.rs:181）与 `try_vendor_route`（router.rs:105）都查 `get_status`。
- 后果：`vendor.model` 直连格式可把请求路由到 Throttled（Token 耗尽/窗外）的 vendor；而同样条件走 abstract 路径会被过滤。同一路由器两条路径语义不一致。

### 5.6 【中·性能】每请求 O(候选数) 次 VendorConfig 级深拷贝 + O(V×M) 全表扫描，无索引
- 证据链：`resolve_model` 命中即克隆（config/manager.rs:83）；过滤阶段每个候选 `get_status→get_vendor` 又克隆一次完整 VendorConfig（vendor/manager.rs:564 → config/manager.rs:52-58，只为读一个 bool）；`try_global_route` 克隆全部 enabled (vendor,model) 对（router.rs:211）；`weighted_select` 选中后再克隆（router.rs:242/255/264）；`RoutingDecision` 持有 owned `VendorConfig`（router.rs:11-15）。锁方面：每次 `get_vendor`/`resolve_model`/`read` 都拿一次 std RwLock 读锁，路由一次请求拿锁 O(候选数) 次（无跨 await 持锁，无死锁风险，但有争用与克隆放大）。
- 量级：V 个 vendor、每家 M 个模型时，`auto` 路由一次请求 ≈ V×M 次 VendorConfig 深拷贝。vendor/模型数增长时每请求成本线性上涨。现成的索引构建器 `get_all_model_mappings`（config/manager.rs:109-137）闲置。

### 5.7 【中·能力缺失】fallback / priority 策略不存在；FallbackChain 是未接线的死代码
- 证据：router/ 内只有加权随机；`FallbackChain/FallbackTarget/FallbackTrigger`（vendor/resilience/fallback.rs，含完整 priority 降级逻辑 fallback.rs:76-99）**全库零调用方**（grep 仅 resilience/mod.rs 再导出）。`ModelConfig`/`VendorConfig` 均无 priority 字段（config/mod.rs:44-76）。
- 相关：`route_with_hints` 指定 vendor 不可用时直接硬报错，不降级到其他候选（router.rs:77-80，注释自知）。AGENTS.md「修改路由策略: weighted / fallback / priority」与代码不符。

### 5.8 【低·正确性】大小写处理三条路径不一致；direct 格式可能被 abstract 抢走
- 证据：入口统一 lowercase（router.rs:39），但 `try_direct_route` 用已 lowercase 的输入与**未 lowercase** 的 `m.model_id / abstract_id` 精确比较（router.rs:159-160），vendor id 查找也区分大小写（config/manager.rs:56）；而 `resolve_model`（config/manager.rs:61,76-82）与 `try_vendor_route`（router.rs:111,117-120）双侧 lowercase。配置含大写 model_id 时 direct 格式不可达。
- 另外 abstract 先于 direct 执行（router.rs:43→48），且 `resolve_model` 把裸 model_id 也当匹配键——输入 `openai.gpt-4o` 若恰有其他 vendor 的 model_id 叫 `openai.gpt-4o`，会被 abstract 抢先并可能路由到**不同 vendor**。含 `.` 的 model_id（如 `gpt-4.1`）靠 abstract 兜住，direct 的 `splitn('.',2)` 会把它误拆（router.rs:142-147），属于隐性耦合。

### 5.9 【低·正确性】热更新新增的 vendor：路由认为可用、执行必失败
- 证据：`initialize()` 仅启动时调用一次（main.rs:42 → manager.rs:195-217，实例只在 `get_or_create_instance` 创建）；`/admin/config` 热更新只走 `config.update_config`（admin.rs:134/158/245 → config/manager.rs:30-50），不触发实例创建。
- 后果：instances 无条目的 vendor，`get_status` 返回 `Available`（manager.rs:579），`get_window_info` 返回 None（factor 0.5 中性）→ 路由可选中；`execute_request` 却因 instance 缺失报 `VendorUnavailable`（manager.rs:597-605）。另外热更新改 `model.weight` 对路由即时生效（读 config），改 vendor 增删则半生效——行为不对称。

### 5.10 【低·语义】TimeWindow 窗外 window_factor=1.0（最高分），靠过滤顺序掩盖
- 证据：`get_window_info` 窗外返回 `(None, None)`（manager.rs:878-880）→ `has_budget()=true`、`budget_percentage()=1.0`、`is_ending_soon()=false` → factor 1.0；同一时刻 `check()` 返回 false → `get_status=Throttled` 被 router.rs:181 过滤。
- 两条读数路径对同一状态给出相反评价，当前靠"先过滤后加权"的顺序兜住；任何复用 `get_window_info` 做调度/展示的代码都会拿到误导值。`Interval` 变体类似：`check()` 恒 true（rate_limiter.rs:134），Interval 限流在任何路径（manager.rs:276/642/726/798）都不会真正拒绝请求，只影响 is_ending_soon 减半惩罚。

### 5.11 【低·代码卫生】
- `try_*` 系列与 `weighted_select` 声明为 async/伴生但内部无任何 await 点（router.rs:94/141/171/192），async 是装饰性的；
- 总权重 ≤0 的均匀随机兜底（router.rs:239-246）会给"预算耗尽但未被过滤"的 0 权重候选被选中的机会（浮点累加路径不会，随机索引路径会）；
- 测试未覆盖 weighted_select 分布与策略链集成（§3.6）。

## 6. 优化切入点（按 预期收益/改动成本 排序）

1. **复活 queue_factor**（一行级）：`get_queue_length` 改读 `instance.queue.len()`（VendorQueue::len 已存在，queue.rs:39-41），或把 instance.queue 注册进 QueueManager；顺带修 `get_all_status`（manager.rs:862）同样的空读。恢复 PRD 承诺的 3D 调度。
2. **把熔断器状态接进路由**：`get_status` 中并入 `circuit_breaker.state()`（Open→Unavailable / HalfOpen→Busy），或 `Router` 过滤时直接查 `get_circuit_state`（manager.rs:172-179 现成）。故障 vendor 自动退出加权池。
3. **修 Token 重置 + 流式记账**：`should_reset` 的 Token 分支用 `last_reset` 比较（rate_limiter.rs:160-174）；流式路径在 SSE 终止事件或流结束时 `consume_tokens`（上游 usage 在流尾 chunk 里）。防止 vendor 被永久排除/预算高估。
4. **补 try_direct_route 的健康检查**：加一行 `get_status==Available`（对齐 router.rs:105/181）。
5. **候选解析建索引 + 去 owned 化**：热更新时用 `get_all_model_mappings` 重建 `HashMap<abstract_id, Vec<key>>`；`RoutingDecision` 改持 `Arc<VendorConfig>`，`get_status` 增加只读 enabled 的无克隆路径（或 `contains_vendor`）。每请求深拷贝从 O(V×M) 降为 O(1)。
6. **失败反馈第 4 因子**：GlobalMetrics 已按 vendor 记录错误率/延迟（vendor/manager.rs:122, 698-701），可作为权重因子替换或补充死掉的 queue 维度；`mark_throttled/inc_error`（manager.rs:842-855）在 retry 分类处接线即可让 VendorState 状态机活过来。
7. **统一大小写规范**：比较处双侧 `to_lowercase`（对齐 config/manager.rs:76-82 的行为）。
8. **接线或删除 FallbackChain**：已有完整实现（vendor/resilience/fallback.rs），接入 api 层"execute 失败后沿 priority 降级重路由"即可补齐 AGENTS.md 声称的能力；否则删除避免维护误导。

## 7. 检查执行记录

| 检查 | 命令 | 结果 |
|------|------|------|
| router 单元测试 | `cargo test --lib router` | **未能运行**：`error: failed to get dbnexus ... failed to read /home/dev/projects/dbnexus/Cargo.toml (os error 2)`——兄弟库路径依赖在本环境缺失 |
| queue_manager 写入方排查 | `grep -rn queue_manager src/` | 仅 4 处（声明/初始化/两个读点），无 insert |
| 状态机写入方排查 | `grep -rn "mark_busy\|mark_available\|mark_throttled\|update_status\|inc_error\|reset_error" src/`（排除定义文件） | 零调用方 |
| FallbackChain 调用方排查 | `grep -rn "FallbackChain\|FallbackTarget\|FallbackTrigger" src/`（排除定义文件） | 零调用方 |
| 路由入口调用方 | `grep` api 层 | api/openai.rs:41-48,253；api/anthropic.rs:33 |
| 初始化/热更新调用点 | `grep -rn "\.initialize()\|get_or_create_instance\|update_config" src/` | initialize 仅 main.rs:42；update_config 三处均不重建实例 |
