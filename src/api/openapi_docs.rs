// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAPI 文档面：主 REST 端点经 sdforge inventory 机制注册进全局 spec；
//! 文档路由由 `[docs]`.enabled 开关挂载（默认 false = 404）——开关本身即
//! 访问门（打开后免认证免限流，spec 非敏感）

use std::sync::Once;

/// OpenAPI 版本号（全条目统一；升级大版本时改此处一处）。
const OPENAPI_VERSION: &str = "v1";

/// 提交简单条目（无路径参数）并登记描述翻译键。
///
/// `#[forge]` 宏不可用（手工 `inventory::submit!` 属 const 上下文），
/// 故直接构造 `OpenApiRouteInfo` 填入 `i18n_key`：spec 生成期经
/// `sdforge::i18n::translate_or_fallback` 按当前 locale 查表，未注册的键
/// 回落编译期英文 `description`（见 [`register_openapi_i18n`]）。
macro_rules! submit_route_i18n {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr, $i18n_key:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo {
                path: $path,
                method: $method,
                summary: $summary,
                description: $desc,
                version: OPENAPI_VERSION,
                tags: $tags,
                path_params: &[],
                success_status: None,
                body_params: &[],
                response_type: None,
                response_schema: None,
                i18n_key: Some($i18n_key),
                deprecated: false,
                sunset: None,
                successor: None,
            }
        }
    };
}

/// 提交带 `{id}` 路径参数的条目并登记描述翻译键。
macro_rules! submit_route_with_id_i18n {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr, $id_desc:expr, $i18n_key:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo {
                path: $path,
                method: $method,
                summary: $summary,
                description: $desc,
                version: OPENAPI_VERSION,
                tags: $tags,
                path_params: &[sdforge::openapi::OpenApiPathParam::new(
                    "id", $id_desc, true, "string", ""
                )],
                success_status: None,
                body_params: &[],
                response_type: None,
                response_schema: None,
                i18n_key: Some($i18n_key),
                deprecated: false,
                sunset: None,
                successor: None,
            }
        }
    };
}

/// 提交带 `{model_action}` 路径参数的条目（Gemini `<model>:<action>` 同段）。
macro_rules! submit_gemini_model_action_i18n {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr, $i18n_key:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo {
                path: $path,
                method: $method,
                summary: $summary,
                description: $desc,
                version: OPENAPI_VERSION,
                tags: $tags,
                path_params: &[sdforge::openapi::OpenApiPathParam::new(
                    "model_action",
                    "Gemini model and action (`<model>:<action>`)",
                    true,
                    "string",
                    "",
                )],
                success_status: None,
                body_params: &[],
                response_type: None,
                response_schema: None,
                i18n_key: Some($i18n_key),
                deprecated: false,
                sunset: None,
                successor: None,
            }
        }
    };
}

/// OpenAPI 条目描述翻译注册进 sdforge 运行时翻译注册表（幂等，建站期触发）；
/// en/zh 字面量与 locales/{en,zh}/api.ftl 同源，两处同步维护
static REGISTER_OPENAPI_I18N: Once = Once::new();

pub(crate) fn register_openapi_i18n() {
    REGISTER_OPENAPI_I18N.call_once(|| {
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-models-list-description",
            "Union of model catalog and enabled vendor models, deduplicated; vendor entries list their routable public name (abstract_id, else model_id). Never cached.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-models-list-description",
            "模型目录与已启用厂商模型的并集，去重；厂商条目列可路由的公开名（abstract_id，缺省 model_id）；不缓存",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-count-tokens-description",
            "Approximate input token estimation (chars/4 + per-message overhead, deterministic).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-count-tokens-description",
            "近似输入 token 估算（字符数/4 + 每条消息结构开销，确定性）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-route-explain-description",
            "Read-only per-candidate routing assessment (reason codes + weight breakdown).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-route-explain-description",
            "只读的逐候选路由评估（原因码 + 权重分解）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-cost-usage-description",
            "Aggregated token and cost ledger grouped by vendor or model, with optional time window and API key filter.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-cost-usage-description",
            "按厂商或模型聚合的 token 与成本分账，支持可选时间窗与 API key 过滤",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-chat-completions-description",
            "Unified chat completions (OpenAI wire format) across vendors.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-chat-completions-description",
            "跨厂商统一聊天补全（OpenAI wire 形态）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-messages-description",
            "Unified messages API (Anthropic wire format) across vendors.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-messages-description",
            "跨厂商统一 messages API（Anthropic wire 形态）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-embeddings-description",
            "Unified embeddings API across vendors.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-embeddings-description",
            "跨厂商统一 embeddings API",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-rerank-description",
            "Unified rerank API (Cohere wire format) across vendors; non-streaming, documents capped at 1000 per request.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-rerank-description",
            "跨厂商统一重排 API（Cohere wire 形态）；非流式，单请求 documents 上限 1000 条",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-responses-description",
            "OpenAI Responses API (semantic SSE). Requests convert to the unified chat IR and route across all chat vendors; native-only capabilities (built-in tools, background, prompt templates, file/audio input) route to api_style=\"responses\" vendors. Supports previous_response_id/conversation replay via the gateway store.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-responses-description",
            "OpenAI Responses API（语义化 SSE）。请求转换为统一聊天 IR 并跨全部聊天厂商路由；原生能力（内置工具/后台/prompt 模板/文件与音频输入）路由到 api_style=\"responses\" 厂商。经网关存储支持 previous_response_id/conversation 回放",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-responses-input-tokens-description",
            "Approximate input token estimation for a Responses request (chars/4 + structure overhead, deterministic).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-responses-input-tokens-description",
            "Responses 请求的近似输入 token 估算（字符/4 + 结构开销，确定性）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-responses-get-description",
            "Retrieve a stored Responses object (tenant-scoped).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-responses-get-description",
            "按租户隔离检索已存 Responses 对象",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-responses-delete-description",
            "Delete a stored Responses object (tenant-scoped).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-responses-delete-description",
            "按租户隔离删除已存 Responses 对象",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-responses-cancel-description",
            "Cancel a background Responses job and return the cancelled response object.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-responses-cancel-description",
            "取消后台 Responses 任务并返回 cancelled 响应对象",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-gemini-generate-description",
            "Gemini native API (`generateContent`, `streamGenerateContent`, `countTokens`, `embedContent`, `batchEmbedContents`). Requests convert to the unified chat IR and route across vendors; safetySettings/cachedContent/built-in tools route to type_=\"gemini\" vendors.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-gemini-generate-description",
            "Gemini 原生 API（generateContent、streamGenerateContent、countTokens、embedContent、batchEmbedContents）。请求转换为统一聊天 IR 并跨厂商路由；safetySettings/cachedContent/内置工具路由到 type_=\"gemini\" 厂商",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-gemini-model-description",
            "Return one Gemini model entry from the gateway catalog.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-gemini-model-description",
            "从网关目录返回单个 Gemini 模型条目",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-gemini-models-list-description",
            "List routable models in Gemini `models[]` shape (abstract_id preferred, deduplicated) with supportedGenerationMethods.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-gemini-models-list-description",
            "以 Gemini models[] 形态列出可路由模型（abstract_id 优先、去重）并附 supportedGenerationMethods",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-logs-description",
            "Paginated request log query with filters.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-logs-description",
            "带过滤条件的分页请求日志查询",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-logs-search-description",
            "Keyword search over request logs.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-logs-search-description",
            "请求日志关键词搜索",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-tokens-stats-description",
            "Aggregated token usage statistics.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-tokens-stats-description",
            "聚合 token 用量统计",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-sessions-description",
            "Session-level aggregates: cost, tokens, request count, first/last timestamps and a title from the first user message. Returns 404 when [usage_insights] is disabled.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-sessions-description",
            "会话级聚合：成本、token、请求数、首末时间戳与首条 user 消息标题。[usage_insights] 关闭时返回 404",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-context-stats-description",
            "Per-model context-window occupancy, cache-read share and five-bucket distribution. Returns 404 when [usage_insights] is disabled.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-context-stats-description",
            "逐模型上下文窗口占用率、缓存读取占比与五档分布。[usage_insights] 关闭时返回 404",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-metrics-description",
            "Internal operational metrics snapshot (JSON).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-metrics-description",
            "内部运维指标快照（JSON）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-quota-ledger-description",
            "Per-window quota usage snapshot (tokens/cost used, reset time, scope: tenant/global/vendor). Optional `key` query filters by tenant digest.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-quota-ledger-description",
            "逐窗口配额用量快照（已用 token/成本、重置时间、作用域：租户/全局/厂商）。可选 key 查询按租户摘要过滤",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendor-budgets-description",
            "Declared vendor budgets with live usage, pace consumption ratio and reset countdown.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendor-budgets-description",
            "已声明厂商预算及实时用量、消耗进度比与重置倒计时",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendors-description",
            "Vendor status and pool overview.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendors-description",
            "厂商状态与池概览",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendor-configs-description",
            "Raw VendorConfig list (config view; complements the status view of /admin/vendors).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendor-configs-description",
            "原始 VendorConfig 列表（配置视图；与 /admin/vendors 状态视图互补）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendor-update-description",
            "Atomically replace one vendor config by id (validated read-modify-write under the config write lock). Unknown id yields 400.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendor-update-description",
            "按 id 原子替换单个厂商配置（配置写锁下校验读改写）。未知 id 返回 400",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-config-get-description",
            "Current runtime configuration snapshot.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-config-get-description",
            "当前运行时配置快照",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-config-update-description",
            "Hot-update runtime configuration (validated, atomic).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-config-update-description",
            "热更新运行时配置（校验后原子生效）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-models-description",
            "Model catalog listing.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-models-description",
            "模型目录列表",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendor-status-description",
            "Per-vendor status detail.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendor-status-description",
            "单个厂商的状态明细",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-catalog-sync-description",
            "Fetch the external model catalog source (add-only merge) into [[model_catalog]]; dry_run returns the diff without persisting.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-catalog-sync-description",
            "拉取外部模型目录源（仅新增合并）进 [[model_catalog]]；dry_run 返回差异不落库",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-vendor-reset-description",
            "Reset per-vendor breakers and quotas.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-vendor-reset-description",
            "重置单厂商熔断器与配额",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-model-update-description",
            "Legacy global-strategy update path (kept for compatibility).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-model-update-description",
            "旧版全局策略更新路径（兼容保留）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-model-upsert-description",
            "Create or replace a catalog entry.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-model-upsert-description",
            "创建或替换目录条目",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-admin-model-delete-description",
            "Remove a catalog entry.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-admin-model-delete-description",
            "删除目录条目",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-health-description",
            "Forge-registered liveness endpoint.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-health-description",
            "forge 注册的存活探针端点",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-readyz-description",
            "Aggregated component readiness (DB, cache, vendors).",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-readyz-description",
            "聚合组件就绪度（DB、缓存、厂商）",
        );
        sdforge::i18n::register_translation(
            "en",
            "api-openapi-metrics-description",
            "RED metrics in Prometheus text exposition format.",
        );
        sdforge::i18n::register_translation(
            "zh",
            "api-openapi-metrics-description",
            "Prometheus 文本暴露格式的 RED 指标",
        );
    });
}

// OpenAI 兼容面
submit_route_i18n!(
    "/v1/chat/completions",
    "POST",
    "OpenAI chat completions",
    "Unified chat completions (OpenAI wire format) across vendors.",
    &["openai"],
    "api-openapi-chat-completions-description"
);
submit_route_i18n!(
    "/v1/messages",
    "POST",
    "Anthropic messages",
    "Unified messages API (Anthropic wire format) across vendors.",
    &["anthropic"],
    "api-openapi-messages-description"
);
submit_route_i18n!(
    "/v1/embeddings",
    "POST",
    "OpenAI embeddings",
    "Unified embeddings API across vendors.",
    &["openai"],
    "api-openapi-embeddings-description"
);
submit_route_i18n!(
    "/v1/rerank",
    "POST",
    "Cohere rerank",
    "Unified rerank API (Cohere wire format) across vendors; non-streaming, documents capped at 1000 per request.",
    &["rerank"],
    "api-openapi-rerank-description"
);
submit_route_i18n!(
    "/v1/responses",
    "POST",
    "OpenAI Responses",
    "OpenAI Responses API (semantic SSE). Requests convert to the unified chat IR and route across all chat vendors; native-only capabilities (built-in tools, background, prompt templates, file/audio input) route to api_style=\"responses\" vendors. Supports previous_response_id/conversation replay via the gateway store.",
    &["openai", "responses"],
    "api-openapi-responses-description"
);
submit_route_i18n!(
    "/v1/responses/input_tokens",
    "POST",
    "Responses input tokens",
    "Approximate input token estimation for a Responses request (chars/4 + structure overhead, deterministic).",
    &["openai", "responses"],
    "api-openapi-responses-input-tokens-description"
);
submit_route_with_id_i18n!(
    "/v1/responses/{id}",
    "GET",
    "Retrieve response",
    "Retrieve a stored Responses object (tenant-scoped).",
    &["openai", "responses"],
    "Response ID",
    "api-openapi-responses-get-description"
);
submit_route_with_id_i18n!(
    "/v1/responses/{id}",
    "DELETE",
    "Delete response",
    "Delete a stored Responses object (tenant-scoped).",
    &["openai", "responses"],
    "Response ID",
    "api-openapi-responses-delete-description"
);
submit_route_with_id_i18n!(
    "/v1/responses/{id}/cancel",
    "POST",
    "Cancel response",
    "Cancel a background Responses job and return the cancelled response object.",
    &["openai", "responses"],
    "Response ID",
    "api-openapi-responses-cancel-description"
);
submit_gemini_model_action_i18n!(
    "/v1beta/models/{model_action}",
    "POST",
    "Gemini generateContent",
    "Gemini native API (`generateContent`, `streamGenerateContent`, `countTokens`, `embedContent`, `batchEmbedContents`). Requests convert to the unified chat IR and route across vendors; safetySettings/cachedContent/built-in tools route to type_=\"gemini\" vendors.",
    &["gemini"],
    "api-openapi-gemini-generate-description"
);
submit_gemini_model_action_i18n!(
    "/v1beta/models/{model_action}",
    "GET",
    "Gemini model metadata",
    "Return one Gemini model entry from the gateway catalog.",
    &["gemini"],
    "api-openapi-gemini-model-description"
);
submit_route_i18n!(
    "/v1beta/models",
    "GET",
    "List Gemini models",
    "List routable models in Gemini `models[]` shape (abstract_id preferred, deduplicated) with supportedGenerationMethods.",
    &["gemini"],
    "api-openapi-gemini-models-list-description"
);
submit_route_i18n!(
    "/v1/models",
    "GET",
    "OpenAI models list",
    "Union of model catalog and enabled vendor models, deduplicated; vendor entries list their routable public name (abstract_id, else model_id). Supports ?task=chat|embedding|rerank filtering. Never cached.",
    &["openai"],
    "api-openapi-models-list-description"
);
submit_route_i18n!(
    "/v1/messages/count_tokens",
    "POST",
    "Anthropic token count",
    "Approximate input token estimation (chars/4 + per-message overhead, deterministic).",
    &["anthropic"],
    "api-openapi-count-tokens-description"
);
submit_route_i18n!(
    "/admin/route/explain",
    "POST",
    "Route explanation",
    "Read-only per-candidate routing assessment (reason codes + weight breakdown).",
    &["admin"],
    "api-openapi-route-explain-description"
);
submit_route_i18n!(
    "/admin/cost",
    "GET",
    "Cost ledger query",
    "Aggregated token and cost ledger grouped by vendor or model, with optional time window and API key filter.",
    &["admin"],
    "api-openapi-cost-usage-description"
);
// 管理面
submit_route_i18n!(
    "/admin/logs",
    "GET",
    "Query request logs",
    "Paginated request log query with filters.",
    &["admin"],
    "api-openapi-admin-logs-description"
);
submit_route_i18n!(
    "/admin/logs/search",
    "GET",
    "Search request logs",
    "Keyword search over request logs.",
    &["admin"],
    "api-openapi-admin-logs-search-description"
);
submit_route_i18n!(
    "/admin/stats/tokens",
    "GET",
    "Token usage stats",
    "Aggregated token usage statistics.",
    &["admin"],
    "api-openapi-admin-tokens-stats-description"
);
submit_route_i18n!(
    "/admin/sessions",
    "GET",
    "Session aggregates",
    "Session-level aggregates: cost, tokens, request count, first/last timestamps and a title from the first user message. Returns 404 when [usage_insights] is disabled.",
    &["admin"],
    "api-openapi-admin-sessions-description"
);
submit_route_i18n!(
    "/admin/context-stats",
    "GET",
    "Context utilisation stats",
    "Per-model context-window occupancy, cache-read share and five-bucket distribution. Returns 404 when [usage_insights] is disabled.",
    &["admin"],
    "api-openapi-admin-context-stats-description"
);
submit_route_i18n!(
    "/admin/metrics",
    "GET",
    "Operational metrics",
    "Internal operational metrics snapshot (JSON).",
    &["admin"],
    "api-openapi-admin-metrics-description"
);
submit_route_i18n!(
    "/admin/quota/ledger",
    "GET",
    "Quota ledger",
    "Per-window quota usage snapshot (tokens/cost used, reset time, scope: tenant/global/vendor). Optional `key` query filters by tenant digest.",
    &["admin"],
    "api-openapi-admin-quota-ledger-description"
);
submit_route_i18n!(
    "/admin/vendor-budgets",
    "GET",
    "Vendor budgets",
    "Declared vendor budgets with live usage, pace consumption ratio and reset countdown.",
    &["admin"],
    "api-openapi-admin-vendor-budgets-description"
);
submit_route_i18n!(
    "/admin/vendors",
    "GET",
    "List vendors",
    "Vendor status and pool overview.",
    &["admin"],
    "api-openapi-admin-vendors-description"
);
submit_route_i18n!(
    "/admin/vendors/config",
    "GET",
    "List vendor configs",
    "Raw VendorConfig list (config view; complements the status view of /admin/vendors).",
    &["admin"],
    "api-openapi-admin-vendor-configs-description"
);
submit_route_i18n!(
    "/admin/vendors/{id}",
    "PUT",
    "Update vendor",
    "Atomically replace one vendor config by id (validated read-modify-write under the config write lock). Unknown id yields 400.",
    &["admin"],
    "api-openapi-admin-vendor-update-description"
);
submit_route_i18n!(
    "/admin/config",
    "GET",
    "Get runtime config",
    "Current runtime configuration snapshot.",
    &["admin"],
    "api-openapi-admin-config-get-description"
);
submit_route_i18n!(
    "/admin/config",
    "POST",
    "Update runtime config",
    "Hot-update runtime configuration (validated, atomic).",
    &["admin"],
    "api-openapi-admin-config-update-description"
);
submit_route_i18n!(
    "/admin/models",
    "GET",
    "List models",
    "Model catalog listing.",
    &["admin"],
    "api-openapi-admin-models-description"
);
submit_route_with_id_i18n!(
    "/admin/vendors/{id}/status",
    "GET",
    "Vendor status",
    "Per-vendor status detail.",
    &["admin"],
    "Vendor ID",
    "api-openapi-admin-vendor-status-description"
);
// 目录自动同步（catalog-sync feature 门控：路由本体同门控，spec 条目
// 仅在该 feature 下登记，避免文档面出现不可达端点）
#[cfg(feature = "catalog-sync")]
submit_route_i18n!(
    "/admin/catalog/sync",
    "POST",
    "Sync model catalog",
    "Fetch the external model catalog source (add-only merge) into [[model_catalog]]; dry_run returns the diff without persisting.",
    &["admin"],
    "api-openapi-admin-catalog-sync-description"
);
submit_route_with_id_i18n!(
    "/admin/vendors/{id}/reset",
    "POST",
    "Reset vendor",
    "Reset per-vendor breakers and quotas.",
    &["admin"],
    "Vendor ID",
    "api-openapi-admin-vendor-reset-description"
);
submit_route_with_id_i18n!(
    "/admin/models/{id}",
    "POST",
    "Update model entry",
    "Legacy global-strategy update path (kept for compatibility).",
    &["admin"],
    "Model ID",
    "api-openapi-admin-model-update-description"
);
submit_route_with_id_i18n!(
    "/admin/models/{id}",
    "PUT",
    "Upsert model entry",
    "Create or replace a catalog entry.",
    &["admin"],
    "Model ID",
    "api-openapi-admin-model-upsert-description"
);
submit_route_with_id_i18n!(
    "/admin/models/{id}",
    "DELETE",
    "Delete model entry",
    "Remove a catalog entry.",
    &["admin"],
    "Model ID",
    "api-openapi-admin-model-delete-description"
);
// 健康与基础设施
submit_route_i18n!(
    "/api/v1/health",
    "GET",
    "Liveness probe",
    "Forge-registered liveness endpoint.",
    &["health"],
    "api-openapi-health-description"
);
submit_route_i18n!(
    "/readyz",
    "GET",
    "Readiness probe",
    "Aggregated component readiness (DB, cache, vendors).",
    &["health"],
    "api-openapi-readyz-description"
);
submit_route_i18n!(
    "/metrics",
    "GET",
    "Prometheus metrics",
    "RED metrics in Prometheus text exposition format.",
    &["health"],
    "api-openapi-metrics-description"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// 描述翻译键与 inventory 条目对齐，且 en/zh 两档在 sdforge 注册表可解析：
    /// 注册缺失或键名手误时 spec 静默回落编译期英文，翻译不生效——在此钉住。
    #[test]
    fn openapi_descriptions_resolve_for_en_and_zh() {
        register_openapi_i18n();
        // (method, path, key)：同路径多方法的条目按 method 精确匹配
        for (method, path, key) in [
            ("GET", "/v1/models", "api-openapi-models-list-description"),
            (
                "POST",
                "/v1/messages/count_tokens",
                "api-openapi-count-tokens-description",
            ),
            (
                "POST",
                "/admin/route/explain",
                "api-openapi-route-explain-description",
            ),
            ("GET", "/admin/cost", "api-openapi-cost-usage-description"),
            (
                "POST",
                "/v1/chat/completions",
                "api-openapi-chat-completions-description",
            ),
            ("POST", "/v1/messages", "api-openapi-messages-description"),
            (
                "POST",
                "/v1/embeddings",
                "api-openapi-embeddings-description",
            ),
            ("POST", "/v1/rerank", "api-openapi-rerank-description"),
            ("POST", "/v1/responses", "api-openapi-responses-description"),
            (
                "POST",
                "/v1/responses/input_tokens",
                "api-openapi-responses-input-tokens-description",
            ),
            (
                "GET",
                "/v1/responses/{id}",
                "api-openapi-responses-get-description",
            ),
            (
                "DELETE",
                "/v1/responses/{id}",
                "api-openapi-responses-delete-description",
            ),
            (
                "POST",
                "/v1/responses/{id}/cancel",
                "api-openapi-responses-cancel-description",
            ),
            (
                "POST",
                "/v1beta/models/{model_action}",
                "api-openapi-gemini-generate-description",
            ),
            (
                "GET",
                "/v1beta/models/{model_action}",
                "api-openapi-gemini-model-description",
            ),
            (
                "GET",
                "/v1beta/models",
                "api-openapi-gemini-models-list-description",
            ),
            ("GET", "/admin/logs", "api-openapi-admin-logs-description"),
            (
                "GET",
                "/admin/logs/search",
                "api-openapi-admin-logs-search-description",
            ),
            (
                "GET",
                "/admin/stats/tokens",
                "api-openapi-admin-tokens-stats-description",
            ),
            (
                "GET",
                "/admin/sessions",
                "api-openapi-admin-sessions-description",
            ),
            (
                "GET",
                "/admin/context-stats",
                "api-openapi-admin-context-stats-description",
            ),
            (
                "GET",
                "/admin/metrics",
                "api-openapi-admin-metrics-description",
            ),
            (
                "GET",
                "/admin/quota/ledger",
                "api-openapi-admin-quota-ledger-description",
            ),
            (
                "GET",
                "/admin/vendor-budgets",
                "api-openapi-admin-vendor-budgets-description",
            ),
            (
                "GET",
                "/admin/vendors",
                "api-openapi-admin-vendors-description",
            ),
            (
                "GET",
                "/admin/vendors/config",
                "api-openapi-admin-vendor-configs-description",
            ),
            (
                "PUT",
                "/admin/vendors/{id}",
                "api-openapi-admin-vendor-update-description",
            ),
            (
                "GET",
                "/admin/config",
                "api-openapi-admin-config-get-description",
            ),
            (
                "POST",
                "/admin/config",
                "api-openapi-admin-config-update-description",
            ),
            (
                "GET",
                "/admin/models",
                "api-openapi-admin-models-description",
            ),
            (
                "GET",
                "/admin/vendors/{id}/status",
                "api-openapi-admin-vendor-status-description",
            ),
            (
                "POST",
                "/admin/vendors/{id}/reset",
                "api-openapi-admin-vendor-reset-description",
            ),
            (
                "POST",
                "/admin/models/{id}",
                "api-openapi-admin-model-update-description",
            ),
            (
                "PUT",
                "/admin/models/{id}",
                "api-openapi-admin-model-upsert-description",
            ),
            (
                "DELETE",
                "/admin/models/{id}",
                "api-openapi-admin-model-delete-description",
            ),
            ("GET", "/api/v1/health", "api-openapi-health-description"),
            ("GET", "/readyz", "api-openapi-readyz-description"),
            ("GET", "/metrics", "api-openapi-metrics-description"),
        ] {
            let route = sdforge::inventory::iter::<sdforge::openapi::OpenApiRouteInfo>()
                .into_iter()
                .find(|route| route.path == path && route.method == method)
                .unwrap_or_else(|| panic!("{method} {path} 必须登记进 OpenAPI inventory"));
            assert_eq!(
                route.i18n_key,
                Some(key),
                "{method} {path} 的描述翻译键失配"
            );
            for locale in ["en", "zh"] {
                assert_ne!(
                    sdforge::i18n::translate_for(locale, key, &[]),
                    key,
                    "{locale}/{key} 未注册——spec 将回落英文并静默丢失翻译"
                );
            }
        }
        // catalog-sync 条目按 feature 门控登记（关闭时不在 inventory 中）
        #[cfg(feature = "catalog-sync")]
        {
            let key = "api-openapi-admin-catalog-sync-description";
            let route = sdforge::inventory::iter::<sdforge::openapi::OpenApiRouteInfo>()
                .into_iter()
                .find(|route| route.path == "/admin/catalog/sync" && route.method == "POST")
                .expect("catalog-sync feature 下条目不登记");
            assert_eq!(route.i18n_key, Some(key));
            for locale in ["en", "zh"] {
                assert_ne!(sdforge::i18n::translate_for(locale, key, &[]), key);
            }
        }
    }
}
