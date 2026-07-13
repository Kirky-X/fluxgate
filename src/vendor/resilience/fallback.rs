// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! FallbackChain — T039 实现。
#![allow(dead_code)]

use crate::error::NexusFlowError;

#[derive(Debug, Clone)]
pub struct FallbackTarget {
    pub vendor_id: String,
    pub model_id: String,
    pub priority: u8,
}

#[derive(Debug, Clone, Copy)]
pub enum FallbackTrigger {
    Http5xx,
    Timeout,
    RateLimited,
    CircuitOpen,
    All,
}

pub struct FallbackChain {
    pub targets: Vec<FallbackTarget>,
    pub trigger: FallbackTrigger,
}

impl FallbackChain {
    pub fn new(targets: Vec<FallbackTarget>, trigger: FallbackTrigger) -> Self {
        Self { targets, trigger }
    }

    pub fn should_trigger(&self, _error: &NexusFlowError) -> bool {
        false
    }

    pub fn next_after_failure(&self, _failed: &str, _error: &NexusFlowError) -> Option<&FallbackTarget> {
        None
    }
}
