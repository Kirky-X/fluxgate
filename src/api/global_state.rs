// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Global state holder for sdforge handlers
//!
//! Since sdforge's `#[service_api]` macro doesn't support Axum's State extractor,
//! we use a global OnceCell to hold the ApiState reference.

use crate::ApiState;
use once_cell::sync::OnceCell;
use std::sync::Arc;

/// Global API state holder
///
/// This is initialized once at server startup and accessed by all sdforge handlers.
pub static GLOBAL_STATE: OnceCell<Arc<ApiState>> = OnceCell::new();

/// Initialize the global state
///
/// # Panics
/// Panics if called more than once
pub fn init_global_state(state: Arc<ApiState>) {
    GLOBAL_STATE
        .set(state)
        .expect("Global state already initialized");
}

/// Get the global state
///
/// # Panics
/// Panics if global state hasn't been initialized
pub fn get_global_state() -> Arc<ApiState> {
    GLOBAL_STATE
        .get()
        .expect("Global state not initialized")
        .clone()
}
