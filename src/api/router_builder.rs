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
use limiteron::limiters::{Limiter, QuotaLimiter, ShardedSlidingWindowLimiter, TokenBucketLimiter};
use limiteron::middleware::{KeyedRateLimitLayer, RejectResponder, RequestKey};
use limiteron::quota::{QuotaConfig, QuotaType};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tower_http::trace::TraceLayer;

use super::response_cache::cache_api_key_hash;
use super::state::ApiState;
use super::{admin, anthropic, health, openai};
use crate::config::{RateLimitRule, RateLimitSection};
use crate::i18n::t;
use crate::middleware::admission::{tier_for_request, AdmissionState};
use crate::middleware::auth_middleware;

/// recId=3 + G5：入站限流中间件状态 — per-endpoint 限流表与准入状态。
/// 全局规则不再经常量 Governor 判定，改由 tower 层
///（[`GlobalRateLimitLayer`]，`create_router` 装配）承担。
pub struct InboundLimitState {
    /// per-endpoint 限流表（path 前缀规则；recId=16 在此挂载准入维度）
    endpoint_table: EndpointLimitTable,
    /// recId=16：拥塞感知优先级准入状态 — 判定在限流之后、handler 之前
    ///（[admission].enabled 热读；默认 false 时判定旁路、行为与现状
    /// 逐位一致）。健康检查白名单（函数头部）先于本判定，永不卸载。
    admission: Arc<AdmissionState>,
}

/// 配额豁免路径（tower 键提取器与端点中间件共用同一判定，行为单点一致）。
/// 健康探针（/health、/healthz、/ready、/api/v1/health、/readyz）、指标抓取
///（/metrics）与文档面（/api-docs/*、/swagger-ui/*）不消耗限流配额。
/// 注意：/health、/healthz、/ready 无对应注册路由（历史遗留探针路径），
/// 保留豁免使误配探针不烧配额（404 本就不计费）。
fn is_quota_exempt_path(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/healthz" | "/ready" | "/api/v1/health" | "/readyz" | "/metrics"
    ) || path.starts_with("/api-docs/")
        || path == "/api-docs"
        || path.starts_with("/swagger-ui/")
        || path == "/swagger-ui"
}

/// G5/G9：请求级租户摘要——端点中间件（最外层自有中间件）计算一次，
/// 经 extensions 传递，tower 键提取器与准入中间件复用（每请求 1 次
/// SHA256；G3 复核性能 H1）。直连 tower/准入层的单测无 extension 时
/// 回退现场计算，行为一致。
#[derive(Debug, Clone)]
pub(crate) struct TenantDigest(pub String);

/// G5：租户摘要键提取器 — 键 = 已验证 API key 的 SHA256 摘要
///（`cache_api_key_hash`，无 key 归入固定 anonymous 桶；关闭 auth 时
/// 伪造 key 的摘要各异，高基数由 QuotaLimiter 1 万 key 上限约束）。
/// 健康白名单路径返回 `None`（tower 层直通不限流）。
/// `per_tenant=false` 的规则用固定空串键（规则级共享桶，旧 Governor
/// 语义；否则恒按租户分桶）。
#[derive(Debug, Clone)]
struct TenantKeyExtractor {
    per_tenant: bool,
}

impl<B> RequestKey<B> for TenantKeyExtractor {
    fn request_key<'a>(&self, request: &'a axum::http::Request<B>) -> Option<Cow<'a, str>> {
        if is_quota_exempt_path(request.uri().path()) {
            return None;
        }
        if !self.per_tenant {
            return Some(Cow::Borrowed(""));
        }
        // 端点中间件已算过摘要（extensions 零分配复用）；直连 tower 层的
        // 单测无 extension，回退现场计算。
        if let Some(digest) = request.extensions().get::<TenantDigest>() {
            return Some(Cow::Borrowed(digest.0.as_str()));
        }
        Some(Cow::Owned(cache_api_key_hash(request.headers())))
    }
}

/// G5：全局规则门 — per-tenant 配额（QuotaLimiter 固定窗口）或空规则
/// 默认兜底（TokenBucket 100/10，与旧 `Governor::new()` 默认逐位一致，
/// 共享桶）。拒绝时把窗口剩余秒记入 `last_retry_after_secs` 供响应器渲染。
/// 内层限流器经 `Arc` 共享，使门整体 `Clone`（axum `Router::layer` 要求）。
#[derive(Clone)]
enum GateLimiter {
    Quota(Arc<QuotaLimiter>),
    TokenBucket(Arc<TokenBucketLimiter>),
}

#[derive(Clone)]
struct GlobalGate {
    inner: GateLimiter,
    /// 拒绝秒数槽（G3 复核 LOW S-L9：多租户并发拒绝下存在秒级错位，
    /// 量级 ≤ 窗口且仅影响等待时长，无正确性影响，故接受共享槽）。
    last_retry_after_secs: Arc<AtomicU64>,
    /// 拒绝时回传的 Retry-After 回退值（配额门法取规则窗口；令牌桶取 1，
    /// 精确值优先读剩余额度快照）。fail-open 统一后后端故障不再走到此处。
    deny_retry_after_secs: u64,
}

#[async_trait::async_trait]
impl Limiter for GlobalGate {
    async fn allow(&self, cost: u64) -> Result<bool, limiteron::LimiteronError> {
        match &self.inner {
            GateLimiter::Quota(q) => q.allow(cost).await,
            GateLimiter::TokenBucket(t) => t.allow(cost).await,
        }
    }

    /// 失败语义统一 fail-open（G3 复核 HIGH）：后端故障放行（与端点表
    /// `Err` 分支、旧 Governor `Err` 分支一致）；仅真实配额耗尽拒绝。
    /// Quota 拒绝回保守整窗秒数（避免二次 `check_retry_after` 重复扣费）。
    async fn check(&self, key: &str) -> Result<(), limiteron::LimiteronError> {
        match &self.inner {
            GateLimiter::Quota(q) => match q.check(key).await {
                Ok(()) => Ok(()),
                Err(limiteron::LimiteronError::QuotaExceeded(_)) => {
                    self.last_retry_after_secs
                        .store(self.deny_retry_after_secs.max(1), Ordering::Relaxed);
                    Err(limiteron::LimiteronError::LimitError(
                        "global rate limit exceeded".to_string(),
                    ))
                }
                Err(_) => Ok(()),
            },
            GateLimiter::TokenBucket(t) => match t.allow(1).await {
                Ok(true) => Ok(()),
                Ok(false) => {
                    let secs = t
                        .remaining()
                        .await
                        .ok()
                        .map(|s| s.reset_secs.max(1))
                        .unwrap_or(self.deny_retry_after_secs.max(1));
                    self.last_retry_after_secs.store(secs, Ordering::Relaxed);
                    Err(limiteron::LimiteronError::LimitError(
                        "global rate limit exceeded".to_string(),
                    ))
                }
                Err(_) => Ok(()),
            },
        }
    }
}

/// G5：全局层拒绝响应 — 429 + `Retry-After: 窗口剩余秒` + 体
/// `t("router-rate-limit-exceeded")`，与 per-endpoint 429 同形（旧全局
/// Governor 拒绝无 Retry-After 头，此处为变更点，见任务验收）。
#[derive(Debug, Clone)]
struct GlobalRejectResponder {
    retry_after_secs: Arc<AtomicU64>,
}

impl RejectResponder<axum::body::Body> for GlobalRejectResponder {
    fn reject_response(
        &self,
        _info: limiteron::middleware::RejectInfo,
    ) -> axum::response::Response<axum::body::Body> {
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(
                "Retry-After",
                self.retry_after_secs
                    .load(Ordering::Relaxed)
                    .max(1)
                    .to_string(),
            )],
            t("router-rate-limit-exceeded"),
        )
            .into_response()
    }
}

/// tower 快速路径的层与 service 均未实现 `Clone`，且 service 的
/// `Error = BoxError`——axum `Router::layer` 要求 `Layer: Clone`、
/// `Service: Clone` 且 `Error: Into<Infallible>`。
/// 本适配层持有全部构造部件（`Arc` + 无状态提取器/响应器，均为 `Clone`），
/// 按请求组装上游 service 并驱动：拒绝响应仍由库 `RejectResponder` 生成，
/// 内层错误转 500；不含任何限流判定逻辑。
#[derive(Clone)]
struct GlobalTowerLayer {
    gate: Arc<GlobalGate>,
    extractor: TenantKeyExtractor,
    responder: GlobalRejectResponder,
}

#[derive(Clone)]
struct GlobalTowerService<S> {
    gate: Arc<GlobalGate>,
    extractor: TenantKeyExtractor,
    responder: GlobalRejectResponder,
    inner: S,
}

impl<S> tower::Layer<S> for GlobalTowerLayer {
    type Service = GlobalTowerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GlobalTowerService {
            gate: self.gate.clone(),
            extractor: self.extractor.clone(),
            responder: self.responder.clone(),
            inner,
        }
    }
}

impl<S, ReqBody> tower::Service<axum::http::Request<ReqBody>> for GlobalTowerService<S>
where
    S: tower::Service<
            axum::http::Request<ReqBody>,
            Response = axum::response::Response<axum::body::Body>,
        > + Clone
        + Send
        + 'static,
    S::Error: Into<tower::BoxError> + Send,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = axum::response::Response<axum::body::Body>;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // 内层 Route 的就绪永不失败；错误吞为就绪（真故障在 call 期转 500）。
        match self.inner.poll_ready(cx) {
            std::task::Poll::Ready(_) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn call(&mut self, req: axum::http::Request<ReqBody>) -> Self::Future {
        use tower::Layer as _;
        let keyed = KeyedRateLimitLayer::with_responder(
            self.gate.clone(),
            self.extractor.clone(),
            self.responder.clone(),
        );
        let mut keyed_service = keyed.layer(self.inner.clone());
        Box::pin(async move {
            match keyed_service.call(req).await {
                Ok(resp) => Ok(resp),
                // G3 复核 LOW S-L10：内层错误原文不进响应体（信息外泄），
                // 只记 warn，调用方收通用 500（内层为 Route 时不可达）。
                Err(err) => {
                    tracing::warn!("global rate limit layer error: {err}");
                    Ok((
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "internal server error",
                    )
                        .into_response())
                }
            }
        })
    }
}

/// G5：全局 tower 限流层（同构叠放，一规则一层）。
type GlobalRateLimitLayer = GlobalTowerLayer;

/// 构造单条全局 tower 层（配额门法与拒绝响应器共享同一秒数槽）。
fn global_rate_limit_layer(
    gate: GlobalGate,
    slot: Arc<AtomicU64>,
    per_tenant: bool,
) -> GlobalRateLimitLayer {
    GlobalTowerLayer {
        gate: Arc::new(gate),
        extractor: TenantKeyExtractor { per_tenant },
        responder: GlobalRejectResponder {
            retry_after_secs: slot,
        },
    }
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
        // G3 复核 LOW S-L11：锁毒化不 panic（取锁内值继续，内存态仍一致）。
        let mut guard = self
            .limiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

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

/// 端点限流中间件（G5 拆分前半）：健康白名单 → per-endpoint 规则表。
/// 全局规则与准入判定不在此（tower 层与 `admission_middleware` 在下游）。
async fn endpoint_limit_middleware(
    State(state): State<Arc<InboundLimitState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    // 健康检查放行（/api/v1/health 为 #[forge(version="v1")] 的实际注册路径）
    if is_quota_exempt_path(&path) {
        return next.run(request).await;
    }

    // recId=3：租户 id = 已验证 API key 的 SHA256 摘要（本中间件在 auth
    // 之后运行——axum layer 顺序 auth 外层；沿用 response_cache 先例：
    // 无 key 请求归入固定 anonymous 租户）。存入 extensions 供下游
    // tower 层与准入中间件复用（G3 复核性能 H1：每请求只算一次）。
    let tenant = cache_api_key_hash(request.headers());
    request
        .extensions_mut()
        .insert(TenantDigest(tenant.clone()));

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
            Err(_) => {} // 限流器故障放行（fail-open）
        }
    }

    next.run(request).await
}

/// 准入中间件（G5 拆分后半）：拥塞感知优先级准入，guard 生命周期包住
/// handler 执行至响应返回；流式 body 传输期不计。
async fn admission_middleware(
    State(state): State<Arc<InboundLimitState>>,
    request: Request,
    next: Next,
) -> Response {
    // recId=16：准入状态与热配置句柄（[admission].enabled 热读判定在
    // admit 内部；此处只借出引用）
    let admission = state.admission.clone();
    let admission_config = admission.config_handle();

    // recId=3：租户 id 与端点中间件同源（extensions 复用，无则回退计算；
    // G3 复核性能 H1）。
    let tenant: std::borrow::Cow<'_, str> = match request.extensions().get::<TenantDigest>() {
        Some(digest) => std::borrow::Cow::Borrowed(digest.0.as_str()),
        None => std::borrow::Cow::Owned(cache_api_key_hash(request.headers())),
    };

    // recId=16：拥塞感知优先级准入（热读 [admission].enabled，
    // 默认 false = 判定旁路、行为与现状逐位一致）。请求档位经
    // [auth].keys 表（认证已在本中间件外层通过）。
    let tier = tier_for_request(admission_config.as_ref(), request.headers());
    // G7：RED 记录（sdforge 全局 registry，供 /metrics 抓取；与内部
    // GlobalMetrics 并存不迁移）。路由标签走静态模板表归一（通配段不
    // 爆炸基数）；基础设施自观察（/metrics、/readyz）不计入。
    // G3 复核性能 M1：标签与方法零分配（&'static / 小串克隆一次），span
    // 延迟到准入通过后创建（拒绝路径不支付）。
    let method = request.method().as_str().to_string();
    let route_label: &'static str = metrics_route_label(request.uri().path());
    let record_metrics = request.uri().path() != "/metrics" && request.uri().path() != "/readyz";
    let started = std::time::Instant::now();
    match admission.admit(tier, &tenant, now_unix_ms()) {
        Ok(guard) => {
            // G9 请求追踪：sdforge context 中间件（最外层）已把 X-Request-Id
            // 注入请求头（缺失则生成）并挂 task_local；此处再把 id 绑到
            // tracing span，同请求日志经 span 关联 request_id（中间件层注入，
            // 不改 inklog 配置）。
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            let span = tracing::info_span!("request", request_id = %request_id);
            use tracing::Instrument as _;
            let response = next.run(request).instrument(span).await;
            drop(guard);
            if record_metrics {
                sdforge::metrics::record_request(
                    route_label,
                    &method,
                    response.status().as_u16(),
                    started.elapsed().as_secs_f64(),
                );
            }
            response
        }
        Err(retry_after) => {
            if record_metrics {
                sdforge::metrics::record_request(
                    route_label,
                    &method,
                    axum::http::StatusCode::TOO_MANY_REQUESTS.as_u16(),
                    started.elapsed().as_secs_f64(),
                );
            }
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", retry_after.to_string())],
                t("router-admission-rejected"),
            )
                .into_response()
        }
    }
}

/// G7：RED 路由标签归一——已知路由模板精确匹配，`{id}` 通配段逐段比对，
/// 未知路径归 `unmatched`（基数有界）。
/// 返回均为 `'static`（模板表/字面量），调用方零分配。
fn metrics_route_label(path: &str) -> &'static str {
    const TEMPLATES: [&str; 14] = [
        "/v1/chat/completions",
        "/v1/embeddings",
        "/v1/messages",
        "/admin/logs",
        "/admin/logs/search",
        "/admin/stats/tokens",
        "/admin/metrics",
        "/admin/vendors",
        "/admin/vendors/{id}/status",
        "/admin/vendors/{id}/reset",
        "/admin/config",
        "/admin/models",
        "/admin/models/{id}",
        "/api/v1/health",
    ];
    // 注：/readyz、/metrics 不在模板表——RED 记录跳过二者（基础设施自观察
    // 不计入），未知路径归 unmatched（见函数尾）。
    if let Some(exact) = TEMPLATES
        .iter()
        .find(|t| !t.contains("{id}") && **t == path)
    {
        return exact;
    }
    let path_segs: Vec<&str> = path.split('/').collect();
    for template in TEMPLATES.iter().filter(|t| t.contains("{id}")) {
        let template_segs: Vec<&str> = template.split('/').collect();
        if template_segs.len() != path_segs.len() {
            continue;
        }
        let matches = template_segs
            .iter()
            .zip(path_segs.iter())
            .all(|(t, p)| *t == "{id}" || t == p);
        if matches {
            return template;
        }
    }
    "unmatched"
}

/// recId=16：当前 UNIX 毫秒（burst EWMA 的时间锚；Record 注入语义见
/// admission.rs）
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// recId=3 + G5：由 [rate_limit] 段构建入站限流判定件。
///
/// - path 前缀规则 → 中间件端点表（per-tenant 分桶在表内实现）；
/// - path 缺省的全局规则 → 一规则一 tower 层（[`GlobalRateLimitLayer`]，
///   QuotaLimiter 按租户摘要分桶；旧 Governor 路径是全租户共享桶——G5 起
///   全局规则真正按租户维度生效，见模块注释）；
/// - rules 为空 → 单层 TokenBucket(100, 10/s) 共享桶（与旧
///   `Governor::new()` 默认兜底逐位一致）+ 空端点表；
/// - 全部规则均带 path 时全局层退为默认兜底（limiteron FlowControl 校验
///   要求规则非空的同款约束）。
///
/// 同步改动：旧 `Decision::Banned → 403` 分支删除——新旧两路径的规则
/// ban 恒为 None，该分支不可达。
fn build_inbound_limits(
    section: &RateLimitSection,
) -> (Vec<GlobalRateLimitLayer>, EndpointLimitTable) {
    let endpoint_table = EndpointLimitTable::build(section);
    let global_rules: Vec<&RateLimitRule> =
        section.rules.iter().filter(|r| r.path.is_none()).collect();

    if global_rules.is_empty() {
        let retry_after_secs = Arc::new(AtomicU64::new(1));
        let gate = GlobalGate {
            inner: GateLimiter::TokenBucket(Arc::new(TokenBucketLimiter::new(100, 10))),
            last_retry_after_secs: retry_after_secs.clone(),
            deny_retry_after_secs: 1,
        };
        return (
            vec![global_rate_limit_layer(gate, retry_after_secs, true)],
            endpoint_table,
        );
    }

    let layers = global_rules
        .into_iter()
        .map(|r| {
            let retry_after_secs = Arc::new(AtomicU64::new(1));
            let window_secs = r.window_secs.max(1);
            let quota = QuotaLimiter::new(QuotaConfig {
                quota_type: QuotaType::Count,
                limit: r.max_requests,
                window_size: window_secs,
                allow_overdraft: false,
                overdraft_limit_percent: 0,
                alert_config: Default::default(),
            });
            let gate = GlobalGate {
                inner: GateLimiter::Quota(Arc::new(quota)),
                last_retry_after_secs: retry_after_secs.clone(),
                deny_retry_after_secs: window_secs,
            };
            global_rate_limit_layer(gate, retry_after_secs, r.per_tenant)
        })
        .collect();
    (layers, endpoint_table)
}

/// 创建 axum 路由，集成 limiteron 限流中间件与 API Key 认证中间件
///
/// 层次（外→内）：Context（请求追踪）→ Trace → 认证 → 端点限流 →
/// 全局 tower 限流 → 准入 → handler。
/// 认证中间件的 state 是 `Arc<ConfigManager>`：每请求从热配置现读 [auth] 段
///（SHA256 摘要存储），key 吊销/轮换与 enabled 开关热更新立即生效；
/// 段缺失时 `enabled=false` 放行（旧配置兼容）。
/// 限流判定件在 Router 构造期从 [rate_limit] 段构建
///（restart-required，热下发列二期）：端点表 + 准入进 from_fn state，
/// 全局规则进 tower 层（G5，一规则一层）。
/// 注意：健康检查由 sdforge `#[forge(version="v1")]` 注册为 /api/v1/health
///（api/health.rs:10，宏加版本前缀），此处不重复注册，避免 merge 时
/// axum duplicate-route panic。
pub async fn create_router(state: Arc<ApiState>) -> Router {
    // forge 端点描述翻译注册进 sdforge（建站期一次性；见 health.rs）
    health::register_forge_i18n();
    // recId=3：[rate_limit] 段消费点（此前该段被 serde 静默丢弃，中间件
    // 恒跑 Governor 库内置默认兜底规则）
    let rate_limit = state.config.read().rate_limit.clone();
    let (global_layers, endpoint_table) = build_inbound_limits(&rate_limit);
    // recId=16：准入状态常驻构造（轻量；[admission].enabled 默认 false
    // 时判定旁路——热开开关立即生效，无需重启）
    let admission = Arc::new(AdmissionState::new(
        state.config.clone(),
        state.vendor_manager.clone(),
    ));
    let limit_state = Arc::new(InboundLimitState {
        endpoint_table,
        admission,
    });

    let router = Router::new()
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
        );
    // G7：Prometheus 指标出口（/metrics）与就绪探针（/readyz）由
    // sdforge 站点（build_with_config）在合并侧自挂（同一 handler +
    // 同一全局注册表）；create_router 侧再挂会导致 merge 时重复路由
    // panic。直连 create_router 的调用方用 sdforge::health::mount_probes
    //（/readyz）或手动挂载 metrics handler 补齐。
    // /api/v1/health（forge 存活探针）保留不变。

    // G11：文档面按开关挂载（默认关 → /api-docs 与 /swagger-ui 不存在）。
    // 合并点在 with_state 之后（两侧均为 Router<()>，无状态 handlers）。
    // 开关直读 config（不经 openapi_docs 模块——该模块只承载 inventory
    // 注册，与挂载解耦）。
    let mut router = router.with_state(state.clone());
    if state.config.read().docs.enabled {
        router = router.merge(sdforge::docs::swagger_ui_router());
    }

    let router = router.layer(from_fn_with_state(
        limit_state.clone(),
        admission_middleware,
    ));
    // G5：全局 tower 层叠在端点限流内侧、准入外侧（执行序：端点 → 全局 →
    // 准入；axum 后声明的 layer 更靠外，故逆序叠，以保持配置序执行）。
    let router = global_layers
        .into_iter()
        .rev()
        .fold(router, |r, layer| r.layer(layer));
    router
        .layer(from_fn_with_state(limit_state, endpoint_limit_middleware))
        .layer(from_fn_with_state(state.config.clone(), auth_middleware))
        .layer(TraceLayer::new_for_http())
        // G9：请求追踪最外层——X-Request-Id 解析/生成 + task_local 挂载 +
        // 响应头回传（sdforge context）；span 关联在准入中间件内完成。
        .layer(axum::middleware::from_fn(
            sdforge::context::context_middleware,
        ))
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

    /// 健康检查白名单（评审 major 修复锁定，G5 保持）。
    ///
    /// 断言空洞背景：axum 0.8 的 `layer` 只作用于已有路由
    ///（axum-0.8.9 src/docs/routing/layer.md:6 "the middleware is only
    /// applied to existing routes"），评审探针实证 create_router 之后
    /// 再 `.route()` 的路径不经过中间件——此前
    /// tests/rate_limit_test.rs 的集成形态无论白名单代码存在与否都会过。
    /// 本用例 route 先于 layer 构建：`/health` 白名单豁免断言 +
    /// `/limited` 对照组（第 2 次 429）共同证明中间件真实挂载且生效；
    /// G5 起白名单由端点中间件与 tower 键提取器双重执行，此处锁定两者。
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
        let (global_layers, endpoint_table) = build_inbound_limits(&section);
        let state = Arc::new(InboundLimitState {
            endpoint_table,
            admission: {
                // 白名单单测不需要准入状态：空 manager 接线（enabled 默认
                // false，判定旁路）
                let cm = Arc::new(crate::config::ConfigManager::new(min_config()));
                let vm = Arc::new(crate::vendor::VendorManager::new(cm.clone()));
                Arc::new(AdmissionState::new(cm, vm))
            },
        });

        // 关键顺序：route 在 layer 之前 → 中间件作用于全部路由；
        // 层序与 create_router 一致：端点（外）→ 全局 tower（内）。
        let mut app = Router::new()
            .route("/health", get_route(|| async { "OK" }))
            .route("/limited", get_route(|| async { "OK" }))
            .layer(from_fn_with_state(state.clone(), admission_middleware));
        for layer in global_layers.into_iter().rev() {
            app = app.layer(layer);
        }
        let app = app.layer(from_fn_with_state(state, endpoint_limit_middleware));

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
            "control path must actually pass through endpoint_limit_middleware"
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
        let (_, mut table) = build_inbound_limits(&section);
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

    /// 空规则默认兜底（G3 复核 MEDIUM A-M16）：TokenBucket(100, 10/s)    /// 共享桶——100 次内全放行，第 101 次拒绝（与旧 Governor::new()
    /// 默认一致；租户维度不分桶）。
    #[tokio::test]
    async fn default_gate_matches_legacy_token_bucket_semantics() {
        use limiteron::limiters::Limiter as _;

        let gate = GlobalGate {
            inner: GateLimiter::TokenBucket(Arc::new(TokenBucketLimiter::new(100, 10))),
            last_retry_after_secs: Arc::new(AtomicU64::new(1)),
            deny_retry_after_secs: 1,
        };
        for _ in 0..100 {
            assert!(
                gate.check("any-tenant").await.is_ok(),
                "first 100 requests must pass the default bucket"
            );
        }
        assert!(
            gate.check("any-tenant").await.is_err(),
            "101st request must be rejected (bucket exhausted)"
        );
        assert!(
            gate.check("other-tenant").await.is_err(),
            "default bucket must be shared across tenants"
        );
    }

    /// 注册表↔指标一致性（G3 复核 MEDIUM A-M13）：inventory 里每条路径在
    /// RED 标签表里都有归一模板（/readyz、/metrics 除外——记录层跳过二者，
    /// 模板表已同步移除，归 unmatched 即正确）。
    #[test]
    fn inventory_paths_all_have_metric_labels() {
        let spec = sdforge::openapi::generate_openapi_spec();
        let paths = spec.paths.paths.keys().cloned().collect::<Vec<_>>();
        assert!(!paths.is_empty(), "spec 不得为空");
        for path in &paths {
            if path == "/readyz" || path == "/metrics" {
                continue;
            }
            assert_ne!(
                metrics_route_label(path),
                "unmatched",
                "inventory 路径 {path} 必须有 RED 标签模板"
            );
        }
    }
}
