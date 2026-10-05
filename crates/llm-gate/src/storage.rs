// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 存储边界缝（design.md D3）：请求日志与 Token 用量的存储契约。
//!
//! 契约类型与两 trait 同居于此，llm-gate 零数据库依赖——dbnexus/sea-orm
//! 实现全部在 nexusflow 的 database 模块，经 trait 对象注入 ApiState。
//! 管道前沿（异步日志 channel、预聚合器）与存储后端（Repository）实现
//! 同一 trait：查询方法带「不支持」默认实现，写半边由各自真实承担——
//! admin 读恒经存储后端实现，管道前沿不被虚构查询能力。

use crate::error::GateError;
use serde::{Deserialize, Serialize};

/// 请求日志行：数据面写入与 admin 查询的共用契约（原 nexusflow
/// database::RequestLog 随 N2 上收 llm-gate，字段逐位保持）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: Option<i64>,
    pub request_id: String,
    pub timestamp: i64,
    pub endpoint: String,
    pub input_format: String,
    pub abstract_model: Option<String>,
    pub messages: String,
    pub actual_vendor: String,
    pub actual_model: String,
    pub routing_strategy: String,
    pub status_code: Option<i32>,
    pub response: Option<String>,
    pub error_message: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// 日志查询过滤（admin /admin/logs、/admin/logs/search）。
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: u32,
}

/// Token 用量按 (vendor, model, date, hour) 预聚合行。原 database::TokenUsage
/// 显式改名消歧——与 IR 的 protocol::TokenUsage 同名易混（T002）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageRow {
    pub id: Option<i64>,
    pub vendor: String,
    pub model: String,
    pub date: String,
    pub hour: i32,
    pub request_count: i64,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd_sum: f64,
}

/// 请求日志存储缝：数据面写入（管道前沿 push 或存储后端直写）+ admin 查询。
#[async_trait::async_trait]
pub trait RequestLogSink: Send + Sync {
    /// 写入一条请求日志。管道实现 = channel push 即返回（满载丢弃并计数，
    /// 不阻塞响应）；存储后端实现 = 直接落库。错误由调用方记日志后吞掉
    ///（日志失败不失败请求）。
    async fn log(&self, log: RequestLog) -> Result<(), GateError>;

    /// 背压丢弃计数：channel 满被丢的日志条数。直写实现恒 0（默认）。
    fn dropped_logs(&self) -> u64 {
        0
    }

    /// secret gate fail-closed 拒发计数：出站密钥扫描拒发而丢弃的日志
    /// 条数。直写实现恒 0（默认）。
    fn secret_scan_dropped(&self) -> u64 {
        0
    }

    /// 关停写入前沿（幂等）：管道实现补最终落盘后退出；直写实现默认
    /// 无操作。生产停机的最终 flush 由装配层经具体句柄承担，不依赖此方法。
    async fn shutdown(&self) -> Result<(), GateError> {
        Ok(())
    }

    /// admin 读：按过滤查日志。管道前沿默认不支持——admin 恒经存储后端
    /// 实现调用。
    async fn query_logs(&self, _filter: &LogFilter) -> Result<Vec<RequestLog>, GateError> {
        Err(unsupported_query())
    }

    /// admin 读：关键字搜索日志。
    async fn search_logs(&self, _keyword: &str, _limit: u32) -> Result<Vec<RequestLog>, GateError> {
        Err(unsupported_query())
    }
}

/// Token 用量存储缝：数据面预聚合写入 + admin 统计查询。
#[async_trait::async_trait]
pub trait UsageSink: Send + Sync {
    /// 记录原始 usage（记录即返回，聚合与 flush 由实现承担；成本单位 USD）。
    /// 错误上抛由调用方记 warn 后吞掉（记账失败不失败请求）。
    async fn record_raw(
        &self,
        vendor: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
    ) -> Result<(), GateError>;

    /// 把缓冲中的聚合行显式落库（幂等；测试与运维的确定性落盘点）。
    /// 直写实现默认无操作。
    async fn flush(&self) -> Result<(), GateError> {
        Ok(())
    }

    /// admin 读：按厂商查预聚合统计（None = 全部厂商）。管道前沿默认不支持。
    async fn get_token_stats(
        &self,
        _vendor: Option<&str>,
    ) -> Result<Vec<TokenUsageRow>, GateError> {
        Err(unsupported_usage_query())
    }
}

fn unsupported_query() -> GateError {
    GateError::Config {
        message: "this RequestLogSink implementation is a data-plane write front and does not support queries (admin reads go through the storage backend implementation)".to_string(),
    }
}

fn unsupported_usage_query() -> GateError {
    GateError::Config {
        message: "this UsageSink implementation does not provide stats queries (admin reads go through the storage backend implementation)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    /// 可注入失败与丢弃计数的写前沿 mock（模拟异步日志 channel 语义）
    #[derive(Default)]
    struct MockLogSink {
        logs: Mutex<Vec<RequestLog>>,
        dropped: AtomicU64,
        fail: AtomicBool,
    }

    #[async_trait::async_trait]
    impl RequestLogSink for MockLogSink {
        async fn log(&self, log: RequestLog) -> Result<(), GateError> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(GateError::Config {
                    message: "mock write rejected".to_string(),
                });
            }
            self.logs.lock().unwrap().push(log);
            Ok(())
        }

        fn dropped_logs(&self) -> u64 {
            self.dropped.load(Ordering::Relaxed)
        }
    }

    /// 聚合原始 usage 的写前沿 mock（模拟预聚合器语义：按 (vendor, model)
    /// 累加，含错误注入）
    #[derive(Default)]
    struct MockUsageSink {
        tokens: Mutex<Vec<(String, String, u64)>>,
        fail: AtomicBool,
    }

    #[async_trait::async_trait]
    impl UsageSink for MockUsageSink {
        async fn record_raw(
            &self,
            vendor: &str,
            model: &str,
            _prompt_tokens: u32,
            _completion_tokens: u32,
            total_tokens: u32,
            _cost_usd: f64,
        ) -> Result<(), GateError> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(GateError::Config {
                    message: "mock record rejected".to_string(),
                });
            }
            self.tokens.lock().unwrap().push((
                vendor.to_string(),
                model.to_string(),
                total_tokens as u64,
            ));
            Ok(())
        }
    }

    fn sample_log(request_id: &str) -> RequestLog {
        RequestLog {
            id: None,
            request_id: request_id.to_string(),
            timestamp: 1_700_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: None,
            messages: "[]".to_string(),
            actual_vendor: "v".to_string(),
            actual_model: "m".to_string(),
            routing_strategy: "weighted".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            total_tokens: Some(30),
            duration_ms: Some(5),
            cost_usd: None,
        }
    }

    /// 写入路径：log 落入 mock 缓冲，dropped_logs 计数独立可读
    #[tokio::test]
    async fn log_sink_write_and_dropped_counter() {
        let sink = MockLogSink::default();
        sink.dropped.store(7, Ordering::Relaxed);
        sink.log(sample_log("r1")).await.expect("write ok");
        sink.log(sample_log("r2")).await.expect("write ok");
        assert_eq!(sink.logs.lock().unwrap().len(), 2);
        assert_eq!(sink.dropped_logs(), 7);
    }

    /// 错误上抛路径：实现拒绝写入时错误原样上抛，调用方处置
    #[tokio::test]
    async fn log_sink_error_propagates() {
        let sink = MockLogSink::default();
        sink.fail.store(true, Ordering::Relaxed);
        let result = sink.log(sample_log("r1")).await;
        assert!(result.is_err(), "注入失败必须上抛");
    }

    /// 查询默认不支持：管道前沿未覆写查询时得到显式错误而非虚构数据
    #[tokio::test]
    async fn log_sink_query_default_unsupported() {
        let sink = MockLogSink::default();
        let result = sink.query_logs(&LogFilter::default()).await;
        assert!(result.is_err(), "默认查询实现必须显式拒绝");
    }

    /// 聚合写入路径：record_raw 逐条进缓冲；错误注入后上抛且不产生记录
    #[tokio::test]
    async fn usage_sink_record_aggregates_and_error_propagates() {
        let sink = MockUsageSink::default();
        sink.record_raw("v", "m", 1, 2, 3, 0.0)
            .await
            .expect("rec ok");
        sink.record_raw("v", "m", 4, 5, 6, 0.0)
            .await
            .expect("rec ok");
        assert_eq!(
            sink.tokens.lock().unwrap().iter().map(|t| t.2).sum::<u64>(),
            9,
            "聚合语义：total_tokens 可被消费方累加"
        );

        sink.fail.store(true, Ordering::Relaxed);
        let result = sink.record_raw("v", "m", 1, 1, 1, 0.0).await;
        assert!(result.is_err(), "注入失败必须上抛");
        let count = sink.tokens.lock().unwrap().len();
        assert_eq!(count, 2, "被拒记录不得进缓冲");
    }

    /// 统计查询默认不支持（与日志查询同语义）
    #[tokio::test]
    async fn usage_sink_stats_default_unsupported() {
        let sink = MockUsageSink::default();
        assert!(sink.get_token_stats(None).await.is_err());
    }

    /// 契约类型 serde 往返：admin 端点按 JSON 序列化这些行，字段漂移在
    /// 编译/测试期即暴露
    #[test]
    fn storage_contracts_serde_roundtrip() {
        let log = sample_log("serde");
        let json = serde_json::to_string(&log).unwrap();
        let back: RequestLog = serde_json::from_str(&json).unwrap();
        assert_eq!(back.request_id, "serde");
        assert_eq!(back.total_tokens, Some(30));

        let row = TokenUsageRow {
            id: None,
            vendor: "v".to_string(),
            model: "m".to_string(),
            date: "2026-10-04".to_string(),
            hour: 21,
            request_count: 3,
            total_tokens: 9,
            prompt_tokens: 5,
            completion_tokens: 4,
            cost_usd_sum: 0.5,
        };
        let json = serde_json::to_string(&row).unwrap();
        let back: TokenUsageRow = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hour, 21);
        assert_eq!(back.request_count, 3);
    }
}
