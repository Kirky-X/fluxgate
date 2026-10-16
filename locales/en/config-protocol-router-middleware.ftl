# en/config-protocol-router-middleware: src/{config,protocol,router,middleware} 用户可见消息
# 通道：Config/Validation/Protocol 错误进 HTTP 响应与 SSE error 帧；其余为 tracing 运维日志。

# ---- config/loader.rs ----
config-file-not-found = Config file not found: { $path }
config-load-failed = Failed to load config file: { $error }
config-parse-failed = Failed to parse config file: { $error }
config-duplicate-vendor-id = Duplicate vendor ID: { $vendor }
config-duplicate-model-id = Duplicate model ID in vendor { $vendor }: { $model }
config-vendor-id-empty = Vendor ID is empty (whitespace only) — routing lookup keys must not be blank
config-model-id-empty = Model ID in vendor { $vendor } is empty (whitespace only)
config-abstract-id-empty = abstract_id of model { $model } in vendor { $vendor } is empty (whitespace only)
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
# 非 Critical 违规不拒绝启动，但必须留痕（错误显性化）。
config-non-critical-admin-secret-violation = non-critical admin secret rule violation: { $message }

# ---- config/manager.rs ----
config-snapshot-serialize-failed = Config snapshot skipped: failed to serialize old config
config-snapshot-failed = Config snapshot failed (update not blocked): { $error }
config-snapshot-saved = Config snapshot saved: { $path }
config-serialize-failed = Failed to serialize config: { $error }
config-path-unknown = config path unknown: ConfigManager not created via ConfigLoader; refusing to persist update
config-disk-modified-refusing-write = { $path } was modified outside this process since it was last loaded or written back; refusing to overwrite it — reload or restart to apply the external edit first (pending edits may target restart-only sections such as the vendor topology)
config-tmp-write-failed = Failed to write config tmp file { $path }: { $error }
config-atomic-replace-failed = Failed to atomically replace { $path }: { $error }

# ---- protocol/{anthropic,openai}_converter.rs、sse_frame.rs ----
protocol-unknown-error = Unknown error
protocol-anthropic-error = Anthropic error: { $type } - { $message }
protocol-openai-error = OpenAI error: { $type } - { $message }
protocol-passthrough-stream-interrupted = upstream passthrough stream interrupted
protocol-stream-error-frame = Upstream stream interrupted; the stream was terminated after partial output.
protocol-tool-buffer-malformed = dropped malformed tool buffer: both id and name are empty
# anthropic 自动缓存断点注入：注入计数与未知锚点跳过（调试留痕）。
protocol-anthropic-cache-breakpoints-injected = anthropic cache breakpoints injected
protocol-anthropic-unknown-cache-anchor = unknown cache breakpoint anchor, skipped

# ---- router/router.rs ----
router-model-not-found = Model { $model } not found
router-global-model-disabled = Model { $model } not found: global model routing is disabled ([global_model].enabled=false)
router-vendor-unavailable = Vendor { $vendor } unavailable or not configured
router-fail-open-engaged = all candidates cooling; fail-open fallback engaged
router-global-fail-open-engaged = all global candidates cooling; fail-open fallback engaged
router-explain-not-in-catalog = Vendor { $vendor } does not provide model '{ $model }'
router-explain-throttled = Rate-limit window exhausted (window has no budget)
router-explain-cooling-down = Vendor status { $status } (not Available)
router-explain-key-pool-exhausted = Key pool exhausted (all { $n }/{ $n } keys are blacklisted: { $prints })
router-explain-affinity-bound-elsewhere = Session affinity bound to '{ $bound }' (hard mode is exclusive)
router-explain-queue-over-limit = Queue depth { $queue_len } ≥ saturation point { $saturation }
router-explain-weight-zero = Composite weight is 0 (production roulette may still pick uniformly when all weights are zero; diagnostic only)
router-explain-eligible = Eligible candidate (selection={ $selection })

# ---- middleware/auth.rs ----
middleware-missing-api-key = Missing API key
middleware-invalid-api-key = Invalid API key

# ---- router/difficulty (config validation) ----
config-difficulty-weights-all-zero = [routing.difficulty] w_clf and w_rule must not both be 0
config-difficulty-threshold-order = [routing.difficulty] score_low ({ $low }) must be less than score_high ({ $high })
config-difficulty-policy-invalid = [routing.difficulty] mid_band_policy "{ $policy }" is invalid (strong/cheap/nearest)
config-difficulty-clf-feature-missing = [routing.difficulty] enabled with w_clf>0 requires the difficulty-classifier feature at compile time
config-difficulty-tier-empty = [routing.difficulty.tiers] tier { $tier } has an empty abstract model name
config-difficulty-tier-collision = [routing.difficulty] { $kind } "{ $name }" collides with a real model_id (tiers/groups must point at abstract names only)
config-difficulty-tier-no-candidate = [routing.difficulty.tiers] tier { $tier } "{ $name }" has no enabled candidate (needs an enabled vendor serving a matching model_id/abstract_id)
config-difficulty-group-collision = [routing.difficulty] group name "{ $name }" collides with a real model_id
config-difficulty-tiers-identical = [routing.difficulty] all tiers resolve to the identical candidate set; difficulty routing has no vendor-selection effect (advisory)
config-difficulty-field-invalid = [routing.difficulty] field { $field } has invalid value { $value } (weights must be finite non-negative, thresholds within [0,1], timeout_ms within [1,60000], failure_threshold >= 1, engine must be candle or onnx)

# ---- router/difficulty/orchestrator.rs 熔断内分类降级运维日志 ----
# F5/F6 降级留痕：全部为 tracing DEBUG，仅熔断关闭恢复时升 INFO。
difficulty-classifier-recovered = difficulty classifier recovered (breaker closed)
difficulty-classifier-failed = difficulty classifier failed
difficulty-classifier-task-panicked = difficulty classifier task panicked
difficulty-classifier-timeout = difficulty classifier timeout
# ---- POST /admin/route/difficulty-explain 的 classifier.error 诊断串 ----
# $p 为分类器返回的越界概率原值；$error / $join_error 为 Debug 透传。
difficulty-explain-invalid-probability = invalid probability: { $p }
difficulty-explain-task-panicked = task panicked: { $join_error }
difficulty-explain-timeout = timeout
difficulty-explain-not-wired = classifier not wired
difficulty-explain-breaker-open = breaker open
difficulty-explain-not-consulted = not consulted
# ---- router/difficulty/vecboost_engine.rs 引擎载荷（经 explain 的 error 字段透出）----
difficulty-laya-engine-load-failed = laya engine load failed: { $error }
difficulty-laya-decide-failed = laya decide failed: { $error }
difficulty-laya-request-rejected = decision request rejected: { $error }
