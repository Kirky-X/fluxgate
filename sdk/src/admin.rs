//! Admin / health 端点客户端方法。

use serde::Deserialize;

use crate::client::NexusFlowClient;
use crate::error::NexusFlowSdkError;

/// 模型信息。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub vendor: String,
}

/// 供应商信息。
#[derive(Debug, Clone, Deserialize)]
pub struct VendorInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// 健康状态。
#[derive(Debug, Clone, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub version: String,
    pub vendors: u32,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct VendorsResponse {
    vendors: Vec<VendorInfo>,
}

impl NexusFlowClient {
    /// GET /v1/models — 获取可用模型列表。
    pub async fn models(&self) -> Result<Vec<ModelInfo>, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("v1/models")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let body: ModelsResponse = resp.json().await?;
        Ok(body.data)
    }

    /// GET /admin/vendors — 获取供应商列表。
    pub async fn vendors(&self) -> Result<Vec<VendorInfo>, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("admin/vendors")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self.http.get(url).bearer_auth(&self.api_key).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let body: VendorsResponse = resp.json().await?;
        Ok(body.vendors)
    }

    /// GET /admin/health — 获取服务健康状态。
    pub async fn health(&self) -> Result<HealthStatus, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("admin/health")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self.http.get(url).bearer_auth(&self.api_key).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let body: HealthStatus = resp.json().await?;
        Ok(body)
    }
}
