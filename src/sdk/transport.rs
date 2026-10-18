// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK 传输层：带重试的发送（`send_with_retry`）。

use reqwest::Response;

use crate::sdk::error::FluxgateSdkError;
use crate::sdk::retry::{system_jitter_seed, RetryPolicy};

/// 带重试的发送。
///
/// 重试判定（对齐官方 SDK）：
/// - 传输错误：`is_timeout` / `is_connect` / `is_request`；
/// - API 错误：`ApiError::is_retryable()`（408/409/429/5xx；
///   `x-should-retry` 显式否决/放行优先）。
///
/// 延迟：上游 `Retry-After` 提示优先（截断到 `max_retry_after`），缺省
/// 指数退避 + 抖动。流式调用方只在响应头阶段失败时重试——返回的
/// `Response` 未消费 body，`bytes_stream` 建立后由调用方负责。
pub(crate) async fn send_with_retry(
    request: &reqwest::RequestBuilder,
    policy: &RetryPolicy,
) -> Result<Response, FluxgateSdkError> {
    let mut attempt: u32 = 0;
    loop {
        // 每次尝试克隆新 builder（body 为 Bytes 可克隆）；不可克隆（如流式
        // body）不是 panic 场景，显性化为 Config 错误。
        let builder = request
            .try_clone()
            .ok_or_else(|| FluxgateSdkError::Config {
                message: "request builder is not cloneable (streaming request body?)".to_string(),
            })?;
        match builder.send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let text = read_error_body_capped(response).await;
                let error = FluxgateSdkError::from_http_error(status, &headers, &text);
                let retry_after = match &error {
                    FluxgateSdkError::Api(api) if api.is_retryable() => api.retry_after,
                    _ => return Err(error),
                };
                if attempt >= policy.max_retries {
                    return Err(error);
                }
                let delay = policy.delay_for(attempt, retry_after, system_jitter_seed());
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(source) => {
                let retryable = source.is_timeout() || source.is_connect() || source.is_request();
                let error = FluxgateSdkError::from(source);
                if !retryable || attempt >= policy.max_retries {
                    return Err(error);
                }
                let delay = policy.delay_for(attempt, None, system_jitter_seed());
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// 错误体只读前缀（64 KiB）：上游错误面不可信（体量与可读性皆然），
/// 读失败降级为已读前缀并 debug 记录，不静默吞错。
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

async fn read_error_body_capped(mut response: Response) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < MAX_ERROR_BODY_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = (MAX_ERROR_BODY_BYTES - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(target: "fluxgate::sdk", %error, "error body read failed; using prefix");
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
