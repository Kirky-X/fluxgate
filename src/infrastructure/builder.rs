// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Builder pattern helpers for infrastructure concrete implementations.
//!
//! This module provides builder types that wrap the concrete implementation
//! builders for ergonomic construction from the infrastructure layer:
//! - [`CacheBuilder`] - builds [`Oxcache`]
//! - [`ConfigBuilder`] - builds [`Confers`]
//! - [`DatabaseBuilder`] - builds [`Dbnexus`]

use std::time::Duration;

use super::cache::Oxcache;
use super::config::Confers;
use super::database::Dbnexus;

/// Builder pattern helper for Cache implementations.
#[derive(Default)]
pub struct CacheBuilder {
    ttl: Option<Duration>,
    capacity: Option<usize>,
}

impl CacheBuilder {
    /// Set the TTL (time-to-live) for cache entries.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the maximum capacity for the cache.
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Build the cache with configured options.
    pub fn build(self) -> Oxcache {
        Oxcache::builder()
            .ttl(
                self.ttl
                    .unwrap_or_else(|| std::time::Duration::from_secs(300)),
            )
            .capacity(self.capacity.unwrap_or(10000))
            .build()
    }
}

/// Builder pattern helper for Config implementations.
#[derive(Default)]
pub struct ConfigBuilder {
    file: Option<String>,
    env_prefix: Option<String>,
}

impl ConfigBuilder {
    /// Set the configuration file path.
    pub fn file(mut self, file: &str) -> Self {
        self.file = Some(file.to_string());
        self
    }

    /// Set the environment variable prefix.
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Build the config with configured options.
    pub fn build(self) -> Confers {
        Confers::builder()
            .file(self.file.as_deref().unwrap_or("config.toml"))
            .env_prefix(self.env_prefix.as_deref().unwrap_or("APP"))
            .build()
    }
}

/// Builder pattern helper for Database implementations.
#[derive(Default)]
pub struct DatabaseBuilder {
    max_connections: Option<u32>,
    timeout: Option<Duration>,
}

impl DatabaseBuilder {
    /// Set the maximum number of connections in the pool.
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = Some(max);
        self
    }

    /// Set the connection timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the database with configured options.
    pub fn build(self, connection_string: &str) -> Dbnexus {
        Dbnexus::builder()
            .max_connections(self.max_connections.unwrap_or(10))
            .timeout(
                self.timeout
                    .unwrap_or_else(|| std::time::Duration::from_secs(30)),
            )
            .build(connection_string)
    }
}
