# en/api: api 桶（src/api）用户可见消息。en 值 = 迁移前原英文原文（忠实保留）。

# ---- admin.rs ----
admin-vendor-limit-auto-managed = Vendor rate-limit state is managed automatically by the limiteron Governor
admin-config-updated-immediate = Config updated; vendors/pricing/auth take effect immediately
admin-config-updated-restart-required = Config updated; vendors/pricing/auth take effect immediately; { $sections } take effect after restart
admin-vendor-not-found = Vendor { $id } not found
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
api-embeddings-openai-only = Embeddings only supported for OpenAI vendors

# ---- api/health.rs（#[forge] description，经 sdforge::i18n::register_translation 运行时注册） ----
api-health-description = Health check endpoint
