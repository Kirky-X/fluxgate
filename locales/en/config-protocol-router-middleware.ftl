# en/config-protocol-router-middleware: src/{config,protocol,router,middleware} 用户可见消息
# 通道：Config/Validation/Protocol 错误进 HTTP 响应与 SSE error 帧；其余为 tracing 运维日志。

# ---- config/loader.rs ----
config-file-not-found = Config file not found: { $path }
config-load-failed = Failed to load config file: { $error }
config-parse-failed = Failed to parse config file: { $error }
config-duplicate-vendor-id = Duplicate vendor ID: { $vendor }
config-duplicate-model-id = Duplicate model ID in vendor { $vendor }: { $model }
config-api-key-env-missing = API key environment variable of vendor { $vendor } is not set (configured value length { $length }) — verify it is an environment variable NAME, not a key pasted by mistake
config-api-key-pool-env-missing = API key pool environment variable #{ $index } (value length { $length }) of vendor { $vendor } is not set — verify it is an environment variable NAME, not a key pasted by mistake
config-duplicate-catalog-model-id = duplicate model catalog id { $model }
config-catalog-context-window-zero = model catalog entry { $model } has context_window = 0; registered models must declare a real window (0 disables TUI auto-compact silently)
config-catalog-too-many = model catalog has { $count } entries; the cap is { $cap }
config-catalog-model-id-empty = model catalog entry has an empty model_id (whitespace only)
config-catalog-price-invalid = model catalog entry { $model } has invalid { $field } = { $value }; prices must be finite and >= 0
config-circuit-open-timeout-too-small = resilience.circuit_open_timeout_ms ({ $value }ms) is below the 100ms floor: a too-short window flips the circuit straight from Open to Half-Open, degrading failure recovery into a probe storm
config-quota-window-too-small = quota.window_secs ({ $value }s) must be at least 1: a zero window flips on every access and the quota never fills
middleware-quota-exceeded = Quota exceeded, please retry later
middleware-quota-body-too-large = Request body exceeds the quota adjudication limit (4 MiB)
# quota.enabled 开而 [auth] 关/缺段的组合告警：tenant 键取自客户端自报头（L9）
config-quota-without-auth = [quota].enabled is on but [auth] is disabled or absent: quota tenant keys come from client-supplied headers without verification, so billing control can be bypassed with forged headers (isolate /admin and data planes, or enable auth)
# $details 为固定序条目明细：位置 + 规则结论（本仓库形态契约或 confers
# JwtSecretValidator 规则内核），只含长度数字，不含条目原文
config-weak-admin-secret = Admin-plane secret rejected at startup (entries must be lowercase SHA256 hex digests): { $details }
config-admin-secret-not-hex-digest = not a lowercase SHA256 hex digest (expected 64 lowercase hex chars, got { $length } bytes)

# ---- config/manager.rs ----
config-snapshot-serialize-failed = Config snapshot skipped: failed to serialize old config
config-snapshot-failed = Config snapshot failed (update not blocked): { $error }
config-snapshot-saved = Config snapshot saved: { $path }
config-serialize-failed = Failed to serialize config: { $error }
config-path-unknown = config path unknown: ConfigManager not created via ConfigLoader; refusing to persist update
config-tmp-write-failed = Failed to write config tmp file { $path }: { $error }
config-atomic-replace-failed = Failed to atomically replace { $path }: { $error }

# ---- protocol/{anthropic,openai}_converter.rs、sse_frame.rs ----
protocol-unknown-error = Unknown error
protocol-anthropic-error = Anthropic error: { $type } - { $message }
protocol-openai-error = OpenAI error: { $type } - { $message }
protocol-passthrough-stream-interrupted = upstream passthrough stream interrupted
protocol-stream-error-frame = Upstream stream interrupted; the stream was terminated after partial output.

# ---- router/router.rs ----
router-model-not-found = Model { $model } not found
router-vendor-unavailable = Vendor { $vendor } unavailable or not configured
router-fail-open-engaged = all candidates cooling; fail-open fallback engaged
router-global-fail-open-engaged = all global candidates cooling; fail-open fallback engaged

# ---- middleware/auth.rs ----
middleware-missing-api-key = Missing API key
middleware-invalid-api-key = Invalid API key
