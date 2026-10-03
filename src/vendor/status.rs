// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorStatus {
    Available,
    Busy,
    Throttled,
    Unavailable,
}

pub struct VendorState {
    status: AtomicU8,
    status_changed: AtomicU64,
    error_count: AtomicU8,
}

impl Default for VendorState {
    fn default() -> Self {
        Self::new()
    }
}

impl VendorState {
    pub fn new() -> Self {
        Self {
            status: AtomicU8::new(VendorStatus::Available as u8),
            status_changed: AtomicU64::new(0),
            error_count: AtomicU8::new(0),
        }
    }

    pub fn get_status(&self) -> VendorStatus {
        VendorStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    pub fn set_status(&self, status: VendorStatus) {
        self.status.store(status as u8, Ordering::Relaxed);
        self.status_changed.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            Ordering::Relaxed,
        );
    }

    pub fn inc_error(&self) -> u8 {
        self.error_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn reset_error(&self) {
        self.error_count.store(0, Ordering::Relaxed);
    }

    pub fn get_error_count(&self) -> u8 {
        self.error_count.load(Ordering::Relaxed)
    }

    pub fn mark_busy(&self) {
        self.set_status(VendorStatus::Busy);
    }

    pub fn mark_available(&self) {
        self.set_status(VendorStatus::Available);
    }

    pub fn mark_throttled(&self, _backoff: std::time::Duration) {
        self.set_status(VendorStatus::Throttled);
    }

    pub fn mark_unavailable(&self) {
        self.set_status(VendorStatus::Unavailable);
    }
}

impl VendorStatus {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => VendorStatus::Available,
            1 => VendorStatus::Busy,
            2 => VendorStatus::Throttled,
            3 => VendorStatus::Unavailable,
            _ => VendorStatus::Available,
        }
    }
}
