// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Rerank 请求/响应类型与客户端方法（POST /v1/rerank、GET /v1/models）。

use serde::{Deserialize, Serialize};

use crate::sdk::client::FluxgateClient;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;

/// Rerank 请求（documents 为纯文本数组；对象形态由 REST 直接使用）。
#[derive(Debug, Clone)]
pub struct RerankRequest {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    /// 缺省 = documents.len()（网关侧语义）
    pub top_n: Option<usize>,
    /// 缺省 false；true 时响应回传文档文本
    pub return_documents: Option<bool>,
    /// 上游支持时透传（Cohere 形态）
    pub instruction: Option<String>,
    pub config: RequestConfig,
}

/// Rerank 响应。
#[derive(Debug, Clone, Deserialize)]
pub struct RerankResponse {
    pub id: String,
    /// 实际上游模型名（多厂商路由后可感知）
    pub model: String,
    pub results: Vec<RerankResult>,
    pub meta: RerankMeta,
}

/// 单条重排结果。
#[derive(Debug, Clone, Deserialize)]
pub struct RerankResult {
    pub index: usize,
    pub relevance_score: f64,
    #[serde(default)]
    pub document: Option<RerankDocument>,
}

/// 回传文档（return_documents=true 时存在）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RerankDocument {
    pub text: String,
}

/// 响应元信息：实际厂商与参与重排的文档数。
#[derive(Debug, Clone, Deserialize)]
pub struct RerankMeta {
    pub vendor: String,
    pub billed_documents: usize,
}

/// rerank 请求 wire 载荷：类型化单次序列化，可选字段省略（无 null）。
#[derive(Serialize)]
struct RerankPayload<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    top_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_documents: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instruction: Option<&'a str>,
}

impl FluxgateClient {
    /// 发送 rerank 请求。POST /v1/rerank（非流式，无采样参数）。
    pub async fn rerank(&self, req: &RerankRequest) -> Result<RerankResponse, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);
        let payload = RerankPayload {
            model: &req.model,
            query: &req.query,
            documents: &req.documents,
            top_n: req.top_n,
            return_documents: req.return_documents,
            instruction: req.instruction.as_deref(),
        };
        let url = self
            .base_url
            .join("v1/rerank")
            .map_err(|e| FluxgateSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(merged.timeout.unwrap_or(self.effective_timeout))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(resp.json().await?)
    }

    /// 模型列表。`task = None` → `GET /v1/models`；Some → `?task=<值>`。
    pub async fn list_models(
        &self,
        task: Option<&str>,
    ) -> Result<serde_json::Value, FluxgateSdkError> {
        let mut url = self
            .base_url
            .join("v1/models")
            .map_err(|e| FluxgateSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;
        if let Some(task) = task {
            url.query_pairs_mut().append_pair("task", task);
        }

        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(resp.json().await?)
    }
}
