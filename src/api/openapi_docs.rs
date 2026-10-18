// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAPI 文档面：主 REST 端点经 sdforge inventory 机制注册进全局 spec；
//! 文档路由由 `[docs]`.enabled 开关挂载（默认 false = 404）——开关本身即
//! 访问门（打开后免认证免限流，spec 非敏感）

use std::sync::Once;

/// OpenAPI 版本号（全条目统一；升级大版本时改此处一处）。
const OPENAPI_VERSION: &str = "v1";

/// 提交简单条目（无路径参数）。
macro_rules! submit_route {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo::new(
                $path, $method, $summary, $desc, OPENAPI_VERSION, $tags
            )
        }
    };
}

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

/// 提交带 `{id}` 路径参数的条目。
macro_rules! submit_route_with_id {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr, $id_desc:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo::with_path_params(
                $path,
                $method,
                $summary,
                $desc,
                OPENAPI_VERSION,
                $tags,
                &[sdforge::openapi::OpenApiPathParam::new(
                    "id", $id_desc, true, "string", ""
                )],
            )
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
    });
}

// OpenAI 兼容面
submit_route!(
    "/v1/chat/completions",
    "POST",
    "OpenAI chat completions",
    "Unified chat completions (OpenAI wire format) across vendors.",
    &["openai"]
);
submit_route!(
    "/v1/messages",
    "POST",
    "Anthropic messages",
    "Unified messages API (Anthropic wire format) across vendors.",
    &["anthropic"]
);
submit_route!(
    "/v1/embeddings",
    "POST",
    "OpenAI embeddings",
    "Unified embeddings API across vendors.",
    &["openai"]
);
submit_route!(
    "/v1/rerank",
    "POST",
    "Cohere rerank",
    "Unified rerank API (Cohere wire format) across vendors; non-streaming, documents capped at 1000 per request.",
    &["rerank"]
);
submit_route_i18n!(
    "/v1/models",
    "GET",
    "OpenAI models list",
    "Union of model catalog and enabled vendor models, deduplicated; vendor entries list their routable public name (abstract_id, else model_id). Never cached.",
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
submit_route!(
    "/admin/logs",
    "GET",
    "Query request logs",
    "Paginated request log query with filters.",
    &["admin"]
);
submit_route!(
    "/admin/logs/search",
    "GET",
    "Search request logs",
    "Keyword search over request logs.",
    &["admin"]
);
submit_route!(
    "/admin/stats/tokens",
    "GET",
    "Token usage stats",
    "Aggregated token usage statistics.",
    &["admin"]
);
submit_route!(
    "/admin/metrics",
    "GET",
    "Operational metrics",
    "Internal operational metrics snapshot (JSON).",
    &["admin"]
);
submit_route!(
    "/admin/vendors",
    "GET",
    "List vendors",
    "Vendor status and pool overview.",
    &["admin"]
);
submit_route!(
    "/admin/config",
    "GET",
    "Get runtime config",
    "Current runtime configuration snapshot.",
    &["admin"]
);
submit_route!(
    "/admin/config",
    "POST",
    "Update runtime config",
    "Hot-update runtime configuration (validated, atomic).",
    &["admin"]
);
submit_route!(
    "/admin/models",
    "GET",
    "List models",
    "Model catalog listing.",
    &["admin"]
);
submit_route_with_id!(
    "/admin/vendors/{id}/status",
    "GET",
    "Vendor status",
    "Per-vendor status detail.",
    &["admin"],
    "Vendor ID"
);
submit_route_with_id!(
    "/admin/vendors/{id}/reset",
    "POST",
    "Reset vendor",
    "Reset per-vendor breakers and quotas.",
    &["admin"],
    "Vendor ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "POST",
    "Update model entry",
    "Legacy global-strategy update path (kept for compatibility).",
    &["admin"],
    "Model ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "PUT",
    "Upsert model entry",
    "Create or replace a catalog entry.",
    &["admin"],
    "Model ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "DELETE",
    "Delete model entry",
    "Remove a catalog entry.",
    &["admin"],
    "Model ID"
);
// 健康与基础设施
submit_route!(
    "/api/v1/health",
    "GET",
    "Liveness probe",
    "Forge-registered liveness endpoint.",
    &["health"]
);
submit_route!(
    "/readyz",
    "GET",
    "Readiness probe",
    "Aggregated component readiness (DB, cache, vendors).",
    &["health"]
);
submit_route!(
    "/metrics",
    "GET",
    "Prometheus metrics",
    "RED metrics in Prometheus text exposition format.",
    &["health"]
);

#[cfg(test)]
mod tests {
    use super::*;

    /// 描述翻译键与 inventory 条目对齐，且 en/zh 两档在 sdforge 注册表可解析：
    /// 注册缺失或键名手误时 spec 静默回落编译期英文，翻译不生效——在此钉住。
    #[test]
    fn openapi_descriptions_resolve_for_en_and_zh() {
        register_openapi_i18n();
        for (path, key) in [
            ("/v1/models", "api-openapi-models-list-description"),
            (
                "/v1/messages/count_tokens",
                "api-openapi-count-tokens-description",
            ),
            (
                "/admin/route/explain",
                "api-openapi-route-explain-description",
            ),
            ("/admin/cost", "api-openapi-cost-usage-description"),
        ] {
            let route = sdforge::inventory::iter::<sdforge::openapi::OpenApiRouteInfo>()
                .into_iter()
                .find(|route| route.path == path)
                .unwrap_or_else(|| panic!("{path} 必须登记进 OpenAPI inventory"));
            assert_eq!(route.i18n_key, Some(key), "{path} 的描述翻译键失配");
            for locale in ["en", "zh"] {
                assert_ne!(
                    sdforge::i18n::translate_for(locale, key, &[]),
                    key,
                    "{locale}/{key} 未注册——spec 将回落英文并静默丢失翻译"
                );
            }
        }
    }
}
