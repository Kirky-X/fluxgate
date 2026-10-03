// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::NexusFlowError;
use crate::protocol::{UnifiedRequest, UnifiedResponse};
use async_channel::{bounded, Receiver, Sender};

pub struct PendingRequest {
    pub request: UnifiedRequest,
    pub response_tx: tokio::sync::oneshot::Sender<Result<UnifiedResponse, NexusFlowError>>,
    /// recId=4③：入队时刻 — worker 完成分支以此计算排队+执行的全程延迟，
    /// 使 metrics.record_request 的口径覆盖队列等待（与 queue_factor 同源）。
    pub enqueued_at: std::time::Instant,
    /// recId=8：worker 取走时刻 — [`VendorQueue::recv`] 单点盖戳，入队方
    /// 恒为 None。queue 延迟道（ewma_queue_ms）= dequeued_at − enqueued_at。
    pub dequeued_at: Option<std::time::Instant>,
}

#[derive(Clone)]
pub struct VendorQueue {
    sender: Sender<PendingRequest>,
    receiver: Receiver<PendingRequest>,
}

impl VendorQueue {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        Self { sender, receiver }
    }

    pub async fn send(&self, request: PendingRequest) -> Result<(), NexusFlowError> {
        self.sender
            .send(request)
            .await
            .map_err(|_| NexusFlowError::VendorUnavailable {
                vendor: "queue_closed".into(),
            })
    }

    pub async fn recv(&self) -> Option<PendingRequest> {
        let mut pending = self.receiver.recv().await.ok()?;
        // recId=8：出队时刻在唯一出队口盖戳——结构上保证「recv 返回的
        // 请求必有 dequeued_at 且 ≥ enqueued_at」，worker 侧无需记忆。
        pending.dequeued_at = Some(std::time::Instant::now());
        Some(pending)
    }

    pub fn len(&self) -> usize {
        self.receiver.len()
    }

    pub fn is_empty(&self) -> bool {
        self.receiver.is_empty()
    }
}

// recId=4②：原 QueueManager 已删除 — 全库无写入方（grep 仅 manager.rs 读侧
// 四处引用），queue_factor 恒 1.0、3D 权重退化 2D 的根因即此死件。队列长度
// 一律直读 `VendorQueue::len()`（manager 的 VendorInstance.queue）。

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pending() -> PendingRequest {
        let (tx, _rx) = tokio::sync::oneshot::channel::<Result<UnifiedResponse, NexusFlowError>>();
        PendingRequest {
            request: serde_json::from_value(serde_json::json!({
                "model": "test",
                "messages": []
            }))
            .unwrap(),
            response_tx: tx,
            enqueued_at: std::time::Instant::now(),
            dequeued_at: None,
        }
    }

    /// recId=8：recv 后 dequeued_at 必被写入且 ≥ enqueued_at（出队口单点
    /// 盖戳的结构性断言）。
    #[tokio::test]
    async fn recv_stamps_dequeued_at_not_before_enqueued_at() {
        let queue = VendorQueue::new(4);
        let pending = sample_pending();
        assert!(pending.dequeued_at.is_none(), "入队方不得预盖戳");
        let enqueued_at = pending.enqueued_at;

        queue.send(pending).await.expect("send ok");
        let received = queue.recv().await.expect("recv ok");

        assert!(
            received.enqueued_at == enqueued_at,
            "enqueued_at 入队后不得被改写"
        );
        let dequeued_at = received.dequeued_at.expect("recv 必须盖 dequeued_at 戳");
        assert!(
            dequeued_at >= received.enqueued_at,
            "出队时刻不得早于入队时刻"
        );
    }

    /// 入队顺序保持：两条 pending 的 dequeued_at 单调（先入先出盖戳）。
    #[tokio::test]
    async fn recv_stamps_are_monotonic_per_fifo_order() {
        let queue = VendorQueue::new(4);
        queue.send(sample_pending()).await.unwrap();
        queue.send(sample_pending()).await.unwrap();

        let first = queue.recv().await.unwrap();
        let second = queue.recv().await.unwrap();
        assert!(
            second.dequeued_at.unwrap() >= first.dequeued_at.unwrap(),
            "FIFO 出队的时间戳必须单调"
        );
    }
}
