// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! ConnectionPool — 基于 tokio::sync::Semaphore 的并发数限制连接池。
//!
//! 复用单个 `reqwest::Client`（内部自带连接池），外层 Semaphore 限制最大并发数。

use crate::error::GateError;
use std::sync::Arc;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct ConnectionPool {
    client: reqwest::Client,
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
}

/// 持有 permit 的 reqwest::Client 包装，drop 时自动释放。
pub struct OwnedReqwestClient {
    client: reqwest::Client,
    _permit: OwnedSemaphorePermit,
}

impl OwnedReqwestClient {
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

impl std::ops::Deref for OwnedReqwestClient {
    type Target = reqwest::Client;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl ConnectionPool {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
        }
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// permit 耗尽则异步等待；semaphore 关闭返回 `GateError::Config`。
    pub async fn acquire(&self) -> Result<OwnedReqwestClient, GateError> {
        // acquire_owned 消耗 Arc，需先 clone
        let permit =
            Arc::clone(&self.semaphore)
                .acquire_owned()
                .await
                .map_err(|e: AcquireError| GateError::Config {
                    message: crate::i18n::t_args(
                        "vendor-pool-semaphore-closed",
                        &[("error", e.to_string())],
                    ),
                })?;
        Ok(OwnedReqwestClient {
            client: self.client.clone(),
            _permit: permit,
        })
    }
}
