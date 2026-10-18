// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

pub mod cost;
pub mod key_pool;
pub mod manager;
pub mod pool;
pub mod probe;
pub mod queue;
pub mod rate_limiter;
#[cfg(feature = "resilience")]
pub mod resilience;
pub mod status;

pub use cost::{CostCalculator, PricingEntry};
pub use key_pool::{KeyPoolInfo, PoolSnapshot};
pub use manager::{
    estimate_cost_json, estimate_cost_unified, TicketPolicy, VendorCallOptions, VendorManager,
    WindowInfo,
};
pub use pool::{ConnectionPool, OwnedReqwestClient};
pub use queue::{PendingRequest, VendorQueue};
pub use rate_limiter::{AdvancedRateLimiter, ConcurrencyLimiter, ConcurrencyPermit, TokenBucket};
pub use status::{VendorState, VendorStatus};
