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
    #[allow(dead_code)]
    queue_size: usize,
}

impl VendorQueue {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        Self {
            sender,
            receiver,
            queue_size: capacity,
        }
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

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.receiver.is_empty()
    }

    #[allow(dead_code)]
    pub fn is_full(&self) -> bool {
        self.receiver.len() >= self.queue_size
    }

    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.queue_size
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

    #[allow(dead_code)]
    pub fn get_or_create(&self, vendor_id: &str, capacity: usize) -> Arc<VendorQueue> {
        let mut guard = self.queues.lock().unwrap();

        if let Some(queue) = guard.get(vendor_id) {
            return queue.clone();
        }

        let queue = Arc::new(VendorQueue::new(capacity));
        guard.insert(vendor_id.to_string(), queue.clone());
        queue
    }

    pub fn get(&self, vendor_id: &str) -> Option<Arc<VendorQueue>> {
        self.queues.lock().unwrap().get(vendor_id).cloned()
    }

    #[allow(dead_code)]
    pub fn remove(&self, vendor_id: &str) {
        self.queues.lock().unwrap().remove(vendor_id);
    }

    #[allow(dead_code)]
    pub fn get_all_lengths(&self) -> std::collections::HashMap<String, usize> {
        self.queues
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect()
    }
}
