// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::protocol::ProtocolConverter;
use crate::router::Router as RouterTrait;
use crate::{ConfigManager, Database, VendorManager};
use reqwest::Client;
use std::sync::Arc;

#[derive(Clone)]
pub struct ApiState {
    pub config: Arc<ConfigManager>,
    pub vendor_manager: Arc<VendorManager>,
    pub router: Arc<RouterTrait>,
    pub client: Client,
    pub openai_converter: Arc<dyn ProtocolConverter>,
    pub anthropic_converter: Arc<dyn ProtocolConverter>,
    pub database: Arc<Database>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("config", &"ConfigManager")
            .field("vendor_manager", &"VendorManager")
            .field("router", &"Router")
            .field("client", &"Client")
            .field("openai_converter", &"ProtocolConverter")
            .field("anthropic_converter", &"ProtocolConverter")
            .field("database", &"Database")
            .finish()
    }
}
