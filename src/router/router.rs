// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{ConfigManager, ModelConfig, VendorConfig};
use crate::error::NexusFlowError;
use crate::vendor::{VendorManager, VendorStatus, WindowInfo};
use rand::prelude::*;
use std::sync::Arc;

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
}

impl Router {
    pub fn new(config: Arc<ConfigManager>, vendor_manager: Arc<VendorManager>) -> Self {
        Self {
            config,
            vendor_manager,
        }
    }

    pub async fn route(&self, abstract_model: &str) -> Result<RoutingDecision, NexusFlowError> {
        let normalized = abstract_model.to_lowercase();
        let is_auto = normalized == "auto";

        // Try abstract route first (model name without vendor prefix)
        if let Some(decision) = self.try_abstract_route(&normalized).await {
            return Ok(decision);
        }

        // Try direct route (vendor.model format)
        if let Some(decision) = self.try_direct_route(&normalized).await {
            return Ok(decision);
        }

        // For "auto" request, try global route
        if is_auto {
            if let Some(decision) = self.try_global_route().await {
                return Ok(decision);
            }
        }

        Err(NexusFlowError::Validation {
            field: format!("模型 {} 未找到", abstract_model),
        })
    }

    /// 使用路由提示进行路由
    pub async fn route_with_hints(
        &self,
        abstract_model: &str,
        preferred_vendor: Option<&str>,
        preferred_model: Option<&str>,
    ) -> Result<RoutingDecision, NexusFlowError> {
        // 如果指定了厂商，尝试直接路由到该厂商
        if let Some(vendor_id) = preferred_vendor {
            if let Some(decision) = self.try_vendor_route(vendor_id, preferred_model).await {
                return Ok(decision);
            }
            // 如果指定的厂商不可用或找不到，返回错误
            return Err(NexusFlowError::Validation {
                field: format!("指定的厂商 {} 不可用或未配置", vendor_id),
            });
        }

        // 如果指定了模型，优先使用该模型
        if let Some(model_id) = preferred_model {
            let normalized = model_id.to_lowercase();
            if let Some(decision) = self.try_abstract_route(&normalized).await {
                return Ok(decision);
            }
        }

        // 默认路由逻辑
        self.route(abstract_model).await
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
        if self.vendor_manager.get_status(vendor_id) != VendorStatus::Available {
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

    async fn try_direct_route(&self, input: &str) -> Option<RoutingDecision> {
        let parts: Vec<&str> = input.splitn(2, '.').collect();
        if parts.len() != 2 {
            return None;
        }

        let (vendor_id, model_id) = (parts[0], parts[1]);

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

    async fn try_abstract_route(&self, abstract_id: &str) -> Option<RoutingDecision> {
        let candidates = self.config.resolve_model(abstract_id);

        let available_candidates: Vec<(VendorConfig, ModelConfig)> = candidates
            .into_iter()
            .filter(|(vendor, model)| {
                // Only consider enabled vendors and models
                if !vendor.enabled || !model.enabled {
                    return false;
                }
                self.vendor_manager.get_status(&vendor.id) == VendorStatus::Available
            })
            .collect();

        if available_candidates.is_empty() {
            return None;
        }

        Some(self.weighted_select(available_candidates, RoutingStrategy::Abstract))
    }

    async fn try_global_route(&self) -> Option<RoutingDecision> {
        let config = self.config.read();

        let mut all_models: Vec<(VendorConfig, ModelConfig)> = Vec::new();

        for vendor in &config.vendors {
            if !vendor.enabled {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                if self.vendor_manager.get_status(&vendor.id) != VendorStatus::Available {
                    continue;
                }

                all_models.push((vendor.clone(), model.clone()));
            }
        }

        if all_models.is_empty() {
            return None;
        }

        Some(self.weighted_select(all_models, RoutingStrategy::Global))
    }

    fn weighted_select(
        &self,
        candidates: Vec<(VendorConfig, ModelConfig)>,
        strategy: RoutingStrategy,
    ) -> RoutingDecision {
        let mut rng = rand::thread_rng();

        let weighted: Vec<(f64, (VendorConfig, ModelConfig))> = candidates
            .into_iter()
            .map(|(vendor, model)| {
                let weight = self.calculate_weight(&vendor, &model);
                (weight, (vendor, model))
            })
            .collect();

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

    fn calculate_weight(&self, vendor: &VendorConfig, model: &ModelConfig) -> f64 {
        let queue_len = self.vendor_manager.get_queue_length(&vendor.id);
        let window = self.vendor_manager.get_window_info(&vendor.id);
        compute_weight(model.weight, queue_len, window.as_ref())
    }
}

/// 3D 权重计算：model.weight × 0.4 + queue_factor × 0.3 + window_factor × 0.3
/// - model.weight: 配置权重（通常 0.0-2.0）
/// - queue_factor: 队列空闲度（[0,1]，0 队列=1.0，100+ 队列=0.0）
/// - window_factor: 时间窗口+Token 预算因子（[0,1]；无窗口=0.5；耗尽=0；即将结束=减半）
fn compute_weight(model_weight: f64, queue_len: usize, window: Option<&WindowInfo>) -> f64 {
    let model_component = model_weight * 0.4;
    let queue_factor = queue_factor_from_len(queue_len);
    let window_factor = window_factor_from_info(window);
    let weight = model_component + queue_factor * 0.3 + window_factor * 0.3;
    if weight.is_finite() && weight > 0.0 {
        weight
    } else {
        0.0
    }
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
    use std::time::{Duration, Instant};

    fn window(max: Option<u64>, used: u64, end_offset_secs: Option<u64>) -> WindowInfo {
        WindowInfo {
            max_tokens: max,
            used_tokens: used,
            window_end: end_offset_secs.map(|s| Instant::now() + Duration::from_secs(s)),
        }
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
        let w = compute_weight(1.0, 0, None);
        assert!((w - 0.85).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_high_model_weight_dominates() {
        // model.weight=2.0, queue=0, window=None
        // = 2.0×0.4 + 1.0×0.3 + 0.5×0.3 = 0.8 + 0.3 + 0.15 = 1.25
        let w = compute_weight(2.0, 0, None);
        assert!((w - 1.25).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_exhausted_budget_lowers_weight() {
        let w_full = compute_weight(1.0, 0, None);
        let exhausted = window(Some(100), 100, None);
        let w_exhausted = compute_weight(1.0, 0, Some(&exhausted));
        // 预算耗尽 → window_factor=0 → 0.4 + 0.3 + 0 = 0.7
        assert!((w_exhausted - 0.7).abs() < 1e-9);
        assert!(w_exhausted < w_full);
    }

    #[test]
    fn test_compute_weight_long_queue_lowers_weight() {
        let w_short = compute_weight(1.0, 0, None);
        let w_long = compute_weight(1.0, 100, None);
        // queue=100 → queue_factor=0 → 0.4 + 0 + 0.15 = 0.55
        assert!((w_long - 0.55).abs() < 1e-9);
        assert!(w_long < w_short);
    }

    #[test]
    fn test_compute_weight_zero_model_weight_keeps_minimum() {
        // model.weight=0, queue=0, window=None
        // = 0 + 0.3 + 0.15 = 0.45 (非零，不归零)
        let w = compute_weight(0.0, 0, None);
        assert!((w - 0.45).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_negative_inputs_clamped_to_zero() {
        // model.weight = -1.0 (异常配置)
        // = -0.4 + 0.3 + 0.15 = 0.05 → > 0 保留
        let w = compute_weight(-1.0, 0, None);
        assert!((w - 0.05).abs() < 1e-9);
    }

    #[test]
    fn test_compute_weight_all_zero_when_exhausted_and_queued() {
        // model.weight=0, queue=100, budget exhausted
        // = 0 + 0 + 0 = 0
        let exhausted = window(Some(100), 100, None);
        let w = compute_weight(0.0, 100, Some(&exhausted));
        assert_eq!(w, 0.0);
    }
}
