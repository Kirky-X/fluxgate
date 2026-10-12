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

# ---- router_builder.rs ----
router-rate-limit-exceeded = Rate limit exceeded
router-admission-rejected = Admission rejected (congestion)
router-access-denied = Access denied
router-governor-init-failed = Governor init failed
router-inbound-rate-limit-init-failed = inbound rate limit init failed

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

# ---- 不变量 panic 文案（api/{anthropic,openai}.rs 最终路由 / api/route_explain.rs 入选者字段）----
api-final-route-missing = leader/self-heal/non-participant path closure must produce a final route
api-explain-no-factors = eligible candidate must carry a weight-factor breakdown
api-explain-no-weight = eligible candidate must carry a weight
api-explain-no-base = eligible candidate must carry a composite base weight
api-explain-no-selection = eligible candidate must carry a selection tier

# ---- api/health.rs（#[forge] description，经 sdforge::i18n::register_translation 运行时注册） ----
api-health-description = Health check endpoint
