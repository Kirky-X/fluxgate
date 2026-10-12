# zh/vendor: src/vendor 用户可见消息（错误构造文本 + tracing warn 文案）。
# 约束：vendor 字段值被 extract_status_code（src/utils/retry.rs）按半角
# 括号结构解析、被 fallback/is_circuit_open_error 按 "circuit open" 子串
# 匹配 —— vendor-* 键保持半角 "(...)" 括号结构；`(circuit open)` 标记
# 不本地化，仍为源码字面量。
vendor-removed-from-config = { $vendor }（已从配置中移除）
vendor-unknown-type = 未知的厂商类型
vendor-api-key-env-not-set = API key 环境变量 { $env } 未设置
# 半角括号 + 状态码结构是 extract_status_code 的解析契约，zh 与 en 同构
vendor-upstream-status = { $vendor } ({ $status })
vendor-parse-error = 解析错误: { $error }
vendor-builder-not-cloneable = 厂商 { $vendor } 的请求构建器不可克隆
vendor-stream-error = { $vendor } (流错误: { $detail })
vendor-stream-interrupted = { $vendor } (流已中断: { $detail })
vendor-upstream-error = 上游错误
vendor-upstream-non-success = 上游厂商返回非成功状态
vendor-stream-upstream-non-success = 流式上游返回非成功状态
vendor-upstream-stream-interrupted = 上游流中断
vendor-stream-cooldown = 厂商 { $vendor } 流中失败，冷却 { $cooldown_ms } 毫秒后重新参与路由
vendor-token-quota-underflow = token 配额下溢：用量未记账（将超出窗口上限）
vendor-pool-semaphore-closed = 连接池信号量已关闭: { $error }
# ---- key_pool.rs（vendor 内 key 池）----
vendor-key-pool-env-missing = 厂商 { $vendor } 的 key 池环境变量 #{ $index }（值长度 { $length }）未设置，已跳过——请核对是否误将 key 原文当作环境变量名
vendor-key-pool-truncated = 厂商 { $vendor } 的 key 池超出上限 { $cap }，已截断（丢弃 { $dropped } 条）
vendor-key-pool-fallback-legacy = 厂商 { $vendor } 的 key 池为空（全部 env 缺失），回退既有单 key 行为
vendor-key-rotated = 厂商 { $vendor } 认证失败 ({ $status })，已推进 key 轮换游标，下次尝试使用下一把 key
vendor-key-pool-shrunk = 厂商 { $vendor } 的 key 池由 { $before } 把收缩至 { $after } 把（{ $missing } 个池 env 现缺失）
vendor-key-blacklisted = key 已进黑名单（永久错误，不自动恢复）
vendor-cost-unregistered-model = 模型未登记 model_catalog，不计价（成本报表按设计少计）

# ---- probe.rs（主动健康检查探针：配置热读告警 + 周期运维日志）----
vendor-probe-unknown-mode = 未知 [health].mode，按 off 处理（fail-safe，防误配探针风暴）
vendor-probe-unknown-strategy = 未知 [health].probe_strategy，按 free 处理
vendor-probe-target-missing = 探针目标实例缺失，跳过
vendor-probe-task-error = 探针任务异常结束（保持现状）: { $error }
vendor-probe-cycle-done = 健康探针周期完成
vendor-probe-free-recovered = 健康探针 free 成功：vendor 冷却已清除（被动恢复）
vendor-probe-url-build-failed = 探针 URL 构造失败，保持现状
vendor-probe-free-non-2xx = free 探测非 2xx，保持现状
vendor-probe-free-failed = free 探测失败，保持现状
vendor-probe-minimal-recovered = 健康探针 minimal_call 成功：key 冷却/黑名单已清除（被动恢复）
vendor-probe-minimal-non-2xx = minimal_call 探测非 2xx，保持现状
vendor-probe-minimal-failed = minimal_call 探测失败，保持现状
vendor-probe-semaphore-closed = 探针信号量永不关闭

# ---- manager.rs extra_headers 注入告警 ----
vendor-extra-header-conflict = extra_headers 与网关头冲突，已跳过
vendor-extra-header-name-invalid = extra_headers 头名非法，已跳过
vendor-extra-header-value-invalid = extra_headers 头值非法，已跳过
