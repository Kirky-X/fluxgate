// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 异步日志写入器
//!
//! 使用专用 channel 实现异步批量日志写入，提高性能。

use crate::database::{Database, Repository, RequestLog};
use crate::error::ProxyError;
use async_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// 日志写入命令
enum LogCommand {
    /// 写入日志
    Write(RequestLog),
    /// 关闭日志写入器
    Shutdown,
}

/// 异步日志写入器
pub struct AsyncLogger {
    sender: Sender<LogCommand>,
}

impl AsyncLogger {
    /// 创建新的异步日志写入器
    pub fn new(
        repository: Arc<Repository>,
        channel_capacity: usize,
        flush_interval: Duration,
    ) -> Self {
        let (sender, receiver) = bounded(channel_capacity);

        // 启动后台写入任务
        tokio::spawn(async move {
            Self::writer_loop(receiver, repository, flush_interval).await;
        });

        Self { sender }
    }

    /// 异步写入日志
    pub async fn log(&self, log: RequestLog) -> Result<(), ProxyError> {
        self.sender
            .send(LogCommand::Write(log))
            .await
            .map_err(|_| ProxyError::Database {
                message: "Async logger channel closed".into(),
            })
    }

    /// 优雅关闭
    pub async fn shutdown(&self) -> Result<(), ProxyError> {
        self.sender
            .send(LogCommand::Shutdown)
            .await
            .map_err(|_| ProxyError::Database {
                message: "Async logger channel closed".into(),
            })
    }

    /// 后台写入循环
    async fn writer_loop(
        receiver: Receiver<LogCommand>,
        repository: Arc<Repository>,
        flush_interval: Duration,
    ) {
        let mut buffer: Vec<RequestLog> = Vec::with_capacity(100);
        let mut interval = tokio::time::interval(flush_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // 定时刷新缓冲区
                    Self::flush_buffer(&repository, &mut buffer).await;
                }
                command = receiver.recv() => {
                    match command {
                        Ok(LogCommand::Write(log)) => {
                            buffer.push(log);
                            // 如果缓冲区已满，立即刷新
                            if buffer.len() >= 100 {
                                Self::flush_buffer(&repository, &mut buffer).await;
                            }
                        }
                        Ok(LogCommand::Shutdown) => {
                            // 关闭前刷新剩余日志
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                        Err(_) => {
                            // Channel 已关闭，退出
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 刷新缓冲区到数据库
    async fn flush_buffer(repository: &Arc<Repository>, buffer: &mut Vec<RequestLog>) {
        if buffer.is_empty() {
            return;
        }

        // 批量写入
        for log in buffer.drain(..) {
            if let Err(e) = repository.log_request(&log).await {
                tracing::error!("Failed to write request log: {}", e);
            }
        }
    }
}

/// 异步日志写入器句柄
#[derive(Clone)]
pub struct AsyncLoggerHandle {
    inner: Arc<Mutex<Option<AsyncLogger>>>,
}

impl AsyncLoggerHandle {
    /// 创建新的句柄
    pub fn new(logger: AsyncLogger) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(logger))),
        }
    }

    /// 写入日志
    pub async fn log(&self, log: RequestLog) -> Result<(), ProxyError> {
        let guard = self.inner.lock().await;
        if let Some(logger) = guard.as_ref() {
            logger.log(log).await
        } else {
            Err(ProxyError::Database {
                message: "Logger already shut down".into(),
            })
        }
    }

    /// 关闭日志写入器
    pub async fn shutdown(&self) -> Result<(), ProxyError> {
        let mut guard = self.inner.lock().await;
        if let Some(logger) = guard.take() {
            logger.shutdown().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_async_logger() {
        // 测试日志写入
        // 注意：实际测试需要模拟数据库连接
    }
}
