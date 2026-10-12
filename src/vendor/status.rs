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
    /// Throttled 冷却到期时刻（unix 毫秒；0 = 非冷却）；读侧惰性恢复。
    throttled_until_ms: AtomicU64,
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
            throttled_until_ms: AtomicU64::new(0),
        }
    }

    pub fn get_status(&self) -> VendorStatus {
        let status = VendorStatus::from_u8(self.status.load(Ordering::Relaxed));
        if status == VendorStatus::Throttled {
            let until = self.throttled_until_ms.load(Ordering::Relaxed);
            if until > 0 && unix_now_ms() >= until {
                self.mark_available();
                return VendorStatus::Available;
            }
        }
        status
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
        self.throttled_until_ms.store(0, Ordering::Relaxed);
        self.set_status(VendorStatus::Available);
    }

    pub fn mark_throttled(&self, backoff: std::time::Duration) {
        // 先写到期时刻再翻状态：避免读侧把 Available 误读成无 TTL 的 Throttled
        self.throttled_until_ms.store(
            unix_now_ms().saturating_add(backoff.as_millis() as u64),
            Ordering::Relaxed,
        );
        self.set_status(VendorStatus::Throttled);
    }

    pub fn mark_unavailable(&self) {
        self.set_status(VendorStatus::Unavailable);
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn throttled_cooldown_recovers_after_backoff_expiry() {
        let state = VendorState::new();
        state.mark_throttled(Duration::from_millis(0));
        assert_eq!(state.get_status(), VendorStatus::Available);
        assert_eq!(state.throttled_until_ms.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn throttled_stays_within_window_and_available_clears_ttl() {
        let state = VendorState::new();
        state.mark_throttled(Duration::from_secs(3600));
        assert_eq!(state.get_status(), VendorStatus::Throttled);

        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);

        state.mark_throttled(Duration::from_secs(3600));
        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);
    }
}
