# en/api: api 桶（src/api）用户可见消息。en 值 = 迁移前原英文原文（忠实保留）。

# ---- admin.rs ----
admin-vendor-reset = Vendor { $id } key pool blacklist and cooldown cleared
admin-config-updated-immediate = Config updated; vendors/pricing/auth take effect immediately
admin-config-updated-restart-required = Config updated; vendors/pricing/auth take effect immediately; { $sections } take effect after restart
admin-vendor-not-found = Vendor { $id } not found
# ---- /admin/models 模型能力目录 CRUD ----
admin-model-upserted = model { $model } upserted into the catalog
admin-model-not-found = model { $model } not found in the catalog
admin-model-deleted = model { $model } removed from the catalog
admin-vendor-updated = Vendor updated
admin-global-model-updated = Global model config updated
admin-only-global-model-supported = Only 'global' model config is supported
# ---- admin.rs /admin/cost 成本分账查询 ----
admin-cost-group-by-invalid = group_by (must be vendor|model)
admin-cost-date-format-invalid = { $name } (must be YYYY-MM-DD)
admin-cost-usage-sink-unwired = usage sink is not wired; cost usage unavailable
admin-token-stats-usage-sink-unwired = usage sink is not wired; token stats unavailable
admin-vendor-reset-unknown-vendor = vendor '{ $id }' (unknown vendor)
# ---- admin.rs /admin/embedding-pins 嵌入 pin 管理 ----
admin-embedding-pins-unwired = embedding pin table is not wired; endpoint unavailable
admin-embedding-pin-reset = embedding pin for '{ $abstract }' cleared; the next embedding request re-pins by weight (may land on a different vendor)
admin-embedding-pin-reset-absent = no embedding pin recorded for '{ $abstract }'; nothing to clear

# ---- router_builder.rs ----
router-rate-limit-exceeded = Rate limit exceeded
router-admission-rejected = Admission rejected (congestion)
router-access-denied = Access denied
router-governor-init-failed = Governor init failed
router-inbound-rate-limit-init-failed = inbound rate limit init failed
# 全局 tower 限流层内层错误：原文只进日志，响应体为通用 500 文案。
router-global-rate-limit-layer-error = global rate limit layer error: { $error }
router-internal-server-error = internal server error

# ---- readiness.rs（就绪探针组件检查失败：details 只给固定码，原文进日志）----
api-readiness-db-probe-failed = readiness db probe failed: { $error }
api-readiness-db-session-failed = readiness db session failed: { $error }
api-readiness-cache-probe-failed = readiness cache probe failed: { $error }

# ---- api/mod.rs ----
api-log-truncated = ...[truncated, { $total_bytes } bytes total]
api-amplification-diverging = retry amplification diverging: r·f > 0.95 (λ_eff unbounded)
api-amplification-tightening = failure rate approaching critical f_c; tightening max_total_attempts to 1 for this request
api-vendor-failed-rerouting = vendor failed; rerouting to fallback
api-vendor-failed-rerouting-pre-stream = vendor failed before response/stream; rerouting to fallback

# ---- api/openai.rs / api/anthropic.rs ----
api-log-push-rejected = request log push rejected: { $error }
api-usage-record-rejected = token usage record rejected: { $error }
api-embeddings-openai-only = Embeddings only supported for OpenAI vendors
api-rerank-capability-mismatch = Rerank request resolved to a vendor or model without rerank capability: check X-Vendor/X-Model hints, or declare model_type = "rerank" on the entry
api-embeddings-dimension-mismatch = Embedding dimension drift from { $vendor }/{ $model }: first response had { $old } dimensions, now { $new } — vectors from different spaces must not mix; reset the pin via POST /admin/embedding-pins/{ $abstract }/reset if the upstream model changed

# ---- api/response_cache.rs ----
api-response-cache-leader-not-cached = response cache: leader-not-cached, follower self-heals from upstream
api-response-cache-library-fault = response cache: library fault, dispatching directly

# ---- api/semantic_cache.rs ----
# BgeEmbedder::load 的失败载荷：经 bootstrap 的 %error 字段落到日志面。
api-semantic-cache-embedder-load-failed = semantic cache embedder load failed: { $error }
# BgeEmbedder::embed 的失败载荷：消费点（lookup/store）静默计数丢弃，无用户可见面。
api-semantic-cache-embed-failed = semantic cache embed failed: { $error }

# ---- api/count_tokens.rs / api/route_explain.rs ----
api-count-tokens-model-invalid = model (missing or not a string)
api-count-tokens-messages-invalid = messages (missing or not an array)
api-count-tokens-messages-empty = messages (must not be empty)
api-route-explain-model-invalid = model (missing or not a string)
api-route-explain-model-unknown = model '{ $model }' (unknown model)

# ---- 不变量 panic 文案（api/{anthropic,openai}.rs 最终路由 / api/route_explain.rs 入选者字段）----
api-final-route-missing = leader/self-heal/non-participant path closure must produce a final route
api-explain-no-factors = eligible candidate must carry a weight-factor breakdown
api-explain-no-weight = eligible candidate must carry a weight
api-explain-no-base = eligible candidate must carry a composite base weight
api-explain-no-selection = eligible candidate must carry a selection tier

# ---- api/health.rs（#[forge] description，经 sdforge::i18n::register_translation 运行时注册） ----
api-health-description = Health check endpoint

# ---- api/openapi_docs.rs（OpenAPI description，经 sdforge::i18n::register_translation 运行时注册） ----
api-openapi-models-list-description = Union of model catalog and enabled vendor models, deduplicated; vendor entries list their routable public name (abstract_id, else model_id). Never cached.
api-openapi-count-tokens-description = Approximate input token estimation (chars/4 + per-message overhead, deterministic).
api-openapi-route-explain-description = Read-only per-candidate routing assessment (reason codes + weight breakdown).
api-openapi-cost-usage-description = Aggregated token and cost ledger grouped by vendor or model, with optional time window and API key filter.
