// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! ConnectionPool — 基于 tokio::sync::Semaphore 的并发数限制连接池。
//!
//! 设计：
//! - 复用单个 `reqwest::Client`（Client 内部已自带连接池）
//! - 外层用 Semaphore 限制最大并发请求数
//! - `acquire()` 返回 `OwnedReqwestClient`，drop 时自动释放 permit
//! - 模仿 `tower::limit::ConcurrencyLimit` 模式但不引入 tower 依赖

use crate::error::NexusFlowError;
use std::sync::Arc;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

/// 连接池 — 共享 reqwest::Client + 并发限制 Semaphore
#[derive(Clone)]
pub struct ConnectionPool {
    client: reqwest::Client,
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
}

/// 持有 permit 的 reqwest::Client 包装。
/// drop 时自动释放并发 permit。
pub struct OwnedReqwestClient {
    client: reqwest::Client,
    _permit: OwnedSemaphorePermit,
}

impl OwnedReqwestClient {
    /// 获取底层 reqwest::Client 引用
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
    /// 创建连接池，最大并发数为 `max_concurrent`
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
        }
    }

    /// 当前可用的并发 permit 数量
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// 最大并发数
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// 获取一个 permit + client 引用。
    /// 若所有 permit 已被占用，会异步等待直到有 permit 释放。
    /// 若 semaphore 被关闭（pool 被销毁），返回 `NexusFlowError::Config`。
    pub async fn acquire(&self) -> Result<OwnedReqwestClient, NexusFlowError> {
        // acquire_owned 需要 `Arc<Semaphore>` by value（消耗 Arc），
        // 因此先 clone 一份 Arc 再传入
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|e: AcquireError| NexusFlowError::Config {
                message: format!("connection pool semaphore closed: {}", e),
            })?;
        Ok(OwnedReqwestClient {
            client: self.client.clone(),
            _permit: permit,
        })
    }
}
