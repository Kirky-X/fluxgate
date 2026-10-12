# zh/api: api 桶（src/api）用户可见消息。zh 值 = 原中文保留或忠实翻译。

# ---- admin.rs ----
admin-vendor-reset = 厂商 { $id } 的密钥池黑名单与冷却状态已清空
admin-config-updated-immediate = 配置已更新；vendors/pricing/auth 立即生效
admin-config-updated-restart-required = 配置已更新；vendors/pricing/auth 立即生效；{ $sections } 重启后生效
admin-vendor-not-found = 厂商 { $id } 不存在
# ---- /admin/models 模型能力目录 CRUD ----
admin-model-upserted = 模型 { $model } 已登记至目录
admin-model-not-found = 模型 { $model } 不在目录中
admin-model-deleted = 模型 { $model } 已从目录移除
admin-vendor-updated = 厂商已更新
admin-global-model-updated = 全局模型配置已更新
admin-only-global-model-supported = 仅支持 'global' 模型配置
# ---- admin.rs /admin/cost 成本分账查询 ----
admin-cost-group-by-invalid = group_by（必须为 vendor|model）
admin-cost-date-format-invalid = { $name }（必须为 YYYY-MM-DD）
admin-cost-usage-sink-unwired = usage sink 未接线；成本分账不可用
admin-vendor-reset-unknown-vendor = 厂商 '{ $id }'（未知厂商）

# ---- router_builder.rs ----
router-rate-limit-exceeded = 已超过速率限制
router-admission-rejected = 准入被拒绝（拥塞）
router-access-denied = 访问被拒绝
router-governor-init-failed = Governor 初始化失败
router-inbound-rate-limit-init-failed = 入站限流初始化失败

# ---- api/mod.rs ----
api-log-truncated = ...[已截断，共 { $total_bytes } 字节]
api-amplification-diverging = 重试放大发散：r·f > 0.95（λ_eff 无界）
api-amplification-tightening = 失败率逼近临界 f_c；本请求 max_total_attempts 收紧为 1
api-vendor-failed-rerouting = vendor 失败；切换 fallback 重路由
api-vendor-failed-rerouting-pre-stream = vendor 在响应/流建立前失败；切换 fallback 重路由

# ---- api/openai.rs / api/anthropic.rs ----
api-log-push-rejected = 请求日志推送被拒：{ $error }
api-usage-record-rejected = Token 用量记账被拒：{ $error }
api-embeddings-openai-only = embeddings 仅支持 OpenAI 类型厂商

# ---- api/response_cache.rs ----
api-response-cache-leader-not-cached = 响应缓存：leader 未缓存，follower 自愈回源
api-response-cache-library-fault = 响应缓存：库侧故障，直接派发

# ---- api/count_tokens.rs / api/route_explain.rs ----
api-count-tokens-model-invalid = model（缺失或非字符串）
api-count-tokens-messages-invalid = messages（缺失或非数组）
api-count-tokens-messages-empty = messages（不得为空）
api-route-explain-model-invalid = model（缺失或非字符串）
api-route-explain-model-unknown = model '{ $model }'（未知模型）

# ---- 不变量 panic 文案（api/{anthropic,openai}.rs 最终路由 / api/route_explain.rs 入选者字段）----
api-final-route-missing = leader/自愈/未参与路径闭包必产出最终路由
api-explain-no-factors = 入选者必有分解
api-explain-no-weight = 入选者必有权重
api-explain-no-base = 入选者必有合成基准
api-explain-no-selection = 入选者必有档位

# ---- api/health.rs（#[forge] description，经 sdforge::i18n::register_translation 运行时注册） ----
api-health-description = 健康检查端点

# ---- api/openapi_docs.rs（OpenAPI description，经 sdforge::i18n::register_translation 运行时注册） ----
api-openapi-models-list-description = 模型目录与已启用厂商模型的并集，去重；不缓存
api-openapi-count-tokens-description = 近似输入 token 估算（字符数/4 + 每条消息结构开销，确定性）
api-openapi-route-explain-description = 只读的逐候选路由评估（原因码 + 权重分解）
api-openapi-cost-usage-description = 按厂商或模型聚合的 token 与成本分账，支持可选时间窗与 API key 过滤
