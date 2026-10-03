// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Infrastructure Layer — DI container
//!
//! Provides a trait-kit `AsyncKit` container that assembles the four
//! brother libs (oxcache, dbnexus, limiteron, inklog) at startup.
//!
//! The `di` submodule is cfg-gated by `infrastructure` + `database` +
//! `vendor-management` + `logging` (all four features must be enabled).

/// trait-kit AsyncKit DI container — only available when all four
/// infrastructure-level features are enabled (oxcache via `infrastructure`,
/// dbnexus via `database`, limiteron via `vendor-management`, inklog via
/// `logging`). The `kit` feature on each dep provides `integrations::kit`.
#[cfg(all(
    feature = "infrastructure",
    feature = "database",
    feature = "vendor-management",
    feature = "logging"
))]
pub mod di;
