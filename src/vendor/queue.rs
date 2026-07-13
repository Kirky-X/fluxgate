// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::NexusFlowError;
use crate::protocol::{UnifiedRequest, UnifiedResponse};
use async_channel::{bounded, Receiver, Sender};
use std::sync::Arc;

pub struct PendingRequest {
    pub request: UnifiedRequest,
    pub response_tx: tokio::sync::oneshot::Sender<Result<UnifiedResponse, NexusFlowError>>,
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
        self.receiver.recv().await.ok()
    }

    pub fn len(&self) -> usize {
        self.receiver.len()
    }
}

#[derive(Clone)]
pub struct QueueManager {
    queues: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<VendorQueue>>>>,
}

impl QueueManager {
    pub fn new() -> Self {
        Self {
            queues: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn get(&self, vendor_id: &str) -> Option<Arc<VendorQueue>> {
        self.queues.lock().unwrap().get(vendor_id).cloned()
    }
}
