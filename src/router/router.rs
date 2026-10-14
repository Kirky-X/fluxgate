// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{ConfigManager, ModelConfig, PricingConfig, VendorConfig};
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::vendor::{VendorManager, VendorStatus, WindowInfo};
use rand::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use super::affinity::AffinityTable;
use super::bandit::{BanditState, LINUCB_DIM};

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub vendor: VendorConfig,
    pub model: ModelConfig,
    pub strategy: RoutingStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoutingStrategy {
    Direct,
    Abstract,
    Global,
}

#[derive(Clone)]
pub struct Router {
    config: Arc<ConfigManager>,
    vendor_manager: Arc<VendorManager>,
    /// 会话亲和表。None = 未接线，路由行为与历史一致
    affinity: Option<Arc<AffinityTable>>,
    /// SLA 虚拟队列 Q：键 = 租户（无租户 "global"），每轮加权选择后
    /// 对偶更新 Q ← max(0, Q + α − ŝ)；sla_v=0（默认）不读写。重启归零。
    sla_queues: Arc<RwLock<HashMap<String, f64>>>,
    /// 「候选集全无定价」一次性 warn 标记（防每请求刷屏）
    sla_no_pricing_warned: Arc<AtomicBool>,
    /// LinUCB 臂状态。None = 未接线，该档回退 weighted
    bandit: Option<Arc<BanditState>>,
}

impl Router {
    pub fn new(config: Arc<ConfigManager>, vendor_manager: Arc<VendorManager>) -> Self {
        Self {
            config,
            vendor_manager,
            affinity: None,
            sla_queues: Arc::new(RwLock::new(HashMap::new())),
            sla_no_pricing_warned: Arc::new(AtomicBool::new(false)),
            bandit: None,
        }
    }

    /// 接线 LinUCB 状态（builder）。未接线时 linucb 档回退 weighted
    pub fn with_bandit(mut self, bandit: Arc<BanditState>) -> Self {
        self.bandit = Some(bandit);
        self
    }

    /// 接线会话亲和表（builder）：亲和命中者优先（hard 模式在合格集合内直接选中，soft 为权重 boost；集合外绝不强选）
    pub fn with_affinity(mut self, affinity: Arc<AffinityTable>) -> Self {
        self.affinity = Some(affinity);
        self
    }

    pub async fn route_excluding(
        &self,
        abstract_model: &str,
        excluded: &[String],
    ) -> Result<RoutingDecision, GateError> {
        self.route_with_session_excluding(abstract_model, None, excluded)
            .await
    }

    pub async fn route(&self, abstract_model: &str) -> Result<RoutingDecision, GateError> {
        self.route_excluding(abstract_model, &[]).await
    }

    /// 带会话键路由：亲和命中者优先，未命中走 weighted_select；
    /// 绑定由 API 层执行成功后回写
    pub async fn route_with_session(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
    ) -> Result<RoutingDecision, GateError> {
        self.route_with_session_excluding(abstract_model, session_key, &[])
            .await
    }

    /// 带会话键 + 排除集的路由（失败迁移重路由的亲和形态）
    pub async fn route_with_session_excluding(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
        excluded: &[String],
    ) -> Result<RoutingDecision, GateError> {
        self.route_inner(
            abstract_model,
            session_key,
            None,
            excluded,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// 带请求级权重的路由：request_weight 经 `[routing]`.allow_request_weight
    /// 门控后覆盖 model.weight；tenant 为调用方 API key 哈希，仅
    /// affinity_tenant_scoped 开启时参与亲和键。
    #[allow(clippy::too_many_arguments)] // 路由上下文穿线
    pub async fn route_weighted(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
        tenant: Option<&str>,
        excluded: &[String],
        request_weight: Option<f64>,
        // LinUCB 请求特征（5 维）；None 或非 linucb 档时不参与
        linucb_features: Option<&[f64]>,
        // x-deadline-ms（毫秒），仅 [routing].lps_deadline_gate 开启时消费
        lps_deadline_ms: Option<f64>,
        // 前缀亲和 + 长上下文价的请求级上下文（None = 旁路）
        ctx: Option<RouteCtx>,
    ) -> Result<RoutingDecision, GateError> {
        let gated = self.gate_request_weight(request_weight);
        self.route_inner(
            abstract_model,
            session_key,
            tenant,
            excluded,
            gated,
            linucb_features,
            lps_deadline_ms,
            ctx,
        )
        .await
    }

    /// 请求级权重门控：allow_request_weight 关闭（默认）时丢弃为 None；pub 供确定性单测
    pub fn gate_request_weight(&self, request_weight: Option<f64>) -> Option<f64> {
        if self.config.read().routing.allow_request_weight {
            request_weight
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn route_inner(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
        tenant: Option<&str>,
        excluded: &[String],
        request_weight: Option<f64>,
        linucb_features: Option<&[f64]>,
        lps_deadline_ms: Option<f64>,
        ctx: Option<RouteCtx>,
    ) -> Result<RoutingDecision, GateError> {
        // SLA 分桶用原始租户，不受 affinity_tenant_scoped 门控约束
        let sla_tenant = tenant;
        // 亲和键租户隔离门控：关闭（默认）一律 legacy 键
        let tenant = if self.config.read().routing.affinity_tenant_scoped {
            tenant
        } else {
            None
        };
        let normalized = crate::router::bandit::linucb_arm_key(abstract_model);
        let is_auto = normalized == "__global__";

        // Try abstract route first (model name without vendor prefix)
        if let Some(decision) = self
            .try_abstract_route(
                &normalized,
                excluded,
                session_key,
                tenant,
                sla_tenant,
                request_weight,
                linucb_features,
                lps_deadline_ms,
                ctx,
            )
            .await
        {
            return Ok(decision);
        }

        // Try direct route (vendor.model format)
        if let Some(decision) = self.try_direct_route(&normalized, excluded).await {
            return Ok(decision);
        }

        // For "auto" request, try global route
        if is_auto {
            // [global_model].enabled=false：auto 档显式关闭（全局池不参与选路）。
            // 报显性 Validation（含「未找到」原句 + 开关来源），避免配置开关
            // 静默空转或把「管理员关闭」误报成「模型不存在」
            if !self.config.is_global_model_enabled() {
                return Err(GateError::Validation {
                    field: t_args(
                        "router-global-model-disabled",
                        &[("model", abstract_model.to_string())],
                    ),
                });
            }
            if let Some(decision) = self
                .try_global_route(
                    excluded,
                    session_key,
                    tenant,
                    sla_tenant,
                    request_weight,
                    linucb_features,
                    lps_deadline_ms,
                    ctx,
                )
                .await
            {
                return Ok(decision);
            }
        }

        Err(GateError::Validation {
            field: t_args(
                "router-model-not-found",
                &[("model", abstract_model.to_string())],
            ),
        })
    }

    /// 构建 abstract_model 的换路 fallback 链（priority 越小越优先，
    /// trigger=All）；无 enabled 候选时返回 None，API 层保持单发语义。
    #[cfg(feature = "resilience")]
    pub fn build_fallback_chain(
        &self,
        abstract_model: &str,
    ) -> Option<crate::vendor::resilience::FallbackChain> {
        use crate::vendor::resilience::{FallbackChain, FallbackTarget, FallbackTrigger};

        let normalized = abstract_model.to_lowercase();
        let candidates = self.config.resolve_model(&normalized);
        let targets: Vec<FallbackTarget> = candidates
            .iter()
            .filter(|(v, m)| v.enabled && m.enabled)
            .map(|(v, m)| FallbackTarget {
                vendor_id: v.id.clone(),
                model_id: m.model_id.clone(),
                priority: v.priority,
            })
            .collect();

        if targets.is_empty() {
            None
        } else {
            Some(FallbackChain::new(targets, FallbackTrigger::All))
        }
    }

    /// 队列深度信号读取（默认 "queue"），权重计算与路由解释共用
    fn queue_len_for(&self, vendor: &VendorConfig, load_signal: &str) -> usize {
        match load_signal {
            "in_flight" => self.vendor_manager.get_in_flight(&vendor.id),
            "in_flight_relative" => {
                let in_flight = self.vendor_manager.get_in_flight(&vendor.id) as f64;
                let fleet_mean = self.vendor_manager.fleet_mean_in_flight();
                relative_in_flight_to_len(in_flight, fleet_mean)
            }
            // "queue" 与未知值：队列深度
            _ => self.vendor_manager.get_queue_length(&vendor.id),
        }
    }

    /// 容量份额（四级权重回退第 3 级输入），权重计算与路由解释共用
    async fn capacity_shares(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
    ) -> HashMap<String, f64> {
        let mut capacity: HashMap<String, f64> = HashMap::new();
        if candidates.iter().any(|(_, m)| m.weight <= 0.0) {
            let mut limits: Vec<(String, u64)> = Vec::new();
            for (vendor, _) in candidates {
                if let Some(limit) = self
                    .vendor_manager
                    .get_window_info(&vendor.id)
                    .await
                    .and_then(|info| info.max_tokens)
                {
                    limits.push((vendor.id.clone(), limit));
                }
            }
            let max_limit = limits.iter().map(|(_, l)| *l).max().unwrap_or(0);
            if max_limit > 0 {
                capacity = limits
                    .into_iter()
                    .map(|(id, limit)| (id, limit as f64 / max_limit as f64))
                    .collect();
            }
        }
        capacity
    }

    // ============ 路由可解释性 ============

    /// 逐候选解释（`POST /admin/route/explain` 数据源）：纯只读，零计量副作用。
    /// reason 优先级（首个命中即定）：not_in_catalog → disabled → throttled →
    /// cooling_down → affinity_bound_elsewhere → queue_over_limit → weight_zero
    /// → eligible；限流窗口耗尽先于状态判定（区分「配额耗尽」与「错误驱动冷却」）。
    /// `model` 无任何 vendor 服务时返回空；`session_key=None` 跳过亲和判定。
    pub async fn explain_route(
        &self,
        model: &str,
        session_key: Option<&str>,
    ) -> Vec<CandidateAssessment> {
        let config = self.config.read();
        let vendors: Vec<VendorConfig> = config.vendors.clone();
        let routing = config.routing.clone();
        drop(config);

        // 每 vendor 的服务条目（大小写不敏感；model_id 或 abstract_id 命中）。
        let mut rows: Vec<(VendorConfig, Option<ModelConfig>)> = Vec::new();
        let mut served_any = false;
        for vendor in vendors {
            let entry = vendor
                .models
                .iter()
                .find(|m| {
                    m.model_id.eq_ignore_ascii_case(model)
                        || m.abstract_id
                            .as_deref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(model))
                })
                .cloned();
            served_any = served_any || entry.is_some();
            rows.push((vendor, entry));
        }
        if !served_any {
            return Vec::new();
        }

        // 亲和绑定：只读 lookup 不计量；hard 模式才构成排他（soft 为 boost）
        let affinity_hard = !routing.affinity_mode.trim().eq_ignore_ascii_case("soft");
        let bound_vendor: Option<String> = if affinity_hard {
            match (self.affinity.as_ref(), session_key) {
                (Some(table), Some(session)) => table.lookup(None, session).await,
                _ => None,
            }
        } else {
            None
        };

        // 先行筛选（与权重无关的 reason），收集合格对。
        enum Screen {
            Excluded(CandidateAssessment),
            Candidate(Box<(VendorConfig, ModelConfig)>),
        }
        let mut screens: Vec<Screen> = Vec::new();
        for (vendor, entry) in rows {
            let Some(model_entry) = entry else {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model.to_string(),
                    REASON_NOT_IN_CATALOG,
                    t_args(
                        "router-explain-not-in-catalog",
                        &[("vendor", vendor.id.clone()), ("model", model.to_string())],
                    ),
                )));
                continue;
            };
            if !vendor.enabled || !model_entry.enabled {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model_entry.model_id.clone(),
                    REASON_DISABLED,
                    format!(
                        "vendor enabled={} model enabled={}",
                        vendor.enabled, model_entry.enabled
                    ),
                )));
                continue;
            }
            let status = self.vendor_manager.get_status(&vendor.id).await;
            let window = self.vendor_manager.get_window_info(&vendor.id).await;
            // 限流窗口耗尽先于状态（get_status peek 窗口——耗尽者状态恒
            // Throttled；先判窗口以区分「配额耗尽」与「错误驱动冷却」）。
            if matches!(&window, Some(info) if !info.has_budget()) {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model_entry.model_id.clone(),
                    REASON_THROTTLED,
                    t("router-explain-throttled"),
                )));
                continue;
            }
            if status != VendorStatus::Available {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model_entry.model_id.clone(),
                    REASON_COOLING_DOWN,
                    t_args(
                        "router-explain-cooling-down",
                        &[("status", format!("{status:?}"))],
                    ),
                )));
                continue;
            }
            // 全部 key 黑名单 = 无可用出站凭证：报 cooling_down 并注明
            // key 池耗尽，detail 携带指纹明细（截断哈希，非 key 原文）
            if let Some(fingerprints) = self.vendor_manager.key_pool_exhausted(&vendor.id) {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model_entry.model_id.clone(),
                    REASON_COOLING_DOWN,
                    t_args(
                        "router-explain-key-pool-exhausted",
                        &[
                            ("n", fingerprints.len().to_string()),
                            ("prints", fingerprints.join(",")),
                        ],
                    ),
                )));
                continue;
            }
            if let Some(bound) = bound_vendor.as_deref() {
                if bound != vendor.id {
                    screens.push(Screen::Excluded(CandidateAssessment::excluded(
                        vendor.id.clone(),
                        model_entry.model_id.clone(),
                        REASON_AFFINITY_BOUND_ELSEWHERE,
                        t_args(
                            "router-explain-affinity-bound-elsewhere",
                            &[("bound", bound.to_string())],
                        ),
                    )));
                    continue;
                }
            }
            let queue_len = self.queue_len_for(&vendor, &routing.load_signal);
            if queue_len >= QUEUE_SATURATION_LEN {
                screens.push(Screen::Excluded(CandidateAssessment::excluded(
                    vendor.id.clone(),
                    model_entry.model_id.clone(),
                    REASON_QUEUE_OVER_LIMIT,
                    t_args(
                        "router-explain-queue-over-limit",
                        &[
                            ("queue_len", queue_len.to_string()),
                            ("saturation", QUEUE_SATURATION_LEN.to_string()),
                        ],
                    ),
                )));
                continue;
            }
            screens.push(Screen::Candidate(Box::new((vendor, model_entry))));
        }

        // 合格对的生产精确权重（与路由同一方法）+ 5 因子分解。
        let eligible: Vec<(VendorConfig, ModelConfig)> = screens
            .iter()
            .filter_map(|s| match s {
                Screen::Candidate(pair) => Some(pair.as_ref().clone()),
                Screen::Excluded(_) => None,
            })
            .collect();
        let finals = self
            .calculate_weights_overridden(eligible.clone(), None, None, None)
            .await;
        let final_by_vendor: HashMap<&str, f64> = finals
            .iter()
            .map(|(w, (v, _))| (v.id.as_str(), *w))
            .collect();
        let capacity = self.capacity_shares(&eligible).await;

        let selection = selection_tier(&routing);
        let mut out = Vec::with_capacity(screens.len());
        for screen in screens {
            match screen {
                Screen::Excluded(assessment) => out.push(assessment),
                Screen::Candidate(pair) => {
                    let (vendor, model_entry) = pair.as_ref().clone();
                    let eff = resolve_effective_model_weight(
                        model_entry.weight,
                        capacity.get(&vendor.id).copied(),
                    );
                    let queue_len = self.queue_len_for(&vendor, &routing.load_signal);
                    let window = self.vendor_manager.get_window_info(&vendor.id).await;
                    let ewma = self.vendor_manager.metrics().vendor_ewma(&vendor.id);
                    let factors =
                        compute_weight_breakdown(eff, queue_len, window.as_ref(), &routing, ewma);
                    let base = factors.total;
                    let weight = final_by_vendor
                        .get(vendor.id.as_str())
                        .copied()
                        .unwrap_or(base);
                    if base <= 0.0 {
                        out.push(CandidateAssessment::excluded(
                            vendor.id.clone(),
                            model_entry.model_id.clone(),
                            REASON_WEIGHT_ZERO,
                            t("router-explain-weight-zero"),
                        ));
                    } else {
                        out.push(CandidateAssessment::eligible(
                            vendor.id.clone(),
                            model_entry.model_id.clone(),
                            factors,
                            base,
                            weight,
                            selection,
                        ));
                    }
                }
            }
        }
        out
    }

    /// 使用路由提示路由：权重仅在 model 提示 / 默认路由路径生效
    #[allow(clippy::too_many_arguments)] // 路由上下文穿线
    pub async fn route_with_hints(
        &self,
        abstract_model: &str,
        preferred_vendor: Option<&str>,
        preferred_model: Option<&str>,
        request_weight: Option<f64>,
        tenant: Option<&str>,
        // LinUCB 请求特征（语义同 [`Self::route_weighted`]）
        linucb_features: Option<&[f64]>,
        // x-deadline-ms 头（语义同 [`Self::route_weighted`]）
        lps_deadline_ms: Option<f64>,
        // 前缀亲和 + 长上下文价 ctx（None = 旁路）
        ctx: Option<RouteCtx>,
    ) -> Result<RoutingDecision, GateError> {
        // 入口统一过权重门控（route_weighted 末尾二次门控幂等）
        let request_weight = self.gate_request_weight(request_weight);
        if let Some(vendor_id) = preferred_vendor {
            if let Some(decision) = self.try_vendor_route(vendor_id, preferred_model).await {
                return Ok(decision);
            }
            return Err(GateError::Validation {
                field: t_args(
                    "router-vendor-unavailable",
                    &[("vendor", vendor_id.to_string())],
                ),
            });
        }

        // hints 是显式指定，不接亲和
        if let Some(model_id) = preferred_model {
            let normalized = model_id.to_lowercase();
            if let Some(decision) = self
                .try_abstract_route(
                    &normalized,
                    &[],
                    None,
                    tenant,
                    tenant,
                    request_weight,
                    linucb_features,
                    lps_deadline_ms,
                    ctx,
                )
                .await
            {
                return Ok(decision);
            }
        }

        self.route_weighted(
            abstract_model,
            None,
            tenant,
            &[],
            request_weight,
            linucb_features,
            lps_deadline_ms,
            ctx,
        )
        .await
    }

    async fn try_vendor_route(
        &self,
        vendor_id: &str,
        model_hint: Option<&str>,
    ) -> Option<RoutingDecision> {
        // 请求头书写大小写不敏感；状态查询用配置规范 id，避免门控漂移
        let vendor = self.config.get_vendor_ci(vendor_id)?;
        if !vendor.enabled {
            return None;
        }

        if self.vendor_manager.get_status(&vendor.id).await != VendorStatus::Available {
            return None;
        }

        // 模型名匹配与直连路径同源：ASCII 大小写折叠、零分配
        if let Some(model_id) = model_hint {
            if let Some(model) = vendor
                .models
                .iter()
                .find(|m| {
                    m.enabled
                        && (m.model_id.eq_ignore_ascii_case(model_id)
                            || m.abstract_id
                                .as_ref()
                                .is_some_and(|a| a.eq_ignore_ascii_case(model_id)))
                })
                .cloned()
            {
                return Some(RoutingDecision {
                    vendor,
                    model,
                    strategy: RoutingStrategy::Direct,
                });
            }
        }

        // 无提示时取第一个启用模型
        let model = vendor.models.iter().find(|m| m.enabled)?.clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy: RoutingStrategy::Direct,
        })
    }

    async fn try_direct_route(&self, input: &str, excluded: &[String]) -> Option<RoutingDecision> {
        let parts: Vec<&str> = input.splitn(2, '.').collect();
        if parts.len() != 2 {
            return None;
        }

        let (vendor_id, model_id) = (parts[0], parts[1]);

        // 排除集内容为配置规范 id，与请求侧书写大小写无关地比较
        if excluded.iter().any(|e| e.eq_ignore_ascii_case(vendor_id)) {
            return None;
        }

        let vendor = self.config.get_vendor_ci(vendor_id)?;
        if !vendor.enabled {
            return None;
        }

        let model = vendor
            .models
            .iter()
            .find(|m| {
                m.enabled
                    && (m.model_id.eq_ignore_ascii_case(model_id)
                        || m.abstract_id
                            .as_ref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(model_id)))
            })?
            .clone();

        Some(RoutingDecision {
            vendor,
            model,
            strategy: RoutingStrategy::Direct,
        })
    }

    #[allow(clippy::too_many_arguments)] // 路由上下文穿线
    async fn try_abstract_route(
        &self,
        abstract_id: &str,
        excluded: &[String],
        session_key: Option<&str>,
        tenant: Option<&str>,
        // SLA Q 分桶用原始租户（tenant 已过亲和键门控）
        sla_tenant: Option<&str>,
        request_weight: Option<f64>,
        // LinUCB 请求特征（selection="linucb" 档选择输入）
        linucb_features: Option<&[f64]>,
        // x-deadline-ms 头（lps_deadline_gate 开启时消费）
        lps_deadline_ms: Option<f64>,
        ctx: Option<RouteCtx>,
    ) -> Option<RoutingDecision> {
        let (available_candidates, status_dropped) = self
            .collect_abstract_candidates(abstract_id, excluded)
            .await;

        if available_candidates.is_empty() {
            // fail-open 安全网：全候选被 CB/Throttled 剔除且
            // [routing].fail_open_when_all_cooling 开启时，回落
            //「忽略状态但 enabled」的候选集加权选择；完整语义见 fail_open_decision
            return self
                .fail_open_decision(
                    RoutingStrategy::Abstract,
                    "router-fail-open-engaged",
                    status_dropped,
                    request_weight,
                    sla_tenant,
                )
                .await;
        }

        // 亲和：hard（默认）命中即返回；soft 命中软化权重乘 (1+factor)
        // 参与竞争；未命中候选集原样归还，续用 LPS/LinUCB/加权阶段
        let mut available_candidates = match self
            .affinity_decision(
                RoutingStrategy::Abstract,
                available_candidates,
                session_key,
                tenant,
                request_weight,
                sla_tenant,
            )
            .await
        {
            Ok(decision) => return Some(decision),
            Err(models) => models,
        };

        // LPS 硬约束门控（默认关）：可行集筛选 + 空集回退 Ŵ 最小者
        match self.lps_deadline_filter(&available_candidates, lps_deadline_ms) {
            LpsDeadline::Feasible(candidates) => available_candidates = candidates,
            LpsDeadline::EmptyPickMin => {
                if let Some(decision) =
                    self.lps_min_wait_decision(&available_candidates, RoutingStrategy::Abstract)
                {
                    return Some(decision);
                }
            }
            LpsDeadline::NoFilter => {}
        }

        // LinUCB 选择档：selection="linucb"（热读）且接线且特征可得时
        // argmax UCB，否则回退下方 weighted
        if let Some(decision) = self.linucb_route(
            abstract_id,
            &available_candidates,
            linucb_features,
            RoutingStrategy::Abstract,
            ctx.as_ref(),
        ) {
            return Some(decision);
        }

        Some(
            self.weighted_select(
                available_candidates,
                RoutingStrategy::Abstract,
                request_weight,
                None,
                sla_tenant,
                ctx,
            )
            .await,
        )
    }

    #[allow(clippy::too_many_arguments)] // 路由上下文穿线
    async fn try_global_route(
        &self,
        excluded: &[String],
        session_key: Option<&str>,
        tenant: Option<&str>,
        // SLA 虚拟队列键用原始租户（语义同 try_abstract_route）
        sla_tenant: Option<&str>,
        request_weight: Option<f64>,
        // LinUCB 请求特征（selection="linucb" 档选择输入）
        linucb_features: Option<&[f64]>,
        // x-deadline-ms 头（语义同 try_abstract_route）
        lps_deadline_ms: Option<f64>,
        ctx: Option<RouteCtx>,
    ) -> Option<RoutingDecision> {
        let config = self.config.read();

        let (all_models, status_dropped) = self
            .collect_global_candidates(&config.vendors, excluded)
            .await;

        if all_models.is_empty() {
            // fail-open 安全网（语义同 try_abstract_route 注释）
            return self
                .fail_open_decision(
                    RoutingStrategy::Global,
                    "router-global-fail-open-engaged",
                    status_dropped,
                    request_weight,
                    sla_tenant,
                )
                .await;
        }

        // 亲和：语义同 try_abstract_route
        let mut all_models = match self
            .affinity_decision(
                RoutingStrategy::Global,
                all_models,
                session_key,
                tenant,
                request_weight,
                sla_tenant,
            )
            .await
        {
            Ok(decision) => return Some(decision),
            Err(models) => models,
        };

        // LPS 硬约束门控（语义同 try_abstract_route；默认关）
        match self.lps_deadline_filter(&all_models, lps_deadline_ms) {
            LpsDeadline::Feasible(candidates) => all_models = candidates,
            LpsDeadline::EmptyPickMin => {
                if let Some(decision) =
                    self.lps_min_wait_decision(&all_models, RoutingStrategy::Global)
                {
                    return Some(decision);
                }
            }
            LpsDeadline::NoFilter => {}
        }

        // LinUCB 选择档（语义同 try_abstract_route）；global 臂键
        // "__global__" 与 abstract 键空间隔离，全模型共享一臂组
        if let Some(decision) = self.linucb_route(
            "__global__",
            &all_models,
            linucb_features,
            RoutingStrategy::Global,
            ctx.as_ref(),
        ) {
            return Some(decision);
        }

        Some(
            self.weighted_select(
                all_models,
                RoutingStrategy::Global,
                request_weight,
                None,
                sla_tenant,
                ctx,
            )
            .await,
        )
    }

    /// abstract 路径候选收集：enabled + 排除集 + 健康过滤（CB/Throttled
    /// 入 status_dropped 供 fail-open 回落）
    async fn collect_abstract_candidates(
        &self,
        abstract_id: &str,
        excluded: &[String],
    ) -> (
        Vec<(VendorConfig, ModelConfig)>,
        Vec<(VendorConfig, ModelConfig)>,
    ) {
        let mut available: Vec<(VendorConfig, ModelConfig)> = Vec::new();
        let mut status_dropped: Vec<(VendorConfig, ModelConfig)> = Vec::new();
        for (vendor, model) in self.config.resolve_model(abstract_id) {
            if !vendor.enabled || !model.enabled {
                continue;
            }
            // 换路 fallback 的排除集
            if excluded.iter().any(|e| e.eq_ignore_ascii_case(&vendor.id)) {
                continue;
            }
            if self.vendor_manager.get_status(&vendor.id).await == VendorStatus::Available {
                available.push((vendor, model));
            } else {
                status_dropped.push((vendor, model));
            }
        }
        (available, status_dropped)
    }

    /// 全局路径候选收集：过滤语义同 collect_abstract_candidates
    async fn collect_global_candidates(
        &self,
        vendors: &[VendorConfig],
        excluded: &[String],
    ) -> (
        Vec<(VendorConfig, ModelConfig)>,
        Vec<(VendorConfig, ModelConfig)>,
    ) {
        let mut all_models: Vec<(VendorConfig, ModelConfig)> = Vec::new();
        let mut status_dropped: Vec<(VendorConfig, ModelConfig)> = Vec::new();

        for vendor in vendors {
            if !vendor.enabled {
                continue;
            }
            if excluded.iter().any(|e| e.eq_ignore_ascii_case(&vendor.id)) {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                if self.vendor_manager.get_status(&vendor.id).await != VendorStatus::Available {
                    status_dropped.push((vendor.clone(), model.clone()));
                    continue;
                }

                all_models.push((vendor.clone(), model.clone()));
            }
        }
        (all_models, status_dropped)
    }

    /// fail-open 安全网（abstract/global 共用）：status_dropped 非空且
    /// fail_open_when_all_cooling 开启时按 strategy 加权选择并告警，否则 None
    async fn fail_open_decision(
        &self,
        strategy: RoutingStrategy,
        engaged_msg: &str,
        status_dropped: Vec<(VendorConfig, ModelConfig)>,
        request_weight: Option<f64>,
        sla_tenant: Option<&str>,
    ) -> Option<RoutingDecision> {
        if status_dropped.is_empty() || !self.config.read().routing.fail_open_when_all_cooling {
            return None;
        }
        tracing::warn!(count = status_dropped.len(), "{}", t(engaged_msg));
        Some(
            self.weighted_select(
                status_dropped,
                strategy,
                request_weight,
                None,
                sla_tenant,
                None,
            )
            .await,
        )
    }

    /// 亲和决策（abstract/global 共用）：soft 命中带 boost 加权选择；
    /// hard 命中且绑定 vendor 在集合内直接选它；否则 Err 归还候选集
    async fn affinity_decision(
        &self,
        strategy: RoutingStrategy,
        all_models: Vec<(VendorConfig, ModelConfig)>,
        session_key: Option<&str>,
        tenant: Option<&str>,
        request_weight: Option<f64>,
        sla_tenant: Option<&str>,
    ) -> Result<RoutingDecision, Vec<(VendorConfig, ModelConfig)>> {
        let bound = match self.affinity_hit(&all_models, session_key, tenant).await {
            Some(bound) => bound,
            None => return Err(all_models),
        };
        let routing = self.config.read().routing.clone();
        if routing.affinity_mode.trim().eq_ignore_ascii_case("soft") {
            // soft 命中：候选集消费进加权选择
            let factor = routing.affinity_boost_factor.max(0.0);
            return Ok(self
                .weighted_select(
                    all_models,
                    strategy,
                    request_weight,
                    Some((&bound, factor)),
                    sla_tenant,
                    None,
                )
                .await);
        }
        // hard 命中：绑定 vendor 在集合内直接选它，否则候选集原样归还
        match all_models.iter().find(|(v, _)| v.id == bound) {
            Some((vendor, model)) => Ok(RoutingDecision {
                vendor: vendor.clone(),
                model: model.clone(),
                strategy,
            }),
            None => Err(all_models),
        }
    }

    /// 会话亲和命中：返回命中 vendor_id（hard 模式由调用方在合格集合内直接选中，soft 为 boost；集合外绝不强选）。
    /// 记账：命中且在集合 → affinity_hits；命中但不可用 → 清旧键 + miss；
    /// 无绑定/未接线/无 session_key → miss 或不计
    async fn affinity_hit(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
        session_key: Option<&str>,
        tenant: Option<&str>,
    ) -> Option<String> {
        let affinity = self.affinity.as_ref()?;
        let session = session_key?;
        let metrics = self.vendor_manager.metrics();

        match affinity.lookup(tenant, session).await {
            Some(vendor_id) => {
                if candidates.iter().any(|(v, _)| v.id == vendor_id) {
                    metrics.record_affinity_hit();
                    return Some(vendor_id);
                }
                // 绑定 vendor 已不在集合：清旧键防持续 miss
                affinity.unbind(tenant, session).await;
                metrics.record_affinity_miss();
                None
            }
            None => {
                metrics.record_affinity_miss();
                None
            }
        }
    }

    /// 前缀亲和候选对（D1）：enabled 且 ctx 带哈希且 n≥2 时产出 (i1,i2)，
    /// 否则 None（选路完全回退历史路径）
    fn affinity_pair(&self, n: usize, ctx: Option<&RouteCtx>) -> Option<(usize, usize)> {
        let ctx = ctx?;
        let enabled = self.config.read().routing.prefix_affinity.enabled;
        if !enabled {
            return None;
        }
        prefix_candidates(ctx.prefix_hash, n)
    }

    /// LinUCB 选择档：selection="linucb"（热读）且接线且特征长度正确时
    /// argmax UCB；任一条件不满足返回 None 回退 weighted。α 热读 ucb_alpha
    ///（非有限/负值按 0 = 纯贪心）
    fn linucb_route(
        &self,
        model_key: &str,
        candidates: &[(VendorConfig, ModelConfig)],
        features: Option<&[f64]>,
        strategy: RoutingStrategy,
        ctx: Option<&RouteCtx>,
    ) -> Option<RoutingDecision> {
        // N1：batch 候选收缩（语义同 weighted_select）
        let batch_restricted;
        let candidates: &[(VendorConfig, ModelConfig)] = if ctx.map(|c| c.batch).unwrap_or(false)
            && candidates.iter().any(|(v, _)| v.supports_batch)
        {
            batch_restricted = candidates
                .iter()
                .filter(|(v, _)| v.supports_batch)
                .cloned()
                .collect::<Vec<_>>();
            &batch_restricted
        } else {
            candidates
        };
        // 前缀亲和：UCB argmax 限定在前缀哈希候选对内（DualMap 形态）
        let restricted;
        let candidates: &[(VendorConfig, ModelConfig)] =
            match self.affinity_pair(candidates.len(), ctx) {
                Some((i1, i2)) => match (candidates.get(i1), candidates.get(i2)) {
                    (Some(a), Some(b)) => {
                        restricted = vec![a.clone(), b.clone()];
                        &restricted
                    }
                    _ => candidates,
                },
                None => candidates,
            };
        if !self
            .config
            .read()
            .routing
            .selection
            .trim()
            .eq_ignore_ascii_case("linucb")
        {
            return None;
        }
        let bandit = self.bandit.as_ref()?;
        let x = features?;
        if x.len() != LINUCB_DIM {
            return None;
        }
        let alpha = {
            let routing_alpha = self.config.read().routing.ucb_alpha;
            if routing_alpha.is_finite() && routing_alpha >= 0.0 {
                routing_alpha
            } else {
                0.0
            }
        };
        let vendor_ids: Vec<&str> = candidates.iter().map(|(v, _)| v.id.as_str()).collect();
        let mut rng = rand::rng();
        let pick = bandit.argmax(model_key, &vendor_ids, x, alpha, &mut rng)?;
        let (vendor, model) = candidates[pick].clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy,
        })
    }

    /// 生产选择入口：async 算权重后注入 rng 同步选择（rng 不跨 await）。
    /// affinity_boost 为 soft 命中乘子 (vendor_id, factor)
    #[allow(clippy::too_many_arguments)]
    async fn weighted_select(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
        strategy: RoutingStrategy,
        request_weight: Option<f64>,
        affinity_boost: Option<(&str, f64)>,
        tenant: Option<&str>,
        ctx: Option<RouteCtx>,
    ) -> RoutingDecision {
        // N1：batch 标记且候选含 supports_batch 厂商 → 收缩候选集（空则忽略）
        let mut candidates = candidates;
        if ctx.map(|c| c.batch).unwrap_or(false) {
            let any_batch = candidates.iter().any(|(v, _)| v.supports_batch);
            if any_batch {
                candidates.retain(|(v, _)| v.supports_batch);
            }
        }
        let mut weighted = self
            .calculate_weights_overridden(candidates, request_weight, tenant, ctx)
            .await;
        if let Some((vendor_id, factor)) = affinity_boost {
            apply_affinity_boost(&mut weighted, vendor_id, factor);
        }
        let pair = ctx.as_ref().and_then(|c| {
            routing_pair_enabled(self)
                .then(|| prefix_candidates(c.prefix_hash, weighted.len()))
                .flatten()
        });
        let decision = {
            let mut rng = rand::rng();
            self.weighted_select_with(weighted, strategy, &mut rng, pair)
        };
        // 选择完成后对偶更新 Q（决策时更新，无需执行结果回传）；
        // direct/亲和 hard 等单候选路径不经此，无对偶更新
        if let Some((_, alpha)) = self.sla_gate() {
            self.sla_record_dual_update(&decision.vendor.id, tenant, alpha);
        }
        decision
    }

    /// async 权重收集：权重计算与 RNG 选择必须两段式（&mut dyn Rng
    /// 非 Send 不能跨 await）；pub 供集成测试确定性注入
    pub async fn calculate_weights(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
    ) -> Vec<(f64, (VendorConfig, ModelConfig))> {
        self.calculate_weights_overridden(candidates, None, None, None)
            .await
    }

    /// 四级权重回退链：请求级权重（X-Weight）→ model.weight →
    /// 容量自动权重（仅 weight<=0，limit 归一 (0,1]）→ 均匀兜底 1.0。
    /// tenant 供 SLA 取 Q；sla_v=0（默认）时该打分项旁路。
    pub async fn calculate_weights_overridden(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
        request_weight: Option<f64>,
        tenant: Option<&str>,
        ctx: Option<RouteCtx>,
    ) -> Vec<(f64, (VendorConfig, ModelConfig))> {
        // 容量权重（第 3 级）：仅无请求级覆盖时计算
        let mut capacity: HashMap<String, f64> = HashMap::new();
        if request_weight.is_none() {
            capacity = self.capacity_shares(&candidates).await;
        }
        let est_prompt_tokens = ctx.as_ref().map(|c| c.est_prompt_tokens);

        // 价格因子上下文（开关关闭时零查询开销）
        let price_factor_enabled = self.config.read().routing.price_factor_enabled;
        let price_factors = if price_factor_enabled {
            self.price_factor_multipliers(&candidates, est_prompt_tokens)
        } else {
            Vec::new()
        };

        let mut weighted = Vec::with_capacity(candidates.len());
        // SLA 打分上下文：sla_v=0（默认）零开销旁路；候选集级预收集
        //（缺价中位数、同一 Q/ŝ 快照）保证集合内可比
        let sla = self.sla_penalty_context(&candidates, tenant);
        // LPS 软约束上下文 — lps_delta=0（默认）None、零开销旁路
        let lps = self.lps_penalty_context(&candidates);
        for (i, (vendor, model)) in candidates.into_iter().enumerate() {
            let effective_model_weight = request_weight.unwrap_or_else(|| {
                resolve_effective_model_weight(model.weight, capacity.get(&vendor.id).copied())
            });
            let mut weight = self.calculate_weight(&vendor, effective_model_weight).await;
            // SLA 惩罚乘在权重链末端，与失败率降权、亲和 boost 独立乘法叠加
            if let Some(sla) = &sla {
                let (cost, satisfaction) = sla.entries[i];
                weight *= sla_penalty_factor(sla.v, cost, sla.q, sla.alpha, satisfaction);
            }
            // LPS 乘法软约束（权重链最末端）
            if let Some(lps) = &lps {
                weight *= lps.factors[i];
            }
            // 价格因子乘性修正（SLA/LPS 之后）
            if price_factor_enabled {
                if let Some(Some(factor)) = price_factors.get(i) {
                    weight *= *factor;
                }
            }
            weighted.push((weight, (vendor, model)));
        }
        weighted
    }

    /// 价格因子：单价 p = (input_price + output_price) / 2（每 1M tokens，
    /// cache 档不计），乘性修正 weight ×= min_p / p ∈ (0,1]；未登记 / 0 价 /
    /// 非有限单价因子 1.0（中性豁免）。查找用精确匹配。
    fn price_factor_multipliers(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
        est_prompt_tokens: Option<u64>,
    ) -> Vec<Option<f64>> {
        let cache_expected = {
            let config = self.config.read();
            config
                .routing
                .price_factor_mode
                .trim()
                .eq_ignore_ascii_case("cache_expected")
        };
        let catalog = &self.config.read().model_catalog;
        if catalog.is_empty() {
            return vec![None; candidates.len()];
        }
        let unit_price = |model_id: &str| -> Option<f64> {
            let entry = catalog.iter().find(|e| e.model_id == model_id)?;
            let mut input = entry.input_price;
            if cache_expected {
                // cache_expected：缓存读价参与期望输入价（前缀亲和生效的部署口径）
                if entry.cache_read_price > 0.0 {
                    input = entry.cache_read_price;
                }
                // N3 长上下文斜率：估 tokens 超阈值且登记了长上下文价则替代
                if let Some(est) = est_prompt_tokens {
                    if entry.long_context_threshold_tokens > 0
                        && est > entry.long_context_threshold_tokens
                        && entry.long_context_input_price > 0.0
                    {
                        input = entry.long_context_input_price;
                    }
                }
            }
            Some((input + entry.output_price) / 2.0).filter(|p| p.is_finite() && *p > 0.0)
        };
        let prices: Vec<Option<f64>> = candidates
            .iter()
            .map(|(_, m)| unit_price(&m.model_id))
            .collect();
        let min_p = prices
            .iter()
            .filter_map(|p| *p)
            .fold(f64::INFINITY, f64::min);
        if !min_p.is_finite() || min_p <= 0.0 {
            return vec![None; candidates.len()];
        }
        prices.into_iter().map(|p| p.map(|p| min_p / p)).collect()
    }

    /// RNG 注入版选择（确定性入口，测试经 StdRng 复现选择分布）。
    /// 温度平滑 w^(1/T) 在本方法内恰好应用一次；入参为已算权重的候选
    ///（权重计算 async 而 &mut dyn Rng 不能跨 await，只能两段式）。
    pub fn weighted_select_with(
        &self,
        weighted: Vec<(f64, (VendorConfig, ModelConfig))>,
        strategy: RoutingStrategy,
        rng: &mut dyn rand::Rng,
        pair: Option<(usize, usize)>,
    ) -> RoutingDecision {
        // 系数与温度热读（同步段，无 await）
        let routing = self.config.read().routing.clone();

        // 前缀亲和 PoTC（D1）：候选生成从"均匀随机"换为"前缀哈希对"，
        // 对内按平滑权重择优。启用时取代 min+buffer 与 power_of_two
        if let Some((i1, i2)) = pair {
            if let (Some(a), Some(b)) = (weighted.get(i1), weighted.get(i2)) {
                let chosen = if a.0 >= b.0 { i1 } else { i2 };
                let (vendor, model) = weighted[chosen].1.clone();
                return RoutingDecision {
                    vendor,
                    model,
                    strategy,
                };
            }
        }

        // min+buffer 候选集（平滑前）：buffer=0（默认）完全跳过
        if routing.latency_buffer > 0.0 {
            if let Some(decision) =
                self.min_buffer_pick(&weighted, strategy, routing.latency_buffer, rng)
            {
                return decision;
            }
        }

        let weighted: Vec<(f64, (VendorConfig, ModelConfig))> = weighted
            .into_iter()
            .map(|(w, c)| (temperature_smooth(w, routing.temperature), c))
            .collect();

        // Po2C 实验开关（默认 weighted 走经典轮盘赌）
        if routing.selection == "power_of_two" && weighted.len() >= 2 {
            return self.power_of_two_pick(&weighted, strategy, routing.latency_ref_ms, rng);
        }

        let total_weight: f64 = weighted.iter().map(|(w, _)| w).sum();

        if total_weight <= 0.0 {
            let idx = rng.random_range(0..weighted.len());
            return RoutingDecision {
                vendor: weighted[idx].1 .0.clone(),
                model: weighted[idx].1 .1.clone(),
                strategy,
            };
        }

        let target = rng.random::<f64>() * total_weight;
        let mut cumulative = 0.0;

        for (weight, (vendor, model)) in weighted.iter() {
            cumulative += weight;
            if cumulative >= target {
                return RoutingDecision {
                    vendor: vendor.clone(),
                    model: model.clone(),
                    strategy,
                };
            }
        }

        // 兜底：返回最后一个
        if let Some((_, (vendor, model))) = weighted.last() {
            RoutingDecision {
                vendor: vendor.clone(),
                model: model.clone(),
                strategy,
            }
        } else {
            // 不应该发生，但提供默认值
            RoutingDecision {
                vendor: weighted[0].1 .0.clone(),
                model: weighted[0].1 .1.clone(),
                strategy,
            }
        }
    }

    /// power-of-two-choices — 均匀随机采样 2 个候选、按 EWMA
    /// 延迟择优（低者胜）。经典 herding 缓解：权重不再直接决定选择概率，
    /// 近期负载信号（EWMA 延迟）在 2 选 1 时生效。无 EWMA 数据的候选按
    /// 2×ref_ms 保守计（视为高延迟）；两者平局时回退平滑权重高者（保持
    /// 权重信号）。纯同步，采样经 rng 注入可复现。
    fn power_of_two_pick(
        &self,
        weighted: &[(f64, (VendorConfig, ModelConfig))],
        strategy: RoutingStrategy,
        latency_ref_ms: f64,
        rng: &mut dyn rand::Rng,
    ) -> RoutingDecision {
        let n = weighted.len();
        let i1 = rng.random_range(0..n);
        // 无放回第二样本：撞车则取相邻（n>=2 时必为另一候选）
        let mut i2 = rng.random_range(0..n);
        if i2 == i1 {
            i2 = (i1 + 1) % n;
        }

        let latency_score = |idx: usize| -> f64 {
            let (vendor, _) = &weighted[idx].1;
            match self.vendor_manager.metrics().vendor_ewma(&vendor.id) {
                Some((ms, _)) => ms,
                None => latency_ref_ms * 2.0,
            }
        };

        let (s1, s2) = (latency_score(i1), latency_score(i2));
        let chosen = if (s1 - s2).abs() < f64::EPSILON {
            // 平局：权重高者胜（全无数据时退化为按权重）
            if weighted[i1].0 >= weighted[i2].0 {
                i1
            } else {
                i2
            }
        } else if s1 < s2 {
            i1
        } else {
            i2
        };

        let (vendor, model) = weighted[chosen].1.clone();
        RoutingDecision {
            vendor,
            model,
            strategy,
        }
    }

    /// min+buffer 候选集：有 e2e 延迟数据的候选 ≥ 2 时，取
    /// ewma_e2e ≤ 最低×(1+buffer) 的集合内均匀随机；不足 2 个 → None 回落轮盘赌。
    /// 同时配置 Po2C 时本阶段优先（集合已含负载均衡语义）
    fn min_buffer_pick(
        &self,
        weighted: &[(f64, (VendorConfig, ModelConfig))],
        strategy: RoutingStrategy,
        buffer: f64,
        rng: &mut dyn rand::Rng,
    ) -> Option<RoutingDecision> {
        let metrics = self.vendor_manager.metrics();
        // 「有延迟数据」= e2e 道有样本；纯流式/纯失败史 vendor 不入判定
        let scored: Vec<(usize, f64)> = weighted
            .iter()
            .enumerate()
            .filter_map(|(i, (_, (v, _)))| metrics.vendor_ewma_e2e(&v.id).map(|e| (i, e)))
            .collect();
        if scored.len() < 2 {
            return None;
        }
        let lowest = scored.iter().map(|(_, e)| *e).fold(f64::INFINITY, f64::min);
        let threshold = lowest * (1.0 + buffer);
        let pool: Vec<usize> = scored
            .into_iter()
            .filter(|(_, e)| *e <= threshold)
            .map(|(i, _)| i)
            .collect();
        // lowest 自身恒 ≤ threshold，pool 非空（防御分支保底）
        if pool.is_empty() {
            return None;
        }
        let idx = pool[rng.random_range(0..pool.len())];
        let (vendor, model) = weighted[idx].1.clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy,
        })
    }

    async fn calculate_weight(&self, vendor: &VendorConfig, effective_model_weight: f64) -> f64 {
        // 系数热读：POST /admin/config 更新后立即生效
        let routing = self.config.read().routing.clone();
        // 负载维输入按 [routing].load_signal 单选
        let queue_len = self.queue_len_for(vendor, &routing.load_signal);
        let window = self.vendor_manager.get_window_info(&vendor.id).await;
        // EWMA 第 4 维输入
        let ewma = self.vendor_manager.metrics().vendor_ewma(&vendor.id);
        let weight = compute_weight(
            effective_model_weight,
            queue_len,
            window.as_ref(),
            &routing,
            ewma,
        );
        // 失败率软降权（门控默认关）
        self.failure_rate_penalty(&vendor.id, weight).await
    }

    /// 虚拟队列只读快照（键 = 租户或 "global"），测试与运维观测用
    pub fn sla_queue_snapshot(&self) -> HashMap<String, f64> {
        self.sla_queues
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 失败率软降权：failure_rate_gate 开启且样本达标且超阈时
    /// 权重 ×failure_rate_penalty；仅乘权重，不改变 Available 状态过滤
    async fn failure_rate_penalty(&self, vendor_id: &str, weight: f64) -> f64 {
        let resilience = self.config.read().resilience.clone();
        if !resilience.failure_rate_gate {
            return weight;
        }
        let (total, failures) = self
            .vendor_manager
            .metrics()
            .vendor_failure_window(vendor_id);
        if total < resilience.failure_rate_min_samples.max(1) {
            return weight; // 样本不足不惩罚（防小样本误杀）
        }
        let fail_pct = failures as f64 / total as f64;
        if fail_pct > resilience.failure_rate_threshold {
            weight * resilience.failure_rate_penalty
        } else {
            weight
        }
    }

    /// SLA 门控热读：sla_v > 0 且有限时开启返回 (V, α)；α 非有限按 0、
    /// 越界钳 [0,1]
    fn sla_gate(&self) -> Option<(f64, f64)> {
        let config = self.config.read();
        let v = config.routing.sla_v;
        if !v.is_finite() || v <= 0.0 {
            return None;
        }
        let alpha = if config.routing.sla_alpha.is_finite() {
            config.routing.sla_alpha.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some((v, alpha))
    }

    /// 满意度代理 ŝ = 1 − EWMA 错误率；无观测返回 1.0（冷启动不惩罚）
    fn satisfaction(&self, vendor_id: &str) -> f64 {
        match self.vendor_manager.metrics().vendor_ewma(vendor_id) {
            Some((_, ewma_err)) => (1.0 - ewma_err).clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    /// 候选集 Ŵ 前瞻：α = 60s 窗口总数/60，μ = 1000/ewma_e2e_ms，
    /// k = 并发上限（缺省 100）。ewma_e2e 含排队，低估 μ 高估 Ŵ，粗分类可接受；
    /// 无 e2e 数据 → Ŵ=0 中性
    fn lps_waits(&self, candidates: &[(VendorConfig, ModelConfig)]) -> Vec<f64> {
        let metrics = self.vendor_manager.metrics();
        candidates
            .iter()
            .map(|(v, _)| {
                let alpha = metrics.vendor_window_total(&v.id) as f64 / 60.0;
                let mu = metrics.vendor_ewma_e2e(&v.id).and_then(|ms| {
                    if ms > 0.0 {
                        Some(1000.0 / ms)
                    } else {
                        None
                    }
                });
                let k = v
                    .concurrency
                    .max_concurrent_requests
                    .map(|k| k as f64)
                    .unwrap_or(LPS_DEFAULT_CONCURRENCY);
                match mu {
                    Some(mu) => lps_wait(alpha, mu, k),
                    None => 0.0,
                }
            })
            .collect()
    }

    /// LPS 软约束上下文：lps_delta=0（默认）返回 None 零开销。
    /// 因子 = 1/(1 + δ·Ŵ/Ŵ_ref)，Ŵ_ref = 候选集内 Ŵ 中位数（乘法而非
    /// 线性负项：线性会触发 compute_weight 非正归零保护）
    fn lps_penalty_context(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
    ) -> Option<LpsPenalty> {
        let delta = {
            let config = self.config.read();
            let d = config.routing.lps_delta;
            if d.is_finite() && d > 0.0 {
                d
            } else {
                return None;
            }
        };
        let waits = self.lps_waits(candidates);
        // Ŵ_ref = 中位数
        let wa_ref = sla_median_cost(waits.clone()).unwrap_or(0.0);
        let factors = waits
            .into_iter()
            .map(|w| {
                if wa_ref <= 0.0 || !wa_ref.is_finite() {
                    1.0
                } else {
                    1.0 / (1.0 + delta * w / wa_ref)
                }
            })
            .collect();
        Some(LpsPenalty { factors })
    }

    /// LPS 硬约束门控热读：lps_deadline_gate（默认 false 不筛选）
    fn lps_deadline_gate_enabled(&self) -> bool {
        self.config.read().routing.lps_deadline_gate
    }

    /// deadline 可行集筛选：Ŵ·1000 ≤ deadline_ms（缺省 default_deadline_ms，
    /// 非法值不设限）
    fn lps_deadline_filter(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
        deadline_ms: Option<f64>,
    ) -> LpsDeadline {
        if !self.lps_deadline_gate_enabled() || candidates.is_empty() {
            return LpsDeadline::NoFilter;
        }
        let deadline =
            deadline_ms.unwrap_or_else(|| self.config.read().routing.default_deadline_ms);
        if !deadline.is_finite() || deadline <= 0.0 {
            return LpsDeadline::NoFilter; // 非法截止 = 不设限（不得误判全超时）
        }
        let waits = self.lps_waits(candidates);
        let feasible: Vec<usize> = (0..candidates.len())
            .filter(|&i| (waits[i] * 1000.0).is_finite() && waits[i] * 1000.0 <= deadline)
            .collect();
        if feasible.is_empty() {
            return LpsDeadline::EmptyPickMin; // 空集规则：调用方改选 Ŵ 最小者
        }
        if feasible.len() == candidates.len() {
            return LpsDeadline::NoFilter; // 全可行：不筛（保持原候选与权重路径）
        }
        LpsDeadline::Feasible(
            feasible
                .into_iter()
                .map(|i| candidates[i].clone())
                .collect(),
        )
    }

    /// 空可行集回退：改选 Ŵ 最小者（无候选达标也不拒绝服务）
    fn lps_min_wait_decision(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
        strategy: RoutingStrategy,
    ) -> Option<RoutingDecision> {
        if candidates.is_empty() {
            return None;
        }
        let waits = self.lps_waits(candidates);
        let min_i = waits
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)?;
        let (vendor, model) = candidates[min_i].clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy,
        })
    }

    /// SLA 打分上下文预收集：sla_v=0（默认）返回 None。缺价者记候选集
    /// 内已知单价中位数（保守不奖励）；全无定价时成本项置中性 0 并 warn 一次
    fn sla_penalty_context(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
        tenant: Option<&str>,
    ) -> Option<SlaPenalty> {
        let (v, alpha) = self.sla_gate()?;
        let config = self.config.read();
        let costs: Vec<Option<f64>> = candidates
            .iter()
            .map(|(vendor, model)| sla_unit_price(&config.pricing, &vendor.id, &model.model_id))
            .collect();
        drop(config);
        let known: Vec<f64> = costs.iter().flatten().copied().collect();
        let fallback = if known.is_empty() {
            if !self.sla_no_pricing_warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    candidates = candidates.len(),
                    "[routing].sla_v>0 but no candidate has a [[pricing]] entry; \
                     cost term neutralized to 0 (pure virtual-queue mode)"
                );
            }
            0.0
        } else {
            sla_median_cost(known).unwrap_or(0.0)
        };
        let queue_key = sla_queue_key(tenant);
        let q = self
            .sla_queues
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(queue_key)
            .copied()
            .unwrap_or(0.0);
        let entries = costs
            .into_iter()
            .zip(candidates.iter())
            .map(|(cost, (vendor, _))| (cost.unwrap_or(fallback), self.satisfaction(&vendor.id)))
            .collect();
        Some(SlaPenalty {
            v,
            alpha,
            entries,
            q,
        })
    }

    /// 选择完成后对偶更新 Q ← max(0, Q + α − ŝ_selected)：读-改-写
    /// 同一写锁内完成（短临界区无 await）；键 = 租户或 "global"
    fn sla_record_dual_update(&self, vendor_id: &str, tenant: Option<&str>, alpha: f64) {
        let satisfaction = self.satisfaction(vendor_id);
        let queue_key = sla_queue_key(tenant);
        let mut guard = self
            .sla_queues
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let q = guard.get(queue_key).copied().unwrap_or(0.0);
        guard.insert(
            queue_key.to_string(),
            sla_dual_update(q, alpha, satisfaction),
        );
    }
}

/// LPS 软约束的候选集级快照 — 因子与候选顺序对齐。
struct LpsPenalty {
    factors: Vec<f64>,
}

/// deadline 门控的筛选结果。
enum LpsDeadline {
    /// 不筛（gate 关 / deadline 非法 / 全候选可行）
    NoFilter,
    /// 可行集（非空、真子集）
    Feasible(Vec<(VendorConfig, ModelConfig)>),
    /// 全候选超时 → 空集规则（改选 Ŵ 最小者）
    EmptyPickMin,
}

/// SLA 打分的候选集级快照：整集共享同一 Q 与缺价兜底，保证可比
struct SlaPenalty {
    v: f64,
    alpha: f64,
    /// (Ĉ_m, ŝ_m)，与候选顺序对齐
    entries: Vec<(f64, f64)>,
    /// 虚拟队列 Q 快照（键 = sla_queue_key(tenant)）
    q: f64,
}

/// [[pricing]] 现查单候选的 Ĉ = (input+output)/2 每 1k 单价（负价钳 0）；
/// 缺价返回 None（调用方以候选集中位数兜底）
fn sla_unit_price(pricing: &[PricingConfig], vendor_id: &str, model_id: &str) -> Option<f64> {
    pricing
        .iter()
        .find(|p| p.vendor_id == vendor_id && p.model_id == model_id)
        .map(|p| ((p.input_per_1k_tokens + p.output_per_1k_tokens) / 2.0).max(0.0))
}

/// 虚拟队列键：租户哈希，无租户上下文时 "global"
fn sla_queue_key(tenant: Option<&str>) -> &str {
    tenant.unwrap_or("global")
}

/// 在途相对化到 queue_factor 输入域：(load − mean) / max(1, mean)
/// 线性映射到 0..100；≤ 均值 → 0，≥ 两倍均值 → 100。pub 供确定性单测
pub fn relative_in_flight_to_len(in_flight: f64, fleet_mean: f64) -> usize {
    let denom = fleet_mean.max(1.0);
    let relative = (in_flight - fleet_mean) / denom;
    (relative.clamp(0.0, 1.0) * 100.0).round() as usize
}

/// 亲和 boost 权重乘子（soft 模式）：命中 vendor ×(1 + factor)，
/// 其余不动；factor <= 0 无操作。pub 供确定性单测
pub fn apply_affinity_boost(
    weighted: &mut [(f64, (VendorConfig, ModelConfig))],
    vendor_id: &str,
    factor: f64,
) {
    if factor <= 0.0 {
        return;
    }
    for (w, (v, _)) in weighted.iter_mut() {
        if v.id == vendor_id {
            *w *= 1.0 + factor;
        }
    }
}

/// SLA 惩罚因子：1 / (1 + V·Ĉ + max(0, Q·(α − ŝ)))，因子 ∈ (0,1]；
/// ŝ ≥ α 时 Q 项钳 0 退化为纯成本项。pub 供确定性单测
pub fn sla_penalty_factor(v: f64, cost: f64, q: f64, alpha: f64, satisfaction: f64) -> f64 {
    let queue_term = (q * (alpha - satisfaction)).max(0.0);
    1.0 / (1.0 + v * cost + queue_term)
}

/// 虚拟队列对偶更新 Q ← max(0, Q + α − ŝ)：服务欠账累积、达标清账钳 0。
/// pub 供确定性单测
pub fn sla_dual_update(q: f64, alpha: f64, satisfaction: f64) -> f64 {
    (q + alpha - satisfaction).max(0.0)
}

/// LPS Ŵ 截断上限：ρ→1 时式 10 发散；候选集内相对归一下 CAP 只影响发散者自身
pub const LPS_WAIT_CAP: f64 = 1e6;

/// 并发上限缺省：max_concurrent_requests 缺省时与 manager 队列容量同源（100）
const LPS_DEFAULT_CONCURRENCY: f64 = 100.0;

/// LPS 平均等待估计：Ŵ = (α/μ)^k / (μ−α)，ρ ≥ 1（超载）返回 CAP。
/// 非法输入 fail-safe：α 非法取 0，μ 无效返回 CAP，k<1 钳 1。pub 供确定性单测
pub fn lps_wait(arrival_per_sec: f64, service_per_sec: f64, concurrency: f64) -> f64 {
    // 防御：α 负/非有限按空闲（零等待）；μ 无效与 k<1 按超载方向 fail-safe
    let alpha = if arrival_per_sec.is_finite() && arrival_per_sec > 0.0 {
        arrival_per_sec
    } else {
        return 0.0;
    };
    let mu = if service_per_sec.is_finite() && service_per_sec > 0.0 {
        service_per_sec
    } else {
        return LPS_WAIT_CAP;
    };
    let k = if concurrency.is_finite() && concurrency >= 1.0 {
        concurrency
    } else {
        1.0
    };
    let rho = alpha / (mu * k);
    if !rho.is_finite() || rho >= 1.0 {
        return LPS_WAIT_CAP; // 式 10 分母 μ−α ≤ 0（表发散）
    }
    (alpha / mu).powf(k) / (mu - alpha)
}

/// 候选集已知单价中位数：缺价候选的 Ĉ 兜底（保守不奖励）；
/// 空集 None（调用方把成本项整体置中性 0）
pub fn sla_median_cost(mut prices: Vec<f64>) -> Option<f64> {
    if prices.is_empty() {
        return None;
    }
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = prices.len() / 2;
    Some(if prices.len() % 2 == 1 {
        prices[mid]
    } else {
        (prices[mid - 1] + prices[mid]) / 2.0
    })
}

/// 路由权重：w_model×model.weight + w_queue×queue_factor +
/// w_window×window_factor + w_latency×latency_factor + w_error×error_factor。
/// 各因子语义见 compute_weight_breakdown；纯因子加权，不含温度平滑
///（平滑由 weighted_select_with 恰好应用一次）。pub 供确定性测试与调试
pub fn compute_weight(
    model_weight: f64,
    queue_len: usize,
    window: Option<&WindowInfo>,
    routing: &crate::config::RoutingSection,
    ewma: Option<(f64, f64)>,
) -> f64 {
    compute_weight_breakdown(model_weight, queue_len, window, routing, ewma).total
}

/// 5 因子权重分解：compute_weight 加法项的逐项贡献 + 合成（total）
pub fn compute_weight_breakdown(
    model_weight: f64,
    queue_len: usize,
    window: Option<&WindowInfo>,
    routing: &crate::config::RoutingSection,
    ewma: Option<(f64, f64)>,
) -> WeightBreakdown {
    let model_component = model_weight * routing.w_model;
    let queue_factor = queue_factor_from_len(queue_len);
    let window_factor = window_factor_from_info(window);
    // 无 EWMA 数据：延迟中性 0.5（等价 ref/(ref+ref)），错误中性 1.0
    let (latency_factor, error_factor) = match ewma {
        Some((ewma_ms, ewma_err)) => (
            latency_factor_from_ewma(ewma_ms, routing.latency_ref_ms),
            (1.0 - ewma_err).clamp(0.0, 1.0),
        ),
        None => (0.5, 1.0),
    };
    let breakdown = WeightBreakdown {
        model: model_component,
        queue: queue_factor * routing.w_queue,
        window: window_factor * routing.w_window,
        latency: latency_factor * routing.w_latency,
        error: error_factor * routing.w_error,
        total: 0.0,
    };
    let weight =
        breakdown.model + breakdown.queue + breakdown.window + breakdown.latency + breakdown.error;
    WeightBreakdown {
        total: if weight.is_finite() && weight > 0.0 {
            weight
        } else {
            0.0
        },
        ..breakdown
    }
}

/// 5 因子权重分解（`compute_weight` 加法项逐项贡献 + 合成 total）。
#[derive(Debug, Clone, PartialEq)]
pub struct WeightBreakdown {
    pub model: f64,
    pub queue: f64,
    pub window: f64,
    pub latency: f64,
    pub error: f64,
    pub total: f64,
}

fn routing_pair_enabled(router: &Router) -> bool {
    router.config.read().routing.prefix_affinity.enabled
}

/// 单请求路由上下文：前缀亲和与价格因子的请求级输入
///（prefix_hash = API 层对稳定前缀的进程内哈希；est_prompt_tokens = body 字节/4 估算）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteCtx {
    pub prefix_hash: u64,
    pub est_prompt_tokens: u64,
    /// N1：X-Batch 请求标记——带内收缩到 supports_batch 厂商（无则忽略）
    pub batch: bool,
}

/// 前缀键纯函数：system + tools 序列化 + 首条 user content 头部 prefix_chars 字符。
/// 进程内一致性哈希（DefaultHasher）——与亲和表同口径，不跨进程
pub fn prefix_hash_key(body: &serde_json::Value, prefix_chars: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let obj = body.as_object();
    let system = obj
        .and_then(|o| o.get("system"))
        .map(|v| v.to_string())
        .unwrap_or_default();
    system.hash(&mut h);
    let tools = obj
        .and_then(|o| o.get("tools"))
        .map(|v| v.to_string())
        .unwrap_or_default();
    tools.hash(&mut h);
    let first_user = obj
        .and_then(|o| o.get("messages"))
        .and_then(|m| m.as_array())
        .and_then(|a| {
            a.iter()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let head: String = first_user.chars().take(prefix_chars.max(1)).collect();
    head.hash(&mut h);
    h.finish()
}

/// 前缀亲和候选对（DualMap 双映射 PoTC 网关形态）：同 hash 恒定同对，
/// 相等则邻位错开；n<2 返回 None（无可选空间）
pub fn prefix_candidates(hash: u64, n: usize) -> Option<(usize, usize)> {
    if n < 2 {
        return None;
    }
    let i1 = (hash % n as u64) as usize;
    let mixed = hash ^ (hash >> 31);
    let mixed = mixed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let i2 = ((mixed >> 17) % n as u64) as usize;
    if i1 == i2 {
        Some((i1, (i1 + 1) % n))
    } else {
        Some((i1, i2))
    }
}

/// 路由解释 reason_code 稳定机器码（客户端可匹配，不做 i18n）。
pub const REASON_DISABLED: &str = "disabled";
pub const REASON_COOLING_DOWN: &str = "cooling_down";
pub const REASON_THROTTLED: &str = "throttled";
pub const REASON_NOT_IN_CATALOG: &str = "not_in_catalog";
pub const REASON_WEIGHT_ZERO: &str = "weight_zero";
pub const REASON_AFFINITY_BOUND_ELSEWHERE: &str = "affinity_bound_elsewhere";
pub const REASON_QUEUE_OVER_LIMIT: &str = "queue_over_limit";
pub const REASON_ELIGIBLE: &str = "eligible";

/// 队列饱和点（queue_factor 归零点），达到即 queue_over_limit 诊断性排除
pub const QUEUE_SATURATION_LEN: usize = 100;

/// 候选行评估结果（`Router::explain_route` 行数据；handler 侧转 JSON）。
#[derive(Debug, Clone)]
pub struct CandidateAssessment {
    pub vendor_id: String,
    pub model_id: String,
    pub eligible: bool,
    pub reason_code: &'static str,
    pub detail: String,
    /// 入选者 5 因子分解（排除项 None）。
    pub factors: Option<WeightBreakdown>,
    /// 分解合成（`factors` 加和；排除项 None）。
    pub base_weight: Option<f64>,
    /// 生产精确权重（与路由同一方法；默认配置下 == base_weight）。
    pub weight: Option<f64>,
    /// 当前 selection 档位（入选者有值）。
    pub selection: Option<&'static str>,
}

impl CandidateAssessment {
    fn excluded(
        vendor_id: String,
        model_id: String,
        reason_code: &'static str,
        detail: String,
    ) -> Self {
        Self {
            vendor_id,
            model_id,
            eligible: false,
            reason_code,
            detail,
            factors: None,
            base_weight: None,
            weight: None,
            selection: None,
        }
    }

    fn eligible(
        vendor_id: String,
        model_id: String,
        factors: WeightBreakdown,
        base_weight: f64,
        weight: f64,
        selection: &'static str,
    ) -> Self {
        Self {
            vendor_id,
            model_id,
            eligible: true,
            reason_code: REASON_ELIGIBLE,
            detail: t_args(
                "router-explain-eligible",
                &[("selection", selection.to_string())],
            ),
            factors: Some(factors),
            base_weight: Some(base_weight),
            weight: Some(weight),
            selection: Some(selection),
        }
    }
}

/// 当前 selection 档位：linucb 大小写不敏感，power_of_two 精确匹配，其余加权
fn selection_tier(routing: &crate::config::RoutingSection) -> &'static str {
    if routing.selection.trim().eq_ignore_ascii_case("linucb") {
        "linucb"
    } else if routing.selection == "power_of_two" {
        "power_of_two"
    } else {
        "weighted"
    }
}

/// 有效模型权重解析（四级回退第 2/3 级；请求覆盖由调用方先行处理）
fn resolve_effective_model_weight(model_weight: f64, capacity_share: Option<f64>) -> f64 {
    if model_weight > 0.0 {
        model_weight
    } else {
        capacity_share.unwrap_or(1.0)
    }
}

/// latency_factor = ref / (ref + ewma_ms) — 有界，
/// ewma == ref 时 0.5，ewma 越小得分越高。ref 配置非法（<=0）时中性 0.5。
fn latency_factor_from_ewma(ewma_ms: f64, ref_ms: f64) -> f64 {
    if !ref_ms.is_finite() || ref_ms <= 0.0 {
        return 0.5;
    }
    let denom = ref_ms + ewma_ms.max(0.0);
    ref_ms / denom
}

/// 温度平滑 w^(1/T) — herding 缓解（权重高的厂商被过度选中后
/// 延迟上升，温度压平差距给长尾厂商探索流量）。T=1 恒等；T>1 压平；T<1
/// 放大。T 非法（<=0/非有限）按恒等处理；w<=0 保持 0。
fn temperature_smooth(weight: f64, temperature: f64) -> f64 {
    if !temperature.is_finite() || temperature <= 0.0 || (temperature - 1.0).abs() < 1e-12 {
        return weight;
    }
    if weight <= 0.0 {
        return weight;
    }
    weight.powf(1.0 / temperature)
}

/// 队列因子：长度 0 → 1.0，长度 ≥ 100 → 0.0
fn queue_factor_from_len(queue_len: usize) -> f64 {
    1.0 - (queue_len as f64 / 100.0).min(1.0)
}

/// 窗口因子：
/// - None（无窗口信息）→ 0.5（中性）
/// - 预算耗尽 → 0.0
/// - 窗口即将结束 → budget_percentage × 0.5（惩罚）
/// - 正常 → budget_percentage
fn window_factor_from_info(window: Option<&WindowInfo>) -> f64 {
    match window {
        None => 0.5,
        Some(info) => {
            if !info.has_budget() {
                0.0
            } else if info.is_ending_soon() {
                info.budget_percentage() * 0.5
            } else {
                info.budget_percentage()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoutingSection;
    use std::time::{Duration, Instant};

    fn window(max: Option<u64>, used: u64, end_offset_secs: Option<u64>) -> WindowInfo {
        WindowInfo {
            max_tokens: max,
            used_tokens: used,
            window_end: end_offset_secs.map(|s| Instant::now() + Duration::from_secs(s)),
        }
    }

    /// 历史默认系数（0.4/0.3/0.3）下的 3D 权重
    fn compute_weight_3d(model_weight: f64, queue_len: usize, window: Option<&WindowInfo>) -> f64 {
        compute_weight(
            model_weight,
            queue_len,
            window,
            &RoutingSection::default(),
            None,
        )
    }

    #[test]
    fn test_queue_factor_decreases_with_length() {
        assert_eq!(queue_factor_from_len(0), 1.0);
        assert!((queue_factor_from_len(50) - 0.5).abs() < 1e-9);
        assert_eq!(queue_factor_from_len(100), 0.0);
        assert_eq!(queue_factor_from_len(500), 0.0);
    }

    #[test]
    fn test_window_factor_none_is_neutral() {
        assert!((window_factor_from_info(None) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_window_factor_no_budget_returns_zero() {
        let w = window(Some(100), 100, None);
        assert!(!w.has_budget());
        assert_eq!(window_factor_from_info(Some(&w)), 0.0);

        let w_over = window(Some(100), 200, None);
        assert!(!w_over.has_budget());
        assert_eq!(window_factor_from_info(Some(&w_over)), 0.0);
    }

    #[test]
    fn test_window_factor_full_budget_no_window_end() {
        // max=None → unlimited → budget_percentage=1.0, is_ending_soon=false
        let w = window(None, 0, None);
        assert!(w.has_budget());
        assert!(!w.is_ending_soon());
        assert!((window_factor_from_info(Some(&w)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_window_factor_partial_budget() {
        // used=50, max=100 → budget_percentage = 1 - 50/100 = 0.5
        let w = window(Some(100), 50, Some(600));
        assert!(w.has_budget());
        assert!(!w.is_ending_soon()); // 600s > 300s threshold
        assert!((window_factor_from_info(Some(&w)) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_window_factor_ending_soon_halves_budget() {
        // 剩余时间 < 300s → is_ending_soon=true → factor = budget_percentage × 0.5
        let w = window(Some(100), 50, Some(60)); // 60s 剩余
        assert!(w.is_ending_soon());
        let expected = 0.5 * 0.5; // budget 0.5 × 0.5 penalty
        assert!((window_factor_from_info(Some(&w)) - expected).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_default_full_budget() {
        // model.weight=1.0, queue=0, window=None
        // = 1.0×0.4 + 1.0×0.3 + 0.5×0.3 = 0.4 + 0.3 + 0.15 = 0.85
        let w = compute_weight_3d(1.0, 0, None);
        assert!((w - 0.85).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_high_model_weight_dominates() {
        // model.weight=2.0, queue=0, window=None
        // = 2.0×0.4 + 1.0×0.3 + 0.5×0.3 = 0.8 + 0.3 + 0.15 = 1.25
        let w = compute_weight_3d(2.0, 0, None);
        assert!((w - 1.25).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_exhausted_budget_lowers_weight() {
        let w_full = compute_weight_3d(1.0, 0, None);
        let exhausted = window(Some(100), 100, None);
        let w_exhausted = compute_weight_3d(1.0, 0, Some(&exhausted));
        // 预算耗尽 → window_factor=0 → 0.4 + 0.3 + 0 = 0.7
        assert!((w_exhausted - 0.7).abs() < 1e-9);
        assert!(w_exhausted < w_full);
    }

    #[test]
    fn test_compute_weight_long_queue_lowers_weight() {
        let w_short = compute_weight_3d(1.0, 0, None);
        let w_long = compute_weight_3d(1.0, 100, None);
        // queue=100 → queue_factor=0 → 0.4 + 0 + 0.15 = 0.55
        assert!((w_long - 0.55).abs() < 1e-9);
        assert!(w_long < w_short);
    }

    #[test]
    fn test_compute_weight_zero_model_weight_keeps_minimum() {
        // model.weight=0, queue=0, window=None
        // = 0 + 0.3 + 0.15 = 0.45 (非零，不归零)
        let w = compute_weight_3d(0.0, 0, None);
        assert!((w - 0.45).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_negative_inputs_clamped_to_zero() {
        // model.weight = -1.0 (异常配置)
        // = -0.4 + 0.3 + 0.15 = 0.05 → > 0 保留
        let w = compute_weight_3d(-1.0, 0, None);
        assert!((w - 0.05).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_all_zero_when_exhausted_and_queued() {
        // model.weight=0, queue=100, budget exhausted
        // = 0 + 0 + 0 = 0
        let exhausted = window(Some(100), 100, None);
        let w = compute_weight_3d(0.0, 100, Some(&exhausted));
        assert_eq!(w, 0.0);
    }

    // ======== in_flight_relative 相对化纯函数 ========

    /// relative_load (x−mean)/max(1,mean) 映射到 0..100，mean=0 分母钳 1 兜底
    #[test]
    fn relative_in_flight_maps_to_queue_len_domain() {
        // 高于均值：x=3, mean=2 → (3−2)/2 = 0.5 → 50
        assert_eq!(relative_in_flight_to_len(3.0, 2.0), 50);
        // 饱和：x=5, mean=2 → 1.5 → clamp 1.0 → 100
        assert_eq!(relative_in_flight_to_len(5.0, 2.0), 100);
        // 恰两倍均值 → 100（queue_factor 触底）
        assert_eq!(relative_in_flight_to_len(4.0, 2.0), 100);
        // 低于均值 → 负相对负载 → 0（全空闲分）
        assert_eq!(relative_in_flight_to_len(1.0, 2.0), 0);
        assert_eq!(relative_in_flight_to_len(0.0, 2.0), 0);
        // 等于均值 → 0
        assert_eq!(relative_in_flight_to_len(2.0, 2.0), 0);
        // 近空闲舰队：mean=0 → 分母钳 1，x=0 → 0（无噪声放大）
        assert_eq!(relative_in_flight_to_len(0.0, 0.0), 0);
        // mean=0 而 x=1（单点忙）：(1−0)/1 = 1 → 100
        assert_eq!(relative_in_flight_to_len(1.0, 0.0), 100);
    }

    // ======== SLA 虚拟队列打分纯函数 ========

    /// 惩罚因子公式：1/(1 + V·Ĉ + max(0, Q·(α−ŝ)))；分母 ≥ 1 → 因子 ≤ 1。
    #[test]
    fn sla_penalty_factor_matches_formula() {
        // Q=0 → 纯成本项：1/(1+0.05)
        assert!((sla_penalty_factor(1.0, 0.05, 0.0, 0.95, 0.5) - 1.0 / 1.05).abs() < 1e-9);
        // α>ŝ 且 Q>0 → Q 项进分母：1/(1+0.05+0.95×0.45)
        assert!((sla_penalty_factor(1.0, 0.05, 0.95, 0.95, 0.5) - 1.0 / 1.4775).abs() < 1e-9);
        // ŝ ≥ α → max(0,·) 钳 0，Q 再大也不进分母
        assert_eq!(sla_penalty_factor(1.0, 0.05, 1e9, 0.95, 0.95), 1.0 / 1.05);
    }

    /// 惩罚单调性：同 ŝ 下 Ĉ 高者因子低；Q 增大压低欠账（ŝ<α）vendor 的
    /// 因子；达标（ŝ≥α）vendor 不受 Q 影响；全零输入 → 恒等 1.0。
    #[test]
    fn sla_penalty_factor_monotonicity() {
        let (v, alpha, s) = (1.0, 0.95, 0.8);
        // 同满意度下成本高 → 因子低
        assert!(
            sla_penalty_factor(v, 0.09, 0.5, alpha, s) < sla_penalty_factor(v, 0.01, 0.5, alpha, s)
        );
        // Q 增大 → 欠账 vendor 被压
        assert!(
            sla_penalty_factor(v, 0.05, 2.0, alpha, s) < sla_penalty_factor(v, 0.05, 0.0, alpha, s)
        );
        // 满意度达标（ŝ≥α）→ Q 增大不影响
        assert_eq!(
            sla_penalty_factor(v, 0.05, 0.0, alpha, 0.99),
            sla_penalty_factor(v, 0.05, 100.0, alpha, 0.99)
        );
        // 无成本无欠账 → 恒等
        assert_eq!(sla_penalty_factor(v, 0.0, 0.0, alpha, 1.0), 1.0);
    }

    /// 对偶更新钳位：α>ŝ 累积；α=ŝ 不变；α<ŝ 递减、清账后钳 0 不越负。
    #[test]
    fn sla_dual_update_clamps_at_zero() {
        // α>ŝ：Q 累积 0+0.95−0.5 = 0.45
        assert!((sla_dual_update(0.0, 0.95, 0.5) - 0.45).abs() < 1e-9);
        // α=ŝ：Q 不变
        assert!((sla_dual_update(0.3, 0.95, 0.95) - 0.3).abs() < 1e-9);
        // α<ŝ 且盈余覆盖存量（ŝ−α=0.05 > q=0.02）：钳 0（不越负）
        assert_eq!(sla_dual_update(0.02, 0.95, 1.0), 0.0);
        // α<ŝ 但存量欠账大：递减不为负
        assert!((sla_dual_update(5.0, 0.95, 1.0) - 4.95).abs() < 1e-9);
        // 持续达标（ŝ=1）→ 收敛归 0
        assert_eq!(sla_dual_update(0.05, 0.95, 1.0), 0.0);
    }

    // ======== LPS 前瞻纯函数 ========

    /// 已知值：α=2, μ=4, k=2 → ρ=0.25<1，Ŵ=(0.5)²/(4−2)=0.125。
    #[test]
    fn lps_wait_known_value() {
        assert!((lps_wait(2.0, 4.0, 2.0) - 0.125).abs() < 1e-12);
    }

    /// ρ→1 与超载发散：ρ=1 / ρ>1 / μ 非法 → CAP（截断必须有）。
    #[test]
    fn lps_wait_diverges_at_rho_one() {
        assert_eq!(lps_wait(4.0, 2.0, 2.0), LPS_WAIT_CAP); // ρ=1
        assert_eq!(lps_wait(5.0, 2.0, 2.0), LPS_WAIT_CAP); // ρ=1.25
        assert_eq!(lps_wait(1.0, 0.0, 1.0), LPS_WAIT_CAP); // μ 非法
        assert_eq!(lps_wait(1.0, f64::NAN, 1.0), LPS_WAIT_CAP);
    }

    /// k=1 退化为 M/M/1 队列等待 Wq = ρ/(μ−α)。
    #[test]
    fn lps_wait_reduces_to_mm1() {
        assert!((lps_wait(1.0, 2.0, 1.0) - 0.5).abs() < 1e-12);
        assert!((lps_wait(3.0, 4.0, 1.0) - 0.75).abs() < 1e-12);
    }

    /// 空闲零等待与 k 防御：α=0 → 0；k<1 钳 1（等价 k=1）。
    #[test]
    fn lps_wait_zero_arrival_and_k_clamp() {
        assert_eq!(lps_wait(0.0, 4.0, 2.0), 0.0);
        assert_eq!(lps_wait(1.0, 2.0, 0.0), lps_wait(1.0, 2.0, 1.0));
        assert_eq!(lps_wait(-1.0, 2.0, 1.0), 0.0, "负到达率按空闲处理");
    }

    /// 缺价中位数兜底：奇数取中、偶数取中间均值、无序可排、空集 None。
    #[test]
    fn sla_median_cost_odd_even_empty() {
        assert!(sla_median_cost(vec![]).is_none());
        assert!((sla_median_cost(vec![0.09, 0.01, 0.05]).unwrap() - 0.05).abs() < 1e-9);
        assert!((sla_median_cost(vec![0.09, 0.01]).unwrap() - 0.05).abs() < 1e-9);
        // 无序输入 + 偶数均值
        assert!((sla_median_cost(vec![0.5, 0.1, 0.9, 0.3]).unwrap() - 0.4).abs() < 1e-9);
    }
}
