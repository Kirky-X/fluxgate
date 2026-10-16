// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::GateError;
use crate::protocol::{UnifiedRequest, UnifiedResponse};
use async_channel::{bounded, Receiver, Sender};

pub struct PendingRequest {
    pub request: UnifiedRequest,
    pub response_tx: tokio::sync::oneshot::Sender<Result<UnifiedResponse, GateError>>,
    /// 入队时刻 — worker 完成分支据此计算排队+执行全程延迟。
    pub enqueued_at: std::time::Instant,
    /// worker 取走时刻 — [`VendorQueue::recv`] 单点盖戳，入队方恒为 None。
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

    pub async fn send(&self, request: PendingRequest) -> Result<(), GateError> {
        self.sender
            .send(request)
            .await
            .map_err(|_| GateError::VendorUnavailable {
                vendor: "queue_closed".into(),
            })
    }

    pub async fn recv(&self) -> Option<PendingRequest> {
        let mut pending = self.receiver.recv().await.ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pending() -> PendingRequest {
        let (tx, _rx) = tokio::sync::oneshot::channel::<Result<UnifiedResponse, GateError>>();
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
