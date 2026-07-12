// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{ConfigManager, VendorConfig, ModelConfig};
use crate::vendor::{VendorManager, VendorStatus, WindowInfo};
use crate::error::ProxyError;
use rand::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub vendor: VendorConfig,
    #[allow(dead_code)]
    pub model: ModelConfig,
    #[allow(dead_code)]
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
        Self { config, vendor_manager }
    }
    
    pub async fn route(&self, abstract_model: &str) -> Result<RoutingDecision, ProxyError> {
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

        Err(ProxyError::Validation {
            field: format!("模型 {} 未找到", abstract_model),
        })
    }

    /// 使用路由提示进行路由
    pub async fn route_with_hints(
        &self,
        abstract_model: &str,
        preferred_vendor: Option<&str>,
        preferred_model: Option<&str>,
    ) -> Result<RoutingDecision, ProxyError> {
        // 如果指定了厂商，尝试直接路由到该厂商
        if let Some(vendor_id) = preferred_vendor {
            if let Some(decision) = self.try_vendor_route(vendor_id, preferred_model).await {
                return Ok(decision);
            }
            // 如果指定的厂商不可用或找不到，返回错误
            return Err(ProxyError::Validation {
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

    async fn try_vendor_route(&self, vendor_id: &str, model_hint: Option<&str>) -> Option<RoutingDecision> {
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
            if let Some(model) = vendor.models.iter().find(|m| {
                m.enabled && (
                    m.model_id.to_lowercase() == normalized ||
                    m.abstract_id.as_ref().map_or(false, |a| a.to_lowercase() == normalized)
                )
            }).cloned() {
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
        
        let model = vendor.models.iter()
            .find(|m| m.enabled && (m.model_id == model_id || m.abstract_id.as_ref().map_or(false, |a| a == model_id)))?
            .clone();
        
        Some(RoutingDecision { vendor, model, strategy: RoutingStrategy::Direct })
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
        let mut weight = model.weight;
        
        let queue_len = self.vendor_manager.get_queue_length(&vendor.id);
        if queue_len == 0 {
            weight *= 1.5;
        } else if queue_len < 10 {
            weight *= 1.2;
        }
        
        weight
    }
}
