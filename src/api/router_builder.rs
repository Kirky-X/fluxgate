// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use axum::{
    extract::{Request, State},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use limiteron::config::{
    Action, ActionConfig, FlowControlConfig, GlobalConfig, LimiterConfig, Matcher, Rule,
};
use limiteron::limiters::{Limiter, ShardedSlidingWindowLimiter};
use limiteron::storage::{MemoryBanStorage, MemoryStorage};
use limiteron::{Decision, Governor, RequestContext};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower_http::trace::TraceLayer;

use super::response_cache::cache_api_key_hash;
use super::state::ApiState;
use super::{admin, anthropic, health, openai};
use crate::config::{RateLimitRule, RateLimitSection};
use crate::i18n::t;
use crate::middleware::admission::{tier_for_request, AdmissionState};
use crate::middleware::auth_middleware;

/// recId=3：入站限流中间件状态 — 专用 struct 替代此前裸 `Arc<Governor>`
/// State 形态，承载 Governor（全局规则）与中间件侧 per-endpoint 限流表
/// 两套判定件；后续准入扩展（recId=16）在其上追加字段。
pub struct InboundLimitState {
    /// limiteron Governor — 全局规则（path 缺省）的判定层；rules 为空时
    /// 即库内置默认兜底规则（与历史行为逐位一致）
    governor: Arc<Governor>,
    /// per-endpoint 限流表（path 前缀规则；recId=16 在此挂载准入维度）
    endpoint_table: EndpointLimitTable,
    /// recId=16：拥塞感知优先级准入状态 — 判定在限流之后、handler 之前
    ///（[admission].enabled 热读；默认 false 时判定旁路、行为与现状
    /// 逐位一致）。健康检查白名单（函数头部）先于本判定，永不卸载。
    admission: Arc<AdmissionState>,
}

/// recId=3：per-endpoint 限流表 — Matcher 六变体无 path 维度
///（limiteron src/config/types/rule.rs），端点自筛在中间件内按 path 前缀
/// 完成；每 `(rule, tenant)` 一只 limiter 惰性创建（limiteron 原语直接
/// 实例化，复用 vendor/rate_limiter.rs 的 wrapper 模式）。
/// 租户数受 auth.api_keys 枚举约束（有界），匿名租户单条；[auth] 缺失或
/// enabled=false 时中间件放行任意伪造 key（auth.rs:150-152），租户键退化为
/// sha256(任意串)——由 [`MAX_ENDPOINT_LIMITERS`] 容量守卫兜底（评审 major：
/// 条目只增不删会构成内存放大 DoS 向量，与同源 cache_api_key_hash 的响应
/// 缓存不同，本表无 TTL 驱逐，必须硬上限）。
struct EndpointLimitTable {
    /// path 前缀规则（构造期从 [rate_limit] 段筛选，restart-required）
    rules: Vec<EndpointRuleSpec>,
    /// 惰性 limiter 池：键 `rule_id \0 tenant`（per_tenant=false 时 tenant
    /// 恒为空串，即规则级单桶）。std Mutex + HashMap：条目数有硬上限
    ///（见 [`MAX_ENDPOINT_LIMITERS`]），锁竞争与 DashMap 无异。
    limiters: Mutex<HashMap<String, Arc<ShardedSlidingWindowLimiter>>>,
    /// 条目上限（生产取 [`MAX_ENDPOINT_LIMITERS`]；测试注入小值驱动溢出）
    max_entries: usize,
}

/// 端点表条目硬上限。auth 启用时正常租户数 = api_keys 枚举规模（个位数
/// 到百级），远低于此值；auth 关闭时达到上限后新租户退化到规则级
/// `__overflow__` 单桶——内存有界（≤ 上限 + per_tenant 规则数条溢出桶），
/// 后续新租户共享限流（保守语义）。
const MAX_ENDPOINT_LIMITERS: usize = 1024;

/// 溢出退化桶的租户键后缀
const OVERFLOW_TENANT: &str = "__overflow__";

struct EndpointRuleSpec {
    rule: RateLimitRule,
}

impl EndpointLimitTable {
    fn build(section: &RateLimitSection) -> Self {
        let rules = section
            .rules
            .iter()
            .filter(|r| r.path.is_some())
            .cloned()
            .map(|rule| EndpointRuleSpec { rule })
            .collect();
        Self {
            rules,
            limiters: Mutex::new(HashMap::new()),
            max_entries: MAX_ENDPOINT_LIMITERS,
        }
    }

    /// 命中请求 path 的规则（前缀匹配）
    fn matching(&self, path: &str) -> Vec<&EndpointRuleSpec> {
        self.rules
            .iter()
            .filter(|spec| {
                spec.rule
                    .path
                    .as_deref()
                    .is_some_and(|prefix| path.starts_with(prefix))
            })
            .collect()
    }

    /// 取（或惰性创建）`(rule, tenant)` 的滑窗 limiter。
    /// window_secs=0 按 1s 兜底（ShardedSlidingWindowLimiter 内部同样
    /// `max(1)`，此处显式化避免零窗口语义歧义）。
    ///
    /// 容量守卫（评审 major）：条目达到 `max_entries` 后不再为新租户建桶，
    /// 退化到规则级 `__overflow__` 单桶（该键每规则至多一条，一旦存在恒
    /// 复用）。全同步无 await 点，临界区内构造（O(1) 轻量）。
    fn limiter_for(
        &self,
        spec: &EndpointRuleSpec,
        tenant: &str,
    ) -> Arc<ShardedSlidingWindowLimiter> {
        let mut guard = self.limiters.lock().unwrap();

        let key = if spec.rule.per_tenant {
            format!("{}\u{0}{}", spec.rule.id, tenant)
        } else {
            spec.rule.id.clone()
        };
        if let Some(limiter) = guard.get(&key) {
            return limiter.clone();
        }

        let insert_key = if spec.rule.per_tenant && guard.len() >= self.max_entries {
            format!("{}\u{0}{}", spec.rule.id, OVERFLOW_TENANT)
        } else {
            key
        };
        if let Some(limiter) = guard.get(&insert_key) {
            return limiter.clone();
        }

        let limiter = Arc::new(ShardedSlidingWindowLimiter::new(
            Duration::from_secs(spec.rule.window_secs.max(1)),
            spec.rule.max_requests,
        ));
        guard.insert(insert_key, limiter.clone());
        limiter
    }
}

/// 限流中间件 — 基于 limiteron::Governor::check（全局规则）+ 中间件侧
/// per-endpoint 限流表（recId=3）。
///
/// 健康检查路径放行（/api/v1/health 为 #[forge(version="v1")] 的实际注册
/// 路径，见 src/api/health.rs；src/middleware/auth.rs 的认证白名单是另一
/// 份独立列表，两处语义各自独立维护）；其余请求先查端点表（超限 429 +
/// Retry-After），再按 Governor 决策返回 429/403 或放行。
async fn rate_limit_middleware(
    State(state): State<Arc<InboundLimitState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    // 健康检查放行（/api/v1/health 为 #[forge(version="v1")] 的实际注册路径）
    // —— recId=16：准入判定在本白名单之后，健康检查永不卸载。
    if matches!(
        path.as_str(),
        "/health" | "/healthz" | "/ready" | "/api/v1/health"
    ) {
        return next.run(request).await;
    }
    // recId=16：准入状态与热配置句柄（[admission].enabled 热读判定在
    // admit 内部；此处只借出引用）
    let admission = state.admission.clone();
    let admission_config = admission.config_handle();

    // recId=3：租户 id = 已验证 API key 的 SHA256 摘要（本中间件在 auth
    // 之后运行——axum layer 顺序 auth 外层；沿用 response_cache 先例：
    // 无 key 请求归入固定 anonymous 租户）
    let tenant = cache_api_key_hash(request.headers());

    // per-endpoint 规则（path 前缀自筛；任一规则拒绝即 429）
    for spec in state.endpoint_table.matching(&path) {
        let tenant_key = if spec.rule.per_tenant {
            tenant.as_str()
        } else {
            ""
        };
        let limiter = state.endpoint_table.limiter_for(spec, tenant_key);
        match limiter.allow(1).await {
            Ok(true) => {}
            Ok(false) => {
                // Retry-After 取表内 limiter 的重置秒数（limiteron
                // RateLimitSnapshot::reset_secs）
                let reset_secs = limiter
                    .remaining()
                    .await
                    .map(|s| s.reset_secs.max(1))
                    .unwrap_or(1);
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("Retry-After", reset_secs.to_string())],
                    t("router-rate-limit-exceeded"),
                )
                    .into_response();
            }
            Err(_) => {} // 限流器故障放行（fail-open，对齐 Governor Err(_) 分支）
        }
    }

    // 全局规则层（limiteron Governor）：recId=3 注入 X-User-Id = 租户摘要，
    // 使 Matcher::User 规则可感知租户维度（limiteron src/matchers/engine.rs
    // 的 MatchCondition::User 只认 X-User-Id 头或 "*"）
    let context = RequestContext::new()
        .with_path(&path)
        .with_method(request.method().as_str())
        .with_header("X-User-Id", &tenant);

    match state.governor.check(&context).await {
        Ok(Decision::Allowed(_)) | Err(_) => {
            // recId=16：拥塞感知优先级准入（热读 [admission].enabled，
            // 默认 false = 判定旁路、行为与现状逐位一致）。请求档位经
            // [auth].keys 表（认证已在本中间件外层通过）。
            let tier = tier_for_request(admission_config.as_ref(), request.headers());
            match admission.admit(tier, &tenant, now_unix_ms()) {
                Ok(guard) => {
                    // guard 生命周期 = 中间件作用域（包住 handler 执行至
                    // 响应返回）；流式 body 传输期不计（粗分类定位，见
                    // admission.rs 模块注释）。
                    let response = next.run(request).await;
                    drop(guard);
                    response
                }
                Err(retry_after) => (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("Retry-After", retry_after.to_string())],
                    t("router-admission-rejected"),
                )
                    .into_response(),
            }
        }
        Ok(Decision::Rejected(_)) => (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            t("router-rate-limit-exceeded"),
        )
            .into_response(),
        Ok(Decision::Banned(_)) => {
            (axum::http::StatusCode::FORBIDDEN, t("router-access-denied")).into_response()
        }
    }
}

/// recId=16：当前 UNIX 毫秒（burst EWMA 的时间锚；Record 注入语义见
/// admission.rs）
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// recId=3：由 [rate_limit] 段构建入站限流判定件（Governor + 端点表）。
///
/// - rules 为空 → `Governor::new()`（库内置默认兜底规则）+ 空端点表，
///   与历史行为逐位一致（结构性断言见 config_toml_example_test）；
/// - path 缺省的全局规则 → 转 limiteron `Rule`（Matcher::User + 滑窗）
///   构造期注入 Governor。limiteron 决策链为规则级单例桶
///   （`DecisionChain::check` 无 context 参数），per_tenant 在该层不生效；
/// - path 前缀规则 → 中间件端点表（per-tenant 分桶在表内实现）；
/// - 全部规则均带 path 时 Governor 保持默认兜底（FlowControlConfig 校验
///   要求规则非空，limiteron src/config/types/types_impl.rs）。
async fn build_inbound_limits(
    section: &RateLimitSection,
) -> Result<(Arc<Governor>, EndpointLimitTable), limiteron::LimiteronError> {
    let endpoint_table = EndpointLimitTable::build(section);
    let global_rules: Vec<&RateLimitRule> =
        section.rules.iter().filter(|r| r.path.is_none()).collect();

    if global_rules.is_empty() {
        // limiteron 0.3 rc.5 起 `Governor::new()` 返回 `Result`（旧版内部
        // expect、返回 Self）。保持旧的失败即 panic 行为，unwrap_or_else 内
        // panic 解包（翻译只在失败路径求值，clippy expect_fun_call）。
        return Ok((
            Arc::new(
                Governor::new()
                    .await
                    .unwrap_or_else(|e| panic!("{}: {e:?}", t("router-governor-init-failed"))),
            ),
            endpoint_table,
        ));
    }

    let rules = global_rules
        .into_iter()
        .map(|r| Rule {
            id: r.id.clone(),
            name: format!("nexusflow rate_limit rule {}", r.id),
            priority: 100,
            matchers: vec![Matcher::User {
                user_ids: vec!["*".to_string()],
            }],
            limiters: vec![LimiterConfig::SlidingWindow {
                window_size: format!("{}s", r.window_secs.max(1)),
                max_requests: r.max_requests,
            }],
            action: ActionConfig {
                on_exceed: Action::Reject,
                ban: None,
            },
        })
        .collect();

    let flow = FlowControlConfig {
        // 构造期一次性注入（restart-required），version 取固定标记
        version: "nexusflow-inbound-1".to_string(),
        global: GlobalConfig::default(),
        rules,
    };
    let governor = Governor::builder()
        .with_config(flow)
        .with_storage(Arc::new(MemoryStorage::new()))
        .with_ban_storage(Arc::new(MemoryBanStorage::new()))
        .build()
        .await?;
    Ok((Arc::new(governor), endpoint_table))
}

/// 创建 axum 路由，集成 limiteron 限流中间件与 API Key 认证中间件
///
/// 层次（外→内）：Trace → 认证 → 限流 → handler。
/// 认证中间件的 state 是 `Arc<ConfigManager>`：每请求从热配置现读 [auth] 段
/// （SHA256 摘要存储），key 吊销/轮换与 enabled 开关热更新立即生效；
/// 段缺失时 `enabled=false` 放行（旧配置兼容）。
/// 限流中间件的 state 是 `Arc<InboundLimitState>`（recId=3）：Governor 与
/// per-endpoint 限流表在 Router 构造期从 [rate_limit] 段构建
///（restart-required，热下发列二期）。
/// 注意：健康检查由 sdforge `#[forge(version="v1")]` 注册为 /api/v1/health
/// （api/health.rs:10，宏加版本前缀），此处不重复注册，避免 merge 时
/// axum duplicate-route panic。
pub async fn create_router(state: Arc<ApiState>) -> Router {
    // forge 端点描述翻译注册进 sdforge（建站期一次性；见 health.rs）
    health::register_forge_i18n();
    // recId=3：[rate_limit] 段消费点（此前该段被 serde 静默丢弃，中间件
    // 恒跑 Governor 库内置默认兜底规则）
    let rate_limit = state.config.read().rate_limit.clone();
    let (governor, endpoint_table) = build_inbound_limits(&rate_limit)
        .await
        .unwrap_or_else(|e| panic!("{}: {e:?}", t("router-inbound-rate-limit-init-failed")));
    // recId=16：准入状态常驻构造（轻量；[admission].enabled 默认 false
    // 时判定旁路——热开开关立即生效，无需重启）
    let admission = Arc::new(AdmissionState::new(
        state.config.clone(),
        state.vendor_manager.clone(),
    ));
    let limit_state = Arc::new(InboundLimitState {
        governor,
        endpoint_table,
        admission,
    });

    Router::new()
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/embeddings", post(openai::embeddings))
        .route("/v1/messages", post(anthropic::messages))
        .route("/admin/logs", get(admin::logs))
        .route("/admin/logs/search", get(admin::search_logs))
        .route("/admin/stats/tokens", get(admin::token_stats))
        .route("/admin/metrics", get(admin::metrics))
        .route("/admin/vendors", get(admin::vendors))
        .route("/admin/vendors/{id}/status", get(admin::vendor_status))
        .route("/admin/vendors/{id}/reset", post(admin::reset_vendor))
        .route(
            "/admin/config",
            get(admin::get_config).post(admin::update_config),
        )
        .route("/admin/models", get(admin::get_models))
        // T006：目录 CRUD —— POST {id} 保留 global 策略更新（存量契约），
        // PUT/DELETE 承载目录条目 upsert/下条
        .route(
            "/admin/models/{id}",
            post(admin::update_model)
                .put(admin::upsert_model)
                .delete(admin::delete_model),
        )
        .with_state(state.clone())
        .layer(from_fn_with_state(limit_state, rate_limit_middleware))
        .layer(from_fn_with_state(state.config.clone(), auth_middleware))
        .layer(TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get as get_route;
    use tower::ServiceExt;

    /// 最小 Config（白名单/端点表单测；admission 默认关闭 = 判定旁路）
    fn min_config() -> crate::config::Config {
        crate::config::Config {
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            resilience: Default::default(),
            docs: Default::default(),
            admission: Default::default(),
            grpc: None,
        }
    }

    fn endpoint_rule(id: &str, path: &str, max_requests: u64) -> RateLimitRule {
        RateLimitRule {
            id: id.into(),
            path: Some(path.into()),
            per_tenant: true,
            window_secs: 60,
            max_requests,
        }
    }

    /// 健康检查白名单（评审 major 修复锁定）。
    ///
    /// 断言空洞背景：axum 0.8 的 `layer` 只作用于已有路由
    ///（axum-0.8.9 src/docs/routing/layer.md:6 "the middleware is only
    /// applied to existing routes"），评审探针实证 create_router 之后
    /// 再 `.route()` 的路径不经过 rate_limit_middleware——此前
    /// tests/rate_limit_test.rs 的集成形态无论白名单代码存在与否都会过。
    /// 本用例 route 先于 layer 构建：`/health` 白名单豁免断言 +
    /// `/limited` 对照组（第 2 次 429）共同证明中间件真实挂载且生效。
    #[tokio::test]
    async fn health_check_bypasses_endpoint_rate_limit() {
        // 规则刻意覆盖 /health 前缀：白名单若失效，第 2 次 GET /health
        // 会进入端点表（max_requests=1）被 429
        let section = RateLimitSection {
            rules: vec![
                endpoint_rule("health-cap", "/health", 1),
                endpoint_rule("limited-cap", "/limited", 1),
            ],
        };
        let (governor, endpoint_table) = build_inbound_limits(&section)
            .await
            .expect("inbound limits");
        let state = Arc::new(InboundLimitState {
            governor,
            endpoint_table,
            admission: {
                // 白名单单测不需要准入状态：空 manager 接线（enabled 默认
                // false，判定旁路）
                let cm = Arc::new(crate::config::ConfigManager::new(min_config()));
                let vm = Arc::new(crate::vendor::VendorManager::new(cm.clone()));
                Arc::new(AdmissionState::new(cm, vm))
            },
        });

        // 关键顺序：route 在 layer 之前 → 中间件作用于全部路由
        let app = Router::new()
            .route("/health", get_route(|| async { "OK" }))
            .route("/limited", get_route(|| async { "OK" }))
            .layer(from_fn_with_state(state, rate_limit_middleware));

        for i in 0..2 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "health check request {i} must bypass the endpoint rule (whitelist)"
            );
        }

        // 对照组：非白名单路径真实经过中间件——排除"中间件根本没挂上"
        // 导致的 /health 假阳性
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/limited")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/limited")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "control path must actually pass through rate_limit_middleware"
        );
    }

    /// 评审 major 修复锁定：端点表条目硬上限——达到 max_entries 后新租户
    /// 退化到规则级 `__overflow__` 单桶（[auth] 缺失/enabled=false 时伪造
    /// Bearer key 不再无限建桶，内存有界）。
    #[tokio::test]
    async fn endpoint_table_caps_entries_and_overflows_to_shared_bucket() {
        let section = RateLimitSection {
            rules: vec![endpoint_rule("cap", "/v1/api", 100)],
        };
        let (_, mut table) = build_inbound_limits(&section)
            .await
            .expect("inbound limits");
        table.max_entries = 2; // 测试注入小上限驱动溢出

        let spec = table.matching("/v1/api/chat").remove(0);
        // 前两个租户各自独立桶
        let t1 = table.limiter_for(spec, "tenant-1");
        let t2 = table.limiter_for(spec, "tenant-2");
        assert!(
            !Arc::ptr_eq(&t1, &t2),
            "distinct tenants must get distinct buckets"
        );
        // 达到上限：t3/t4 退化共享 overflow 桶
        let t3 = table.limiter_for(spec, "tenant-3");
        let t4 = table.limiter_for(spec, "tenant-4");
        assert!(
            Arc::ptr_eq(&t3, &t4),
            "overflow tenants must share the single __overflow__ bucket"
        );
        assert!(
            !Arc::ptr_eq(&t3, &t1) && !Arc::ptr_eq(&t3, &t2),
            "overflow bucket must not alias an existing tenant bucket"
        );
        // 既有租户桶不受溢出影响
        let t1_again = table.limiter_for(spec, "tenant-1");
        assert!(
            Arc::ptr_eq(&t1, &t1_again),
            "existing tenant bucket must be preserved after overflow"
        );
    }
}
