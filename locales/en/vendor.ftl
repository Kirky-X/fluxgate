# en/vendor: src/vendor 用户可见消息（错误构造文本 + tracing warn 文案）。
# 约束：vendor 字段值被 extract_status_code（src/utils/retry.rs）按半角
# 括号结构解析、被 fallback/is_circuit_open_error 按 "circuit open" 子串
# 匹配 —— 相关结构在此保持原样；`(circuit open)` 标记不本地化，仍为
# 源码字面量。
vendor-removed-from-config = { $vendor } (removed from config)
vendor-unknown-type = Unknown vendor type
vendor-api-key-env-not-set = API key env { $env } not set
# 半角括号 + 状态码结构是 extract_status_code 的解析契约，en/zh 同构
vendor-upstream-status = { $vendor } ({ $status })
vendor-parse-error = Parse error: { $error }
vendor-builder-not-cloneable = request builder not cloneable for vendor { $vendor }
vendor-stream-error = { $vendor } (stream error: { $detail })
vendor-stream-interrupted = { $vendor } (stream interrupted: { $detail })
vendor-upstream-error = upstream error
vendor-upstream-non-success = upstream vendor returned non-success status
vendor-stream-upstream-non-success = streaming upstream returned non-success status
vendor-upstream-stream-interrupted = upstream stream interrupted
vendor-token-quota-underflow = token quota underflow: usage not accounted (would exceed window cap)
vendor-pool-semaphore-closed = connection pool semaphore closed: { $error }
# ---- key_pool.rs（vendor 内 key 池）----
vendor-key-pool-env-missing = key pool environment variable #{ $index } (value length { $length }) of vendor { $vendor } is not set; skipped — verify it is an environment variable NAME, not a key pasted by mistake
vendor-key-pool-truncated = key pool of vendor { $vendor } exceeds the cap of { $cap }; truncated ({ $dropped } dropped)
vendor-key-pool-fallback-legacy = key pool of vendor { $vendor } is empty (all envs missing); falling back to legacy single-key behavior
vendor-key-rotated = vendor { $vendor } auth failure ({ $status }); key cursor advanced, the next attempt uses the next key
vendor-key-pool-shrunk = key pool of vendor { $vendor } shrank from { $before } to { $after } keys ({ $missing } pool envs now missing)
vendor-cost-unregistered-model = model not registered in model_catalog; not priced (cost report under-counts by design)
