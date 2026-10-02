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
vendor-token-quota-underflow = token 配额下溢：用量未记账（将超出窗口上限）
vendor-pool-semaphore-closed = 连接池信号量已关闭: { $error }
vendor-circuit-error = { $vendor } (熔断错误: { $detail })
