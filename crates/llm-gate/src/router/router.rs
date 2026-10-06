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
    /// recId=11：会话亲和表（oxcache）。None = 未接线，路由行为与历史一致
    affinity: Option<Arc<AffinityTable>>,
    /// recId=14：SLA 虚拟队列 Q — 键 = 租户（调用方 API key 哈希；无租户
    /// 上下文时 "global"）。sla_v > 0 时每轮加权选择完成后对偶更新
    /// `Q ← max(0, Q + α − ŝ_selected)`（决策时更新，无需执行结果回传）；
    /// sla_v = 0（默认）不读不写、行为与现状逐位一致。重启归零（Lyapunov
    /// 收敛自动重建）。std RwLock：临界区无 await，与 metrics 同款。
    sla_queues: Arc<RwLock<HashMap<String, f64>>>,
    /// recId=14：「候选集全无定价 → 成本项置中性 0」的进程内一次性 warn
    /// 标记（防每请求刷屏）。
    sla_no_pricing_warned: Arc<AtomicBool>,
    /// recId=15：LinUCB 状态 — selection="linucb" 档的臂状态来源。None =
    /// 未接线（该档回退 weighted，安全语义对齐 affinity 的 None）。
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

    /// recId=15：接线 LinUCB 状态（builder，`with_affinity` 同款）。接线后
    /// `[routing].selection = "linucb"` 档在加权选择路径生效；未接线时该档
    /// 回退 weighted（行为与历史一致）。
    pub fn with_bandit(mut self, bandit: Arc<BanditState>) -> Self {
        self.bandit = Some(bandit);
        self
    }

    /// recId=11：接线会话亲和表（builder）。接线后 `route_with_session`
    /// 的合格集合内亲和命中者优先（boost 而非硬粘）。
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

    /// recId=11：带会话键的路由 — 合格集合（enabled + Available）内亲和
    /// 命中者优先；未命中走 weighted_select。绑定回写不在此处：只记
    /// "上次成功服务的 vendor"，执行成功后由 API 层 `AffinityTable::bind`。
    pub async fn route_with_session(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
    ) -> Result<RoutingDecision, GateError> {
        self.route_with_session_excluding(abstract_model, session_key, &[])
            .await
    }

    /// recId=11：带会话键 + 排除集的路由（失败迁移重路由的亲和形态）
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
        )
        .await
    }

    /// recId=19(a)：带请求级权重的路由变体（X-Weight，LiteLLM simple_shuffle
    /// _router_weights 对照）— `request_weight` 经 `[routing].allow_request_weight`
    /// 门控（关闭时忽略，行为与现状逐位一致）后覆盖 model.weight 参与
    /// 权重计算。`Router::route` 等原签名与行为不变。
    /// recId=10(2)：`tenant` 为调用方 API key 哈希（cache_api_key_hash
    /// 同源），仅 `[routing].affinity_tenant_scoped` 开启时参与亲和键。
    #[allow(clippy::too_many_arguments)] // 路由上下文穿线（log_chat_request 同款先例）
    pub async fn route_weighted(
        &self,
        abstract_model: &str,
        session_key: Option<&str>,
        tenant: Option<&str>,
        excluded: &[String],
        request_weight: Option<f64>,
        // recId=15：LinUCB 请求特征（`linucb_features`，5 维）。None 或
        // selection 非 linucb 档时该维度不参与（行为与历史一致）。
        linucb_features: Option<&[f64]>,
        // recId=17：x-deadline-ms 头（毫秒）。仅 [routing].lps_deadline_gate
        // 开启时被消费（缺省用 default_deadline_ms）；默认关闭时本参数
        // 不参与任何路径（行为与历史一致）。
        lps_deadline_ms: Option<f64>,
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
        )
        .await
    }

    /// recId=19(a)：请求级权重门控 — `[routing].allow_request_weight` 开启
    /// 时透传，关闭（默认）丢弃为 None。`pub` 供确定性单测
    ///（tests/router_test.rs；compute_weight 同款先例）。
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
    ) -> Result<RoutingDecision, GateError> {
        // recId=14：SLA 虚拟队列键使用**原始**租户 —— affinity_tenant_scoped
        // 是亲和键格式兼容开关（recId=10(2)），只约束亲和 lookup，不约束
        // SLA 分桶（键 = 租户 hash，无租户上下文 "global"）。
        let sla_tenant = tenant;
        // recId=10(2)：键租户隔离门控 — 关闭（默认）一律 legacy 键（行为
        // 与键格式逐位一致）
        let tenant = if self.config.read().routing.affinity_tenant_scoped {
            tenant
        } else {
            None
        };
        // recId=15（评审修复 major）：臂键归一经 bandit::linucb_arm_key
        // （lowercase + auto→"__global__"），与奖励侧同源（DRY）
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
            if let Some(decision) = self
                .try_global_route(
                    excluded,
                    session_key,
                    tenant,
                    sla_tenant,
                    request_weight,
                    linucb_features,
                    lps_deadline_ms,
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

    /// recId=7⑤：按 config vendors 的 priority 字段构建 abstract_model 的
    /// 换路 fallback 链（resilience feature）。
    ///
    /// targets = `resolve_model` 的 enabled 候选（abstract_id 精确匹配），
    /// priority 数值越小越优先；trigger=All（耗尽/429/5xx/timeout 均换路）。
    /// 无候选（如 direct 形式 "vendor.model" 不在 abstract 表中）返回 None，
    /// API 层据此保持单发语义。
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

    /// 使用路由提示进行路由（recId=19(a)：新增请求级权重入参——vendor 提示
    /// 路径为单候选无权重面，权重仅在 model 提示 / 默认路由路径生效；
    /// recId=10(2)：tenant 亲和键租户维度同源传入）
    #[allow(clippy::too_many_arguments)] // 路由上下文穿线（route_weighted 同款）
    pub async fn route_with_hints(
        &self,
        abstract_model: &str,
        preferred_vendor: Option<&str>,
        preferred_model: Option<&str>,
        request_weight: Option<f64>,
        tenant: Option<&str>,
        // recId=15：LinUCB 请求特征（语义同 [`Self::route_weighted`]）
        linucb_features: Option<&[f64]>,
        // recId=17：x-deadline-ms 头（语义同 [`Self::route_weighted`]）
        lps_deadline_ms: Option<f64>,
    ) -> Result<RoutingDecision, GateError> {
        // recId=19(a) 评审修复：preferred_model 分支此前把 request_weight
        // 裸传 try_abstract_route，绕过 allow_request_weight 门控（X-Model
        // + X-Weight 在门控关闭时仍覆盖 model.weight）。入口统一过门——
        // 末尾 route_weighted 的二次门控幂等（None 穿透、开关语义不变）。
        let request_weight = self.gate_request_weight(request_weight);
        // 如果指定了厂商，尝试直接路由到该厂商
        if let Some(vendor_id) = preferred_vendor {
            if let Some(decision) = self.try_vendor_route(vendor_id, preferred_model).await {
                return Ok(decision);
            }
            // 如果指定的厂商不可用或找不到，返回错误
            return Err(GateError::Validation {
                field: t_args(
                    "router-vendor-unavailable",
                    &[("vendor", vendor_id.to_string())],
                ),
            });
        }

        // 如果指定了模型，优先使用该模型（hints 是显式指定，不接亲和）
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
                )
                .await
            {
                return Ok(decision);
            }
        }

        // 默认路由逻辑
        self.route_weighted(
            abstract_model,
            None,
            tenant,
            &[],
            request_weight,
            linucb_features,
            lps_deadline_ms,
        )
        .await
    }

    async fn try_vendor_route(
        &self,
        vendor_id: &str,
        model_hint: Option<&str>,
    ) -> Option<RoutingDecision> {
        let vendor = self.config.get_vendor(vendor_id)?;
        if !vendor.enabled {
            return None;
        }

        // 检查厂商状态
        if self.vendor_manager.get_status(vendor_id).await != VendorStatus::Available {
            return None;
        }

        // 如果有模型提示，查找匹配的模型
        if let Some(model_id) = model_hint {
            let normalized = model_id.to_lowercase();
            if let Some(model) = vendor
                .models
                .iter()
                .find(|m| {
                    m.enabled
                        && (m.model_id.to_lowercase() == normalized
                            || m.abstract_id
                                .as_ref()
                                .is_some_and(|a| a.to_lowercase() == normalized))
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

        // 默认使用第一个启用的模型
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

        if excluded.iter().any(|e| e == vendor_id) {
            return None;
        }

        let vendor = self.config.get_vendor(vendor_id)?;
        if !vendor.enabled {
            return None;
        }

        let model = vendor
            .models
            .iter()
            .find(|m| {
                m.enabled
                    && (m.model_id == model_id
                        || m.abstract_id.as_ref().is_some_and(|a| a == model_id))
            })?
            .clone();

        Some(RoutingDecision {
            vendor,
            model,
            strategy: RoutingStrategy::Direct,
        })
    }

    #[allow(clippy::too_many_arguments)] // 路由上下文穿线（log_chat_request 同款先例）
    async fn try_abstract_route(
        &self,
        abstract_id: &str,
        excluded: &[String],
        session_key: Option<&str>,
        tenant: Option<&str>,
        // recId=14：SLA 虚拟队列键用原始租户（上面的 `tenant` 已过亲和
        // 键门控，供 affinity lookup；SLA Q 分桶用本参数，不经门控）
        sla_tenant: Option<&str>,
        request_weight: Option<f64>,
        // recId=15：LinUCB 请求特征（selection="linucb" 档选择输入）
        linucb_features: Option<&[f64]>,
        // recId=17：x-deadline-ms 头（lps_deadline_gate 开启时消费）
        lps_deadline_ms: Option<f64>,
    ) -> Option<RoutingDecision> {
        let candidates = self.config.resolve_model(abstract_id);

        // recId=5：get_status async 化（Token 窗口 peek），filter 闭包改 for 循环
        let mut available_candidates: Vec<(VendorConfig, ModelConfig)> = Vec::new();
        // recId=19(b)：因 CB/Throttled 被剔除的候选（fail-open 回落集）
        let mut status_dropped: Vec<(VendorConfig, ModelConfig)> = Vec::new();
        for (vendor, model) in candidates {
            // Only consider enabled vendors and models
            if !vendor.enabled || !model.enabled {
                continue;
            }
            // recId=7⑤：换路 fallback 的排除集
            if excluded.iter().any(|e| e == &vendor.id) {
                continue;
            }
            if self.vendor_manager.get_status(&vendor.id).await == VendorStatus::Available {
                available_candidates.push((vendor, model));
            } else {
                status_dropped.push((vendor, model));
            }
        }

        if available_candidates.is_empty() {
            // recId=19(b)：全冷却 fail-open 安全网 — 全部候选因 CB Open /
            // Throttled 被剔除且 `[routing].fail_open_when_all_cooling`
            // 开启时，回落「忽略状态但 enabled」的候选集加权选择。现实
            // 约束：NexusFlow 无主动健康检查驱动的冷却，全部为真实失败
            // 冷却——本开关覆盖的是 CB Open 超时未过窗口 / 限流窗口未
            // 重置，语义弱于 LiteLLM 的 health-check 限定；把已冷却
            // vendor 重新纳入流量是有意冒险，仅建议有兜底容量的部署开启。
            // 默认关闭 = None（上层收敛为 Validation 错误，现状不变）。
            if !status_dropped.is_empty() && self.config.read().routing.fail_open_when_all_cooling {
                tracing::warn!(
                    count = status_dropped.len(),
                    "{}",
                    t("router-fail-open-engaged")
                );
                return Some(
                    self.weighted_select(
                        status_dropped,
                        RoutingStrategy::Abstract,
                        request_weight,
                        None,
                        sla_tenant,
                    )
                    .await,
                );
            }
            return None;
        }

        // recId=10(1)：亲和 —— hard（默认）命中即返回（现状）；soft 命中
        // 软化为评分项（权重乘 (1+factor) 后与其余候选照常竞争）
        if let Some(bound) = self
            .affinity_hit(&available_candidates, session_key, tenant)
            .await
        {
            let routing = self.config.read().routing.clone();
            if routing.affinity_mode.trim().eq_ignore_ascii_case("soft") {
                let factor = routing.affinity_boost_factor.max(0.0);
                return Some(
                    self.weighted_select(
                        available_candidates,
                        RoutingStrategy::Abstract,
                        request_weight,
                        Some((&bound, factor)),
                        sla_tenant,
                    )
                    .await,
                );
            }
            // hard：命中即返回（bind 命中必在合格集合内——affinity_hit 保证）
            if let Some((vendor, model)) = available_candidates.iter().find(|(v, _)| v.id == bound)
            {
                return Some(RoutingDecision {
                    vendor: vendor.clone(),
                    model: model.clone(),
                    strategy: RoutingStrategy::Abstract,
                });
            }
        }

        // recId=17：LPS 硬约束门控（默认关）——置于亲和块后：hard/soft
        // 命中语义优先（现状不变）。可行集筛选 + 空集回退 Ŵ 最小者。
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

        // recId=15：LinUCB 选择档 — selection="linucb"（热读）且接线且
        // 特征可得时 argmax UCB；否则 None 回退下方 weighted（安全语义对
        // 齐 affinity None）。置于亲和块后：hard 命中仍优先（现状语义）；
        // soft/加权路径不受影响。
        if let Some(decision) = self.linucb_route(
            abstract_id,
            &available_candidates,
            linucb_features,
            RoutingStrategy::Abstract,
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
            )
            .await,
        )
    }

    #[allow(clippy::too_many_arguments)] // 路由上下文穿线（log_chat_request 同款先例）
    async fn try_global_route(
        &self,
        excluded: &[String],
        session_key: Option<&str>,
        tenant: Option<&str>,
        // recId=14：SLA 虚拟队列键用原始租户（语义同 try_abstract_route）
        sla_tenant: Option<&str>,
        request_weight: Option<f64>,
        // recId=15：LinUCB 请求特征（selection="linucb" 档选择输入）
        linucb_features: Option<&[f64]>,
        // recId=17：x-deadline-ms 头（语义同 try_abstract_route）
        lps_deadline_ms: Option<f64>,
    ) -> Option<RoutingDecision> {
        let config = self.config.read();

        let (all_models, status_dropped) = self
            .collect_global_candidates(&config.vendors, excluded)
            .await;

        if all_models.is_empty() {
            // recId=19(b)：fail-open 安全网（语义同 try_abstract_route 注释）
            return self
                .global_fail_open(status_dropped, request_weight, sla_tenant)
                .await;
        }

        // recId=10(1)：亲和 —— hard（默认）命中即返回；soft 评分 boost
        //（语义同 try_abstract_route 注释）；未命中候选集原样归还续用
        let mut all_models = match self
            .global_affinity_decision(all_models, session_key, tenant, request_weight, sla_tenant)
            .await
        {
            Ok(decision) => return Some(decision),
            Err(models) => models,
        };

        // recId=17：LPS 硬约束门控（语义同 try_abstract_route；默认关）
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

        // recId=15：LinUCB 选择档（语义同 try_abstract_route 注释）；global
        // 路径无 abstract 模型键，臂键用 "__global__"（与 abstract 键空间
        // 隔离，全模型共享一臂组）。
        if let Some(decision) = self.linucb_route(
            "__global__",
            &all_models,
            linucb_features,
            RoutingStrategy::Global,
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
            )
            .await,
        )
    }

    /// 全局路径候选收集：enabled 过滤 + 排除集过滤 + 实时健康过滤
    ///（CB/Throttled 候选入 status_dropped，供 recId=19(b) fail-open 回落）。
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
            if excluded.iter().any(|e| e == &vendor.id) {
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

    /// recId=19(b) fail-open 安全网：存在被 CB/Throttled 剔除的候选且
    /// [routing].fail_open_when_all_cooling 开启 → 从回落集加权选择；
    /// 否则 None（调用方继续后续路由阶段）。
    async fn global_fail_open(
        &self,
        status_dropped: Vec<(VendorConfig, ModelConfig)>,
        request_weight: Option<f64>,
        sla_tenant: Option<&str>,
    ) -> Option<RoutingDecision> {
        if status_dropped.is_empty() || !self.config.read().routing.fail_open_when_all_cooling {
            return None;
        }
        tracing::warn!(
            count = status_dropped.len(),
            "{}",
            t("router-global-fail-open-engaged")
        );
        Some(
            self.weighted_select(
                status_dropped,
                RoutingStrategy::Global,
                request_weight,
                None,
                sla_tenant,
            )
            .await,
        )
    }

    /// recId=10(1)：亲和决策 —— soft 模式命中即带 boost 加权选择（候选集
    /// 消费，零克隆）；hard 模式命中且绑定 vendor 在合格集合内直接选它；
    /// 绑定 vendor 不在集合内则 Err 归还候选集（调用方继续 LPS/LinUCB/
    /// 加权阶段）。
    async fn global_affinity_decision(
        &self,
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
            // soft 命中：候选集消费进加权选择（原 move 语义，零克隆）
            let factor = routing.affinity_boost_factor.max(0.0);
            return Ok(self
                .weighted_select(
                    all_models,
                    RoutingStrategy::Global,
                    request_weight,
                    Some((&bound, factor)),
                    sla_tenant,
                )
                .await);
        }
        // hard 命中：绑定 vendor 在合格集合内直接选它（仅克隆选中项）；
        // 未命中/绑定 vendor 不在集合内：候选集原样归还（调用方继续
        // LPS/LinUCB/加权阶段）
        match all_models.iter().find(|(v, _)| v.id == bound) {
            Some((vendor, model)) => Ok(RoutingDecision {
                vendor: vendor.clone(),
                model: model.clone(),
                strategy: RoutingStrategy::Global,
            }),
            None => Err(all_models),
        }
    }

    /// recId=11：会话亲和 boost — 命中 vendor 在合格集合内则直接选它，
    /// 未命中走 weighted_select。boost 而非硬粘：合格集合外绝不强选
    /// （Astrolabe herding 证据——亲和与负载均衡冲突时，健康过滤优先）。
    ///
    /// 命中率记账（GlobalMetrics，recId=11⑤）：
    /// - 命中且在集合 → affinity_hits；
    /// - 命中但 vendor 已不可用/被排除 → 清除旧键 + affinity_misses；
    /// - 无绑定 → affinity_misses；
    /// - 未接线亲和表或无 session_key → 不计（不参与亲和的路由不进分母）。
    ///
    /// recId=10(1)：命中语义从「直接返回决策」改为「返回命中 vendor_id」
    /// —— hard 模式（默认）由调用方直接构造决策（现状不变），soft 模式
    /// 以权重乘子参与竞争。
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
                // 绑定的 vendor 已不在合格集合（下线/不可用/被排除）——
                // 清除旧键防持续 miss，回落 weighted_select
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

    /// recId=15：LinUCB 选择档 — `[routing].selection = "linucb"`（热读）
    /// 且 BanditState 已接线且特征可得（长度 == LINUCB_DIM）时，候选集内
    /// argmax UCB（`BanditState::argmax`，平局 rng 决断）；任一条件不满足
    /// 返回 None 由调用方回退 weighted（未接线安全语义，affinity None 同款）。
    /// α 热读 `[routing].ucb_alpha`（非有限/负值按 0 = 纯贪心）。
    fn linucb_route(
        &self,
        model_key: &str,
        candidates: &[(VendorConfig, ModelConfig)],
        features: Option<&[f64]>,
        strategy: RoutingStrategy,
    ) -> Option<RoutingDecision> {
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
        let mut rng = rand::thread_rng();
        let pick = bandit.argmax(model_key, &vendor_ids, x, alpha, &mut rng)?;
        let (vendor, model) = candidates[pick].clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy,
        })
    }

    /// 生产选择入口 — recId=4⑥ 后为薄 wrapper：先 async 计算权重，再以
    /// `thread_rng` 注入同步的 [`Self::weighted_select_with`]，行为与旧实现
    /// 完全一致（rng 在全部 await 之后创建，ThreadRng 不跨 await，Send 安全）。
    /// recId=10(1)：`affinity_boost` 为 soft 模式命中乘子 `Some((vendor_id,
    /// factor))`——权重计算后对命中 vendor 乘 (1+factor)，其余候选照常竞争。
    async fn weighted_select(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
        strategy: RoutingStrategy,
        request_weight: Option<f64>,
        affinity_boost: Option<(&str, f64)>,
        tenant: Option<&str>,
    ) -> RoutingDecision {
        let mut weighted = self
            .calculate_weights_overridden(candidates, request_weight, tenant)
            .await;
        if let Some((vendor_id, factor)) = affinity_boost {
            apply_affinity_boost(&mut weighted, vendor_id, factor);
        }
        let decision = {
            let mut rng = rand::thread_rng();
            self.weighted_select_with(weighted, strategy, &mut rng)
        };
        // recId=14：选择完成后对偶更新 Q ← max(0, Q + α − ŝ_selected)——
        // 决策时更新（SLARouter Eq 4/5 的 Lyapunov 语义），无需执行结果
        // 回传。sla_v=0（默认）旁路：Q 不读不写、行为与现状逐位一致。
        // 注意：direct/亲和 hard 等「命中即返回」的单候选路径不经本方法，
        // 无选择即无对偶更新（SLA 打分只作用于加权选择面）。
        if let Some((_, alpha)) = self.sla_gate() {
            self.sla_record_dual_update(&decision.vendor.id, tenant, alpha);
        }
        decision
    }

    /// recId=4⑥/10：async 权重收集 — calculate_weight 逐候选 await
    ///（get_window_info peek）。与同步选择核心分离的边界：`&mut dyn RngCore`
    /// 非 Send，无法跨 await 持有（编译器实测：axum handler future 报
    /// `dyn RngCore` 未实现 Send），故权重计算与 RNG 选择必须两段式。
    /// `pub`：集成测试的确定性入口（先收集真实权重，再注入 StdRng 选择）。
    pub async fn calculate_weights(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
    ) -> Vec<(f64, (VendorConfig, ModelConfig))> {
        self.calculate_weights_overridden(candidates, None, None)
            .await
    }

    /// recId=19(a)：四级权重回退链 —
    /// 1. 请求级权重（X-Weight 头；`[routing].allow_request_weight` 门控
    ///    已在 [`Self::route_weighted`] 判定，本函数不重复判）；
    /// 2. 配置 `model.weight`（> 0 时）；
    /// 3. vendor 容量自动权重 — 仅当 `model.weight <= 0`：window_snapshot
    ///    的 limit 除以集合内最大 limit 归一到 (0,1]（ModelConfig weight
    ///    缺省 1.0，该分支默认不触发）；
    /// 4. 均匀兜底 1.0（无请求权重且容量数据不可得）。
    ///
    /// recId=14：`tenant` 供 SLA 虚拟队列取 Q（键 = 租户，无租户上下文
    /// "global"）；`[routing].sla_v = 0`（默认）时该打分项整体旁路。
    pub async fn calculate_weights_overridden(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
        request_weight: Option<f64>,
        tenant: Option<&str>,
    ) -> Vec<(f64, (VendorConfig, ModelConfig))> {
        // 容量权重解析（第 3 级）：仅当无请求级覆盖且存在 model.weight<=0
        // 的候选才逐个取窗口快照；limit 归一基准 = 集合内最大 limit
        let mut capacity: HashMap<String, f64> = HashMap::new();
        if request_weight.is_none() && candidates.iter().any(|(_, m)| m.weight <= 0.0) {
            let mut limits: Vec<(String, u64)> = Vec::new();
            for (vendor, _) in &candidates {
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

        // 价格因子上下文（开关关闭时 None、零查询开销——逐位一致）
        let price_factor_enabled = self.config.read().routing.price_factor_enabled;
        let price_factors = if price_factor_enabled {
            self.price_factor_multipliers(&candidates)
        } else {
            Vec::new()
        };

        let mut weighted = Vec::with_capacity(candidates.len());
        // recId=14：SLA 虚拟队列打分上下文 — sla_v=0（默认）返回 None、
        // 零开销旁路（惩罚因子不参与权重）。候选集级预收集（缺价中位数、
        // 同一 Q 快照、ŝ 快照），保证集合内候选可比。
        let sla = self.sla_penalty_context(&candidates, tenant);
        // recId=17：LPS 软约束上下文 — lps_delta=0（默认）None、零开销旁路
        let lps = self.lps_penalty_context(&candidates);
        for (i, (vendor, model)) in candidates.into_iter().enumerate() {
            let effective_model_weight = request_weight.unwrap_or_else(|| {
                if model.weight > 0.0 {
                    model.weight
                } else {
                    capacity.get(&vendor.id).copied().unwrap_or(1.0)
                }
            });
            let mut weight = self.calculate_weight(&vendor, effective_model_weight).await;
            // recId=14：SLA 惩罚乘在权重链末端（calculate_weight 返回后；
            // 与失败率软降权、亲和 boost 独立乘法叠加）。因子需要候选集级
            // 上下文（缺价中位数、租户 Q 快照），故置于本层而非
            // calculate_weight 内部（后者仅有单 vendor 视野）。
            if let Some(sla) = &sla {
                let (cost, satisfaction) = sla.entries[i];
                weight *= sla_penalty_factor(sla.v, cost, sla.q, sla.alpha, satisfaction);
            }
            // recId=17：LPS 乘法软约束（权重链最末端；与 #14 独立乘法叠加）
            if let Some(lps) = &lps {
                weight *= lps.factors[i];
            }
            // 价格因子乘性修正（SLA/LPS 之后；定义与中性豁免见
            // apply_price_factor doc）。开关关闭时整体跳过——路由行为与
            // 现状逐位一致。
            if price_factor_enabled {
                if let Some(Some(factor)) = price_factors.get(i) {
                    weight *= *factor;
                }
            }
            weighted.push((weight, (vendor, model)));
        }
        weighted
    }

    /// 价格因子 — model_catalog 单价参与加权路由。
    ///
    /// 因子定义：候选模型单价 `p = (input_price + output_price) / 2`（每
    /// 1M tokens；cache 档不进因子——cache 命中率随负载变化，input/output
    /// 基础对是稳定公布口径）；集合内已登记候选取 `min_p`，乘性修正
    /// `weight ×= min_p / p ∈ (0,1]`——等价条件下低价候选权重不衰减、
    /// 高价按价格比例衰减。中性豁免：未登记 / 0 价 / 非有限单价候选因子
    /// 1.0（不奖励无定价者——与 recId=14 SLA 缺价中位数互为独立实现，
    /// 数据源不同：本因子吃 [[model_catalog]]，SLA 吃 [[pricing]]）。
    /// 单候选 / 空目录 / 无已登记候选整体中性。
    ///
    /// 查找用精确匹配（路由候选是 vendor 配置的精确模型身份；前缀回退属
    /// 计价域——D7.11 非对称锚）。
    fn price_factor_multipliers(
        &self,
        candidates: &[(VendorConfig, ModelConfig)],
    ) -> Vec<Option<f64>> {
        let catalog = &self.config.read().model_catalog;
        if catalog.is_empty() {
            return vec![None; candidates.len()];
        }
        let unit_price = |model_id: &str| -> Option<f64> {
            catalog
                .iter()
                .find(|e| e.model_id == model_id)
                .map(|e| (e.input_price + e.output_price) / 2.0)
                .filter(|p| p.is_finite() && *p > 0.0)
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

    /// recId=4⑥：RNG 注入版选择 — 原无 seed 注入点的 `thread_rng` 直写拆出
    /// 确定性入口。测试经 `StdRng::seed_from_u64(固定值)` 复现选择分布
    /// （recId=10 的 Po2C 采样亦经此注入）。生产行为不变。
    ///
    /// 评审修正（recId=10④）：温度平滑 `w^(1/T)` 在本方法内**恰好应用一次**
    ///（此前 compute_weight 与本方法各应用一次，等效 w^(1/T²)——T≠1 时
    /// 压平/放大语义错误）。入参权重应为未平滑的因子加权和
    ///（[`Self::calculate_weights`]/[`compute_weight`] 产出）。
    ///
    /// 与 plan 签名的偏差（以源码为准）：入参为**已算权重**的候选
    /// `Vec<(f64, (VendorConfig, ModelConfig))>` 而非裸 candidates —
    /// 权重计算是 async（get_window_info peek），而 `&mut dyn RngCore` 不能
    /// 跨 await 持有（Send 约束），async 计算 + 同步选择只能两段式；
    /// 调用方经 [`Self::weighted_select`]（生产）或
    /// [`Self::calculate_weights`] 语义等价获取权重。
    pub fn weighted_select_with(
        &self,
        weighted: Vec<(f64, (VendorConfig, ModelConfig))>,
        strategy: RoutingStrategy,
        rng: &mut dyn rand::RngCore,
    ) -> RoutingDecision {
        // recId=10①④：系数与温度热读（同步段，无 await）
        let routing = self.config.read().routing.clone();

        // recId=8③：min+buffer 候选集（平滑前的可选阶段）。命中即返回，
        // 不消费下游轮盘赌/Po2C 路径；buffer=0（默认）完全跳过、rng 零消耗，
        // 下游与历史逐位一致。
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

        // recId=10⑤：Po2C 实验开关（默认 weighted 走经典轮盘赌）
        if routing.selection == "power_of_two" && weighted.len() >= 2 {
            return self.power_of_two_pick(&weighted, strategy, routing.latency_ref_ms, rng);
        }

        let total_weight: f64 = weighted.iter().map(|(w, _)| w).sum();

        if total_weight <= 0.0 {
            let idx = rng.gen_range(0..weighted.len());
            return RoutingDecision {
                vendor: weighted[idx].1 .0.clone(),
                model: weighted[idx].1 .1.clone(),
                strategy,
            };
        }

        let target = rng.gen::<f64>() * total_weight;
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

    /// recId=10⑤：power-of-two-choices — 均匀随机采样 2 个候选、按 EWMA
    /// 延迟择优（低者胜）。经典 herding 缓解：权重不再直接决定选择概率，
    /// 近期负载信号（EWMA 延迟）在 2 选 1 时生效。无 EWMA 数据的候选按
    /// 2×ref_ms 保守计（视为高延迟）；两者平局时回退平滑权重高者（保持
    /// 权重信号）。纯同步，采样经 rng 注入可复现。
    fn power_of_two_pick(
        &self,
        weighted: &[(f64, (VendorConfig, ModelConfig))],
        strategy: RoutingStrategy,
        latency_ref_ms: f64,
        rng: &mut dyn rand::RngCore,
    ) -> RoutingDecision {
        let n = weighted.len();
        let i1 = rng.gen_range(0..n);
        // 无放回第二样本：撞车则取相邻（n>=2 时必为另一候选）
        let mut i2 = rng.gen_range(0..n);
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

    /// recId=8③：min+buffer 候选集 — 有 e2e 延迟数据的候选 ≥ 2 时，取
    /// `ewma_e2e ≤ 最低×(1+buffer)` 的候选集，集合内均匀随机（LiteLLM
    /// lowest_latency.py:482-494 语义：延迟相近的候选共享流量，防打爆
    /// 最快者）。无数据候选 / 不足 2 个 → None 回落下游轮盘赌。
    ///
    /// 与 Po2C 的关系：本阶段在前，命中即返回（二者同为实验机制，同时
    /// 配置时候选集优先——集合已含负载均衡语义，无需再 2 选 1）。
    /// 集合内均匀随机不经权重（权重信号保留给回落路径）。
    fn min_buffer_pick(
        &self,
        weighted: &[(f64, (VendorConfig, ModelConfig))],
        strategy: RoutingStrategy,
        buffer: f64,
        rng: &mut dyn rand::RngCore,
    ) -> Option<RoutingDecision> {
        let metrics = self.vendor_manager.metrics();
        // 「有延迟数据」= e2e 道有样本（vendor_ewma_e2e 的判定口径）；
        // 纯流式/纯失败史 vendor 不入候选集判定
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
        let idx = pool[rng.gen_range(0..pool.len())];
        let (vendor, model) = weighted[idx].1.clone();
        Some(RoutingDecision {
            vendor,
            model,
            strategy,
        })
    }

    async fn calculate_weight(&self, vendor: &VendorConfig, effective_model_weight: f64) -> f64 {
        // recId=10①：系数热读 — 每次路由从 DynamicField 热快照现读，
        // POST /admin/config 更新 [routing] 后立即生效（无需重启）
        let routing = self.config.read().routing.clone();
        // recId=6：负载维输入按 [routing].load_signal 单选（门控全部在调用侧，
        // compute_weight 纯函数签名不变）。默认 "queue" 与历史行为逐位一致。
        let queue_len = match routing.load_signal.as_str() {
            "in_flight" => self.vendor_manager.get_in_flight(&vendor.id),
            "in_flight_relative" => {
                let in_flight = self.vendor_manager.get_in_flight(&vendor.id) as f64;
                let fleet_mean = self.vendor_manager.fleet_mean_in_flight();
                relative_in_flight_to_len(in_flight, fleet_mean)
            }
            // "queue" 与未知值：队列深度（默认路径 + 回滚阀同路径）
            _ => self.vendor_manager.get_queue_length(&vendor.id),
        };
        let window = self.vendor_manager.get_window_info(&vendor.id).await;
        // recId=10③：EWMA 第 4 维输入（同步读，无 await）
        let ewma = self.vendor_manager.metrics().vendor_ewma(&vendor.id);
        // recId=19(a)：effective_model_weight 由四级回退链解析（calculate_weights_overridden）
        let weight = compute_weight(
            effective_model_weight,
            queue_len,
            window.as_ref(),
            &routing,
            ewma,
        );
        // recId=7（W5）：失败率软降权 — 门控默认关，关闭时与现状逐位一致
        self.failure_rate_penalty(&vendor.id, weight).await
    }

    /// recId=14：虚拟队列快照（只读）— 测试与运维观测的确定性断言入口
    ///（键 = 租户或 "global"）。poison 锁按空表处理（与 metrics 快照同款
    /// 容错方向）。
    pub fn sla_queue_snapshot(&self) -> HashMap<String, f64> {
        self.sla_queues
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// recId=7（W5）：失败率软降权 — `[resilience].failure_rate_gate` 开启
    /// 且失败率滑窗样本达标（total ≥ failure_rate_min_samples）且失败率
    /// 超阈（fail_pct > failure_rate_threshold）时权重 ×failure_rate_penalty
    /// （LiteLLM 数值对照 0.1）。软降权是权重面乘子而非状态位：路由层的
    /// `== Available` 过滤不动，只作用于仍 Available 的劣化 vendor。
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

    /// recId=14：SLA 门控热读 — `[routing].sla_v > 0` 且有限时开启，返回
    /// (V, α)。负值/NaN/±inf 一律视为关闭（`> 0` 门控，NaN 比较恒 false）。
    /// α 非有限按 0 处理、越界钳 [0,1]（α=0 → Q 恒 0，退化为纯成本项，
    /// fail-safe 方向）。每轮权重计算现读：POST /admin/config 更新后
    /// 下一请求立即生效（DynamicField 既有模式）。
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

    /// recId=14：满意度代理 ŝ = 1 − EWMA 错误率（零模型版：错误率代理
    /// 满意度，弱信号——调研报告已声明）。`vendor_ewma` 无 e2e 观测时
    /// 返回 1.0（冷启动乐观不惩罚；与 compute_weight 的 error_factor
    /// 无数据=1.0 同语义）。钳 [0,1] 防观测越界传导。
    fn satisfaction(&self, vendor_id: &str) -> f64 {
        match self.vendor_manager.metrics().vendor_ewma(vendor_id) {
            Some((_, ewma_err)) => (1.0 - ewma_err).clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    /// recId=17：候选集 Ŵ 前瞻 — α = 60s 流量窗总数/60（到达率，
    /// `vendor_window_total`，与 #7/#16 共用计数器）；μ = 1000/ewma_e2e_ms；
    /// k = 并发上限（缺省 100，与 manager 队列容量同源）。**口径声明**：
    /// ewma_e2e_ms 的口径含排队（recId=8），作为服务时长近似会低估 μ、
    /// 高估 Ŵ——粗分类定位可接受；无 e2e 数据 → Ŵ=0 中性（冷启动不惩罚，
    /// 与 error_factor 无数据=1.0 同语义）。
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

    /// recId=17：LPS 软约束上下文 — lps_delta=0（默认）返回 None 零开销。
    /// 因子 = 1/(1 + δ·Ŵ/Ŵ_ref)，Ŵ_ref = 候选集内 Ŵ 中位数（相对归一，
    /// 零新增配置）；Ŵ_ref=0（全候选无数据）→ 因子恒 1；全候选发散时
    /// 中位数=CAP → 因子均匀不扭曲选择。**与报告的偏差**：报告机制为
    /// 线性软约束「−δ·Ŵ 并入 compute_weight」（报告 :154），本实现有意
    /// 改乘法 1/(1+δ·Ŵ/Ŵ_ref)——线性负项会触发 compute_weight 的非正
    /// 归零保护（compute_weight 末尾 is_finite/＞0 检查）造成权重归零
    /// 被剔除的语义污染（评审 issue#17 采纳）。
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
        // Ŵ_ref = 中位数（与 #14 缺价中位数同构造）
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

    /// recId=17：LPS 硬约束门控热读 — `[routing].lps_deadline_gate`（默认
    /// false = 不消费 deadline、不筛选）。
    fn lps_deadline_gate_enabled(&self) -> bool {
        self.config.read().routing.lps_deadline_gate
    }

    /// recId=17：deadline 门控下的可行集筛选 — `Ŵ·1000 ≤ deadline_ms`
    ///（deadline = 请求头值，缺省 default_deadline_ms 热读；非法值视为
    /// 不设限）。
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

    /// recId=17：空可行集回退 — 改选 Ŵ 最小者（JITServe 软信号语义：
    /// 预测当软信号，无候选达标时也不拒绝服务）。
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

    /// recId=14：SLA 打分上下文预收集 — sla_v=0（默认）返回 None。
    /// Ĉ_m 现查热配置 [[pricing]]（(input+output)/2 每 1k 单价）；缺价者
    /// 记候选集内已知单价中位数（保守不奖励——记 0 会让无定价 vendor
    /// 获得最小惩罚、乘法式下权重最高，与成本感知目的相反）；候选集全无
    /// 定价时成本项整体置中性 0（退化为纯 Q 项）并 warn 一次。整候选集
    /// 共享同一 Q 快照，保证集合内可比。
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

    /// recId=14：选择完成后对偶更新 Q ← max(0, Q + α − ŝ_selected)。
    /// 读-改-写同一把写锁内完成（短临界区，无 await；poison 按 manager.rs
    /// 先例 into_inner 容错）。键 = 租户（API key 哈希）或 "global"。
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

/// recId=17：LPS 软约束的候选集级快照 — 因子与候选顺序对齐。
struct LpsPenalty {
    factors: Vec<f64>,
}

/// recId=17：deadline 门控的筛选结果。
enum LpsDeadline {
    /// 不筛（gate 关 / deadline 非法 / 全候选可行）
    NoFilter,
    /// 可行集（非空、真子集）
    Feasible(Vec<(VendorConfig, ModelConfig)>),
    /// 全候选超时 → 空集规则（改选 Ŵ 最小者，JITServe 软信号语义）
    EmptyPickMin,
}

/// recId=14：SLA 打分的候选集级快照 — 一次权重计算内所有候选共享同一
/// Q 值、同一缺价中位数兜底与各自的 (Ĉ, ŝ)，保证集合内可比。
struct SlaPenalty {
    v: f64,
    alpha: f64,
    /// (Ĉ_m, ŝ_m)，与候选顺序对齐
    entries: Vec<(f64, f64)>,
    /// 虚拟队列 Q 快照（键 = sla_queue_key(tenant)）
    q: f64,
}

/// recId=14：[[pricing]] 现查单候选的 Ĉ = (input+output)/2 每 1k 单价
///（热配置现读，与 CostCalculator::calculate 同键语义；负价钳 0，防
/// 惩罚因子 > 1 反向奖励）。缺价返回 None（调用方以候选集中位数兜底）。
fn sla_unit_price(pricing: &[PricingConfig], vendor_id: &str, model_id: &str) -> Option<f64> {
    pricing
        .iter()
        .find(|p| p.vendor_id == vendor_id && p.model_id == model_id)
        .map(|p| ((p.input_per_1k_tokens + p.output_per_1k_tokens) / 2.0).max(0.0))
}

/// recId=14：虚拟队列键 — 租户（调用方 API key 哈希，cache_api_key_hash
/// 同源），无租户上下文时 "global"。
fn sla_queue_key(tenant: Option<&str>) -> &str {
    tenant.unwrap_or("global")
}

/// recId=6：在途相对化到 queue_factor 输入域的纯函数 —
/// production-stack relative_load = (load − mean) / max(1, mean)（分母钳 1：
/// 近空闲舰队 mean→0 时不得放大噪声），线性映射到 queue_factor 的 0..100
/// 输入刻度：相对负载 ≤ 0（不高于舰队均值）→ 0（全空闲分）；
/// ≥ +1（两倍舰队均值）→ 100（queue_factor 触底 0）。
/// `pub` 供确定性单测（tests/router_test.rs 纯函数断言）。
pub fn relative_in_flight_to_len(in_flight: f64, fleet_mean: f64) -> usize {
    let denom = fleet_mean.max(1.0);
    let relative = (in_flight - fleet_mean) / denom;
    (relative.clamp(0.0, 1.0) * 100.0).round() as usize
}

/// recId=10(1)：亲和 boost 权重乘子（soft 模式）— 命中 vendor 权重
/// ×(1 + factor)，其余候选权重不动（照常竞争）。factor <= 0 为无操作
///（分布与无亲和逐位一致——结构性回滚语义）。production-stack
/// score = 亲和收益 − β·相对负载的形态以乘子近似（β=0 语义修正见报告）。
/// 独立纯函数 `pub` 供确定性单测（compute_weight 同款先例）。
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

/// recId=14：SLA 惩罚因子（SLARouter 零模型版）—
/// `1 / (1 + V·Ĉ_m + max(0, Q·(α − ŝ_m)))`。V>0、Ĉ≥0、Q≥0 时分母 ≥ 1，
/// 因子 ∈ (0,1]（纯减权，不产生负/无穷权重）。ŝ ≥ α（满意度达标）时
/// Q 项被 max(0,·) 钳 0，因子退化为纯成本项。独立纯函数 `pub` 供
/// 确定性单测（compute_weight 同款先例）。
pub fn sla_penalty_factor(v: f64, cost: f64, q: f64, alpha: f64, satisfaction: f64) -> f64 {
    let queue_term = (q * (alpha - satisfaction)).max(0.0);
    1.0 / (1.0 + v * cost + queue_term)
}

/// recId=14：虚拟队列对偶更新 `Q ← max(0, Q + α − ŝ_selected)`（决策时
/// 更新，SLARouter Eq 4/5 的 Lyapunov 语义）：α > ŝ（服务欠账）→ Q 累积；
/// α < ŝ（满意度达标）→ Q 递减、清账后钳 0（不越 0）。独立纯函数
/// `pub` 供确定性单测。
pub fn sla_dual_update(q: f64, alpha: f64, satisfaction: f64) -> f64 {
    (q + alpha - satisfaction).max(0.0)
}

/// recId=17：LPS Ŵ 截断上限 — ρ→1 时式 10 发散（(α/μ)^k 有界但 μ−α→0⁺），
/// 候选集内相对归一（Ŵ_ref 取中位数）下 CAP 只影响发散者自身；全候选
/// 发散时中位数=CAP → 因子均匀、不扭曲选择。
pub const LPS_WAIT_CAP: f64 = 1e6;

/// recId=17：并发上限缺省 — `VendorConfig.concurrency.max_concurrent_requests`
/// 缺省时与 VendorManager 实例化的队列容量同源（manager.rs `queue_size`
/// = 100）。
const LPS_DEFAULT_CONCURRENCY: f64 = 100.0;

/// recId=17：LPS 平均情形等待估计（论文式 10）—
/// `Ŵ = (α/μ)^k / (μ−α)`，ρ = α/(μk) ≥ 1（超载）时返回 [`LPS_WAIT_CAP`]。
/// k=1 退化为 M/M/1 队列等待 Wq = ρ/(μ−α)。非法输入按 fail-safe 处理：
/// α 负/非有限取 0（空闲零等待）；μ ≤ 0/非有限（无有效服务率）与
/// k < 1 视为超载方向（CAP / 钳 1）。`pub` 供确定性单测。
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

/// recId=14：候选集已知单价中位数 — 缺价候选的 Ĉ 兜底（保守不奖励：记 0
/// 会让无定价 vendor 获得最小惩罚、乘法式下权重最高，与成本感知目的相
/// 反）。偶数个取中间两值均值；空集 None（调用方据此把成本项整体置中性 0）。
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

/// 路由权重（recId=10 升级版）：
/// `weight = w_model×model.weight + w_queue×queue_factor + w_window×window_factor
///         + w_latency×latency_factor + w_error×error_factor`。
///
/// - model.weight: 配置权重（通常 0.0-2.0）
/// - queue_factor: 队列空闲度（[0,1]，0 队列=1.0，100+ 队列=0.0）
/// - window_factor: 时间窗口+Token 预算因子（[0,1]；无窗口=0.5；耗尽=0；即将结束=减半）
/// - latency_factor: ref/(ref+ewma_ms)（有界 (0,1)；无 EWMA 数据=中性 0.5；
///   recId=10③ 呼应 RouteBalance「在模型选择时就给延迟定价」）
/// - error_factor: 1 - ewma_error_rate（[0,1]；无数据=1.0 不惩罚冷启动）
///
/// 默认系数（0.4/0.3/0.3 + w_latency=w_error=0）与历史 3D 权重逐位一致。
/// `routing` 每次路由从热配置现读（recId=10① 热更新）；`pub` 供确定性
/// 测试与运维调试（tests/router_test.rs 纯函数断言）。
///
/// 评审修正（recId=10④）：本函数是**因子加权**纯函数，不含温度平滑——
/// 平滑由选择层 [`Router::weighted_select_with`] 恰好应用一次（此前
/// compute_weight 与 weighted_select_with 各平滑一次，等效 w^(1/T²)）。
pub fn compute_weight(
    model_weight: f64,
    queue_len: usize,
    window: Option<&WindowInfo>,
    routing: &crate::config::RoutingSection,
    ewma: Option<(f64, f64)>,
) -> f64 {
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
    let weight = model_component
        + queue_factor * routing.w_queue
        + window_factor * routing.w_window
        + latency_factor * routing.w_latency
        + error_factor * routing.w_error;
    if weight.is_finite() && weight > 0.0 {
        weight
    } else {
        0.0
    }
}

/// recId=10③：latency_factor = ref / (ref + ewma_ms) — 有界 (0,1)，
/// ewma == ref 时 0.5，ewma 越小得分越高。ref 配置非法（<=0）时中性 0.5。
fn latency_factor_from_ewma(ewma_ms: f64, ref_ms: f64) -> f64 {
    if !ref_ms.is_finite() || ref_ms <= 0.0 {
        return 0.5;
    }
    let denom = ref_ms + ewma_ms.max(0.0);
    ref_ms / denom
}

/// recId=10④：温度平滑 w^(1/T) — herding 缓解（权重高的厂商被过度选中后
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

    /// 历史默认系数（0.4/0.3/0.3、EWMA 维全关）下的 3D 权重 —
    /// 既有断言的数值语义保持不变。
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

    // ======== recId=6：in_flight_relative 相对化纯函数 ========

    /// production-stack relative_load = (x−mean)/max(1,mean) 映射到 0..100：
    /// 高于均值 → 正分；两倍均值饱和 100；不高于均值（含负）→ 0；
    /// 近空闲舰队 mean=0 由分母钳 1 兜底（防除零 + 不放大噪声）。
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

    // ======== recId=14：SLA 虚拟队列打分纯函数 ========

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

    // ======== recId=17：LPS 前瞻纯函数 ========

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
