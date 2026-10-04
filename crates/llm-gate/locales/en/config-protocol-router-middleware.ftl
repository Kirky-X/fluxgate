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
config-circuit-open-timeout-too-small = resilience.circuit_open_timeout_ms ({ $value }ms) is below the 100ms floor: a too-short window flips the circuit straight from Open to Half-Open, degrading failure recovery into a probe storm

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
protocol-stream-interrupted = upstream stream interrupted: { $error }

# ---- router/router.rs ----
router-model-not-found = Model { $model } not found
router-vendor-unavailable = Vendor { $vendor } unavailable or not configured
router-fail-open-engaged = all candidates cooling; fail-open fallback engaged
router-global-fail-open-engaged = all global candidates cooling; fail-open fallback engaged

# ---- middleware/auth.rs ----
middleware-missing-api-key = Missing API key
middleware-invalid-api-key = Invalid API key
