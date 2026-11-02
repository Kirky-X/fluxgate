// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OTLP 尝试级追踪（feature `otlp`；与 Prometheus 文本桥 `utils/otlp.rs`
//! 职责分离——桥是 metrics 面，本模块是 trace 面）。
//!
//! 语义（delta spec R-otlp-001/002）：
//! - 请求入站解析 W3C `traceparent`（无/非法 → 新建 root span）；
//! - 每个 vendor attempt 一个子 span，属性：`vendor`/`model`/`attempt`/
//!   `status_code`/`error_class`/`cost_details.estimate_usd`；fallback 实际
//!   命中者的 span 标 `fluxgate.served=true`；
//! - 出站请求注入 `traceparent`（本尝试 span 上下文）；
//! - 双零默认关：`[otlp].tracing.enabled=false`（不装 provider）或
//!   `sample_ratio=0.0`（全不采样）→ 零 span、零网络导出；
//! - 请求/响应体默认不导出（无 body 事件、无临时文件落盘）。
//!
//! 装配：bootstrap 在 metrics 桥装配点旁挂 [`trace_from_section`] →
//! [`install`]；shutdown 经 teardown coordinator flush + [`uninstall`]
//!（best-effort：flush 失败只告警不阻塞停机，不承诺零丢失）。
//!
//! 采样口径：`TraceIdRatioBased` 按 trace id 确定性判定——对远端父上下文
//! 同样按本地比例重判（不跟随上游采样决策；本地确定性优先，避免
//! `sample_ratio=0.0` 时远端 sampled trace 仍产生 span）。
//!
//! 覆盖面：attempt 级 span 挂 converter 非流式（重试循环）与两条流式
//!（`dispatch_stream_once` 共用段）与透传非流式发送点；request root span
//! 由 api 层 fallback 循环创建（跨 vendor 共享同一 trace），无循环路径由
//! manager 派发入口懒建。

use crate::config::OtlpTracingSection;
use opentelemetry::trace::{
    Span as _, SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    Tracer as _, TracerProvider as _,
};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::{BatchSpanProcessor, Sampler, SdkTracerProvider, Tracer};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// root span 名（request 域）。
const REQUEST_SPAN_NAME: &str = "fluxgate.request";
/// attempt 子 span 名。
const ATTEMPT_SPAN_NAME: &str = "fluxgate.attempt";

/// 尝试级追踪装配参数（`[otlp].tracing` 段映射而来）。
#[derive(Debug, Clone)]
pub struct TraceOptions {
    pub sample_ratio: f64,
    pub endpoint: String,
    pub protocol: String,
    pub timeout_secs: u64,
}

/// 装配裁决（与 metrics 桥 [`crate::utils::otlp::BridgeSpawn`] 同形）：
/// Disabled = 运行时半边关；Ready = 可 install；Failed = exporter 构建
/// 失败（fail-open：停用追踪，服务不受影响，调用方负责 i18n 告警）。
#[derive(Debug)]
pub enum TraceSpawn {
    Disabled,
    Ready(Arc<TraceController>),
    Failed(String),
}

/// 装配期一次性裁决（启动期启用与否即定；enabled=false 不建任何 OTel 资源）。
pub fn trace_from_section(section: &OtlpTracingSection) -> TraceSpawn {
    if !section.enabled {
        return TraceSpawn::Disabled;
    }
    match TraceController::new(TraceOptions {
        sample_ratio: section.sample_ratio,
        endpoint: section.endpoint.clone(),
        protocol: section.protocol.clone(),
        timeout_secs: section.timeout_secs,
    }) {
        Ok(controller) => TraceSpawn::Ready(Arc::new(controller)),
        Err(e) => TraceSpawn::Failed(e.to_string()),
    }
}

/// 追踪控制器：SdkTracerProvider + fluxgate tracer（本地持有，不进
/// opentelemetry global——测试可逐实例注入 in-memory exporter）。
pub struct TraceController {
    provider: SdkTracerProvider,
    tracer: Tracer,
}

impl std::fmt::Debug for TraceController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceController").finish_non_exhaustive()
    }
}

impl TraceController {
    /// 构建真实 exporter（http-proto 或 grpc-tonic 传输）并装配 provider。
    pub fn new(options: TraceOptions) -> Result<Self, opentelemetry_otlp::ExporterBuildError> {
        let timeout = Duration::from_secs(options.timeout_secs.max(1));
        let exporter = if options.protocol.as_str() == "grpc" {
            SpanExporter::builder()
                .with_tonic()
                .with_endpoint(options.endpoint)
                .with_timeout(timeout)
                .build()?
        } else {
            SpanExporter::builder()
                .with_http()
                .with_endpoint(options.endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(timeout)
                .build()?
        };
        let processor = BatchSpanProcessor::builder(exporter).build();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::TraceIdRatioBased(
                options.sample_ratio.clamp(0.0, 1.0),
            ))
            .with_span_processor(processor)
            .build();
        Ok(Self::with_provider(provider))
    }

    /// 以既有 provider 装配（测试注入 InMemorySpanExporter 用）。
    pub(crate) fn with_provider(provider: SdkTracerProvider) -> Self {
        let tracer = provider.tracer("fluxgate.trace");
        Self { provider, tracer }
    }

    /// 立即触发一次采集+导出（停机 flush 复用）。
    pub fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.provider.force_flush()
    }

    /// 停机：停 processor。
    pub fn shutdown(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.provider.shutdown()
    }

    fn start_root(&self, traceparent: Option<&str>) -> opentelemetry_sdk::trace::Span {
        match traceparent.and_then(parse_traceparent) {
            Some(remote) => {
                let cx = opentelemetry::Context::current().with_remote_span_context(remote);
                self.tracer
                    .span_builder(REQUEST_SPAN_NAME)
                    .start_with_context(&self.tracer, &cx)
            }
            None => self.tracer.start(REQUEST_SPAN_NAME),
        }
    }

    fn start_attempt(
        &self,
        parent: Option<&SpanContext>,
        vendor: &str,
        model: &str,
        attempt: u32,
    ) -> opentelemetry_sdk::trace::Span {
        let attrs = vec![
            KeyValue::new("vendor", vendor.to_string()),
            KeyValue::new("model", model.to_string()),
            KeyValue::new("attempt", i64::from(attempt)),
        ];
        let builder = self
            .tracer
            .span_builder(ATTEMPT_SPAN_NAME)
            .with_attributes(attrs);
        match parent.filter(|sc| sc.is_valid()) {
            Some(sc) => {
                let cx = opentelemetry::Context::current().with_remote_span_context(sc.clone());
                builder.start_with_context(&self.tracer, &cx)
            }
            None => builder.start(&self.tracer),
        }
    }
}

/// 进程级控制器持有（bootstrap install；shutdown uninstall）。None =
/// 未装配（追踪整体关闭，span 创建全部 no-op）。
static CONTROLLER: RwLock<Option<Arc<TraceController>>> = RwLock::new(None);

/// 装配控制器（bootstrap 一次；重复 install 以最后一次为准）。
pub fn install(controller: Arc<TraceController>) {
    *CONTROLLER.write().unwrap_or_else(|p| p.into_inner()) = Some(controller);
}

/// 撤除控制器（shutdown；此后 span 创建恒 no-op）。
pub fn uninstall() {
    *CONTROLLER.write().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 当前控制器（None = 未装配）。
pub fn installed() -> Option<Arc<TraceController>> {
    CONTROLLER.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// 尝试成本估算面：`f(model, vendor_type, input) -> Option<usd>`（vendored
/// 闭包，由 manager 注入计价能力——本模块零 vendor 依赖）。
pub type Estimator = Arc<dyn Fn(&str, &str, EstimateInput) -> Option<f64> + Send + Sync>;

/// 估算输入：成功尝试带响应 usage；失败尝试带请求字节数（输入腿估算）。
#[derive(Debug, Clone)]
pub enum EstimateInput {
    Usage(crate::protocol::TokenUsage),
    RequestBytes(usize),
}

/// 请求域追踪句柄：root span + 派生父上下文 + 尝试估算面。Arc 共享
///（options.trace 与 fallback 循环/派发入口同源），任一持有者在请求终点
/// 调 [`RequestTrace::end`]（Mutex 承载 end 幂等：先到先终结）。
pub struct RequestTrace {
    root: std::sync::Mutex<opentelemetry_sdk::trace::Span>,
    parent: SpanContext,
    estimator: Option<Estimator>,
}

impl std::fmt::Debug for RequestTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestTrace")
            .field("trace_id", &format!("{:032x}", self.parent.trace_id()))
            .finish_non_exhaustive()
    }
}

impl RequestTrace {
    /// 入站解析 `traceparent`（无/非法 → 新建 root）。未装配 → None。
    /// Arc 返回：root 由 fallback 循环与派发入口共享（options.trace 同源）。
    pub fn start(traceparent: Option<&str>, estimator: Option<Estimator>) -> Option<Arc<Self>> {
        let controller = installed()?;
        let root = controller.start_root(traceparent);
        let parent = root.span_context().clone();
        Some(Arc::new(Self {
            root: std::sync::Mutex::new(root),
            parent,
            estimator,
        }))
    }

    /// root 上下文（attempt 子 span 的父）。
    pub fn span_context(&self) -> &SpanContext {
        &self.parent
    }

    /// 出站注入面：root 上下文的 W3C `traceparent`（无效 → None 不注入）。
    pub fn traceparent_header(&self) -> Option<String> {
        if self.parent.is_valid() {
            Some(format_traceparent(&self.parent))
        } else {
            None
        }
    }

    /// 尝试成本估算（未装配估算面 → None）。
    pub fn estimate_usd(
        &self,
        model: &str,
        vendor_type: &str,
        input: EstimateInput,
    ) -> Option<f64> {
        let estimator = self.estimator.as_ref()?;
        estimator(model, vendor_type, input)
    }

    /// 请求终点：success → span OK；失败带错误分类 → span Error。
    pub fn end(&self, success: bool, error_class: Option<&str>) {
        let mut root = self.root.lock().unwrap_or_else(|p| p.into_inner());
        if success {
            root.set_status(opentelemetry::trace::Status::Ok);
        } else {
            let class = error_class.unwrap_or("error");
            root.set_attribute(KeyValue::new("error_class", class.to_string()));
            root.set_status(opentelemetry::trace::Status::error(class.to_string()));
        }
        root.end();
    }
}

/// 单个 vendor attempt 的子 span（创建即计 attempt 序号；`finish` 写终态
/// 属性并终结）。未装配控制器时恒 no-op。
pub struct AttemptSpan {
    span: Option<opentelemetry_sdk::trace::Span>,
}

impl AttemptSpan {
    /// 创建 attempt 子 span（`parent` = root 上下文；attempt 从 1 起）。
    pub fn begin(parent: Option<&SpanContext>, vendor: &str, model: &str, attempt: u32) -> Self {
        let Some(controller) = installed() else {
            return Self { span: None };
        };
        Self {
            span: Some(controller.start_attempt(parent, vendor, model, attempt)),
        }
    }

    /// 出站注入面：本尝试的 W3C `traceparent`（无效 → None 不注入）。
    pub fn traceparent_header(&self) -> Option<String> {
        let span = self.span.as_ref()?;
        let sc = span.span_context();
        if !sc.is_valid() {
            return None;
        }
        Some(format_traceparent(sc))
    }

    /// 终结 attempt：served 标记 + 状态码 + 错误分类 + 成本估算。
    pub fn finish(
        self,
        status_code: Option<u16>,
        error_class: Option<&str>,
        served: bool,
        estimate_usd: Option<f64>,
    ) {
        let Some(mut span) = self.span else { return };
        span.set_attribute(KeyValue::new("fluxgate.served", served));
        if let Some(code) = status_code {
            span.set_attribute(KeyValue::new("status_code", i64::from(code)));
        }
        if let Some(class) = error_class {
            span.set_attribute(KeyValue::new("error_class", class.to_string()));
            span.set_status(opentelemetry::trace::Status::error(class.to_string()));
        } else {
            span.set_status(opentelemetry::trace::Status::Ok);
        }
        if let Some(usd) = estimate_usd {
            span.set_attribute(KeyValue::new("cost_details.estimate_usd", usd));
        }
        span.end();
    }
}

/// 解析 W3C `traceparent`（`00-<32hex>-<16hex>-<2hex>`；仅支持 0.0 版——
/// `ff` 与其他版本号视同无父；trace/span id 全零拒绝）。
pub(crate) fn parse_traceparent(value: &str) -> Option<SpanContext> {
    let parts: Vec<&str> = value.trim().split('-').collect();
    if parts.len() != 4 || parts[0] != "00" {
        return None;
    }
    let (trace_hex, span_hex, flags_hex) = (parts[1], parts[2], parts[3]);
    let all_hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !all_hex(trace_hex, 32) || !all_hex(span_hex, 16) || !all_hex(flags_hex, 2) {
        return None;
    }
    let trace_id = TraceId::from_hex(trace_hex).ok()?;
    let span_id = SpanId::from_hex(span_hex).ok()?;
    if trace_id == TraceId::INVALID || span_id == SpanId::INVALID {
        return None;
    }
    let flags = u8::from_str_radix(flags_hex, 16).ok()?;
    let state = TraceState::from_key_value(Vec::<(String, String)>::new()).ok()?;
    Some(SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags),
        true,
        state,
    ))
}

/// W3C `traceparent` 序列化（version 00；flags 原样保留）。
pub(crate) fn format_traceparent(sc: &SpanContext) -> String {
    format!(
        "00-{trace_id:032x}-{span_id:016x}-{flags:02x}",
        trace_id = sc.trace_id(),
        span_id = sc.span_id(),
        flags = sc.trace_flags().to_u8()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SimpleSpanProcessor};
    use std::sync::Mutex;

    /// 全局 holder 进程内单例：span 断言类测试串行（install 互斥）。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 装配 in-memory 控制器并 install；返回 (exporter, controller)。
    fn install_in_memory(ratio: f64) -> (Arc<InMemorySpanExporter>, Arc<TraceController>) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::TraceIdRatioBased(ratio))
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let controller = Arc::new(TraceController::with_provider(provider));
        install(Arc::clone(&controller));
        (Arc::new(exporter), controller)
    }

    fn attr_bool(kv: &opentelemetry::KeyValue) -> Option<bool> {
        match &kv.value {
            opentelemetry::Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn attr_f64(kv: &opentelemetry::KeyValue) -> Option<f64> {
        match &kv.value {
            opentelemetry::Value::F64(f) => Some(*f),
            opentelemetry::Value::I64(i) => Some(*i as f64),
            _ => None,
        }
    }

    type FinishedSpans = Vec<opentelemetry_sdk::trace::SpanData>;

    fn flush_and_spans(
        controller: &TraceController,
        exporter: &InMemorySpanExporter,
    ) -> FinishedSpans {
        controller.force_flush().expect("flush");
        exporter.get_finished_spans().expect("spans")
    }

    /// R-otlp-001 验收：单请求 N 次 fallback → N+1（root+attempt）个 span
    /// 且共享同一 trace id；served 属性只在最终成功者上。
    #[test]
    fn fallback_n_attempts_share_root_trace_and_served_only_final() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (exporter, controller) = install_in_memory(1.0);
        let trace = RequestTrace::start(None, None).expect("trace");
        let root_sc = trace.span_context().clone();
        for attempt in 1..=3u32 {
            let span = AttemptSpan::begin(Some(&root_sc), "vendor-a", "gpt-test", attempt);
            let served = attempt == 3;
            span.finish(
                Some(if served { 200 } else { 503 }),
                if served { None } else { Some("http") },
                served,
                if served { Some(0.0123) } else { None },
            );
        }
        trace.end(true, None);
        let spans = flush_and_spans(&controller, &exporter);
        assert_eq!(spans.len(), 4, "root + 3 attempts = 4 spans");
        let trace_ids: std::collections::HashSet<_> =
            spans.iter().map(|s| s.span_context.trace_id()).collect();
        assert_eq!(trace_ids.len(), 1, "全部 span 共享同一 trace id");
        let root = spans
            .iter()
            .find(|s| s.name == REQUEST_SPAN_NAME)
            .expect("root");
        let attempts: Vec<_> = spans
            .iter()
            .filter(|s| s.name == ATTEMPT_SPAN_NAME)
            .collect();
        assert_eq!(attempts.len(), 3);
        let served_flags: Vec<bool> = attempts
            .iter()
            .map(|s| {
                s.attributes
                    .iter()
                    .find(|kv| kv.key.as_str() == "fluxgate.served")
                    .and_then(attr_bool)
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            served_flags,
            vec![false, false, true],
            "served 只标最终成功者"
        );
        let estimate = attempts[2]
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == "cost_details.estimate_usd")
            .and_then(attr_f64);
        assert_eq!(estimate, Some(0.0123), "估算成本属性在位");
        assert_eq!(
            attempts[2].span_context.trace_id(),
            root.span_context.trace_id(),
            "attempt 是 root 的子 span（同 trace）"
        );
        uninstall();
    }

    /// R-otlp-001/002 验收：sample_ratio=0.0（默认）零 span 产生。
    #[test]
    fn default_ratio_zero_produces_no_spans() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (exporter, controller) = install_in_memory(0.0);
        let trace = RequestTrace::start(None, None).expect("trace");
        let span = AttemptSpan::begin(Some(trace.span_context()), "vendor-a", "gpt-test", 1);
        span.finish(Some(200), None, true, None);
        trace.end(true, None);
        let spans = flush_and_spans(&controller, &exporter);
        assert!(spans.is_empty(), "ratio 0.0 必须零 span: {}", spans.len());
        uninstall();
    }

    /// R-otlp-002 验收：enabled=false 装配裁决 Disabled（不建任何资源）。
    #[test]
    fn disabled_section_yields_disabled_spawn() {
        let section = OtlpTracingSection {
            enabled: false,
            ..Default::default()
        };
        assert!(matches!(trace_from_section(&section), TraceSpawn::Disabled));
    }

    /// 未装配控制器（uninstall 态）：span 创建全 no-op。
    #[test]
    fn uninstalled_controller_no_ops() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        uninstall();
        assert!(RequestTrace::start(None, None).is_none());
        let attempt = AttemptSpan::begin(None, "v", "m", 1);
        attempt.finish(Some(200), None, true, None);
        assert!(AttemptSpan::begin(None, "v", "m", 1)
            .traceparent_header()
            .is_none());
    }

    /// 入站 traceparent 解析 → root 延续同一 trace id；非法形态新建 root。
    #[test]
    fn inbound_traceparent_extends_remote_trace() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (exporter, controller) = install_in_memory(1.0);
        let remote = parse_traceparent("00-11111111111111111111111111111111-2222222222222222-01")
            .expect("合法形态");
        let trace = RequestTrace::start(
            Some("00-11111111111111111111111111111111-2222222222222222-01"),
            None,
        )
        .expect("trace");
        assert_eq!(trace.span_context().trace_id(), remote.trace_id());
        trace.end(true, None);
        let spans = flush_and_spans(&controller, &exporter);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].span_context.trace_id(),
            remote.trace_id(),
            "root 延续远端 trace"
        );
        for bad in [
            "ff-11111111111111111111111111111111-2222222222222222-01",
            "00-00000000000000000000000000000000-2222222222222222-01",
            "00-xyz-2222222222222222-01",
            "00-11111111111111111111111111111111-2222222222222222",
            "",
        ] {
            assert!(parse_traceparent(bad).is_none(), "非法形态必须拒绝: {bad}");
        }
        uninstall();
    }

    /// 出站注入面：attempt span 产出可回读的 traceparent（round-trip）。
    #[test]
    fn attempt_traceparent_round_trips() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_exporter, _controller) = install_in_memory(1.0);
        let trace = RequestTrace::start(None, None).expect("trace");
        let attempt = AttemptSpan::begin(Some(trace.span_context()), "vendor-a", "gpt-test", 1);
        let header = attempt.traceparent_header().expect("header");
        let parsed = parse_traceparent(&header).expect("可解析");
        assert_eq!(parsed.trace_id(), trace.span_context().trace_id());
        assert!(parsed.trace_flags().is_sampled(), "采样位随 span 传播");
        attempt.finish(Some(200), None, true, None);
        trace.end(true, None);
        uninstall();
    }

    /// 估算面注入：usage 与 RequestBytes 两形态都进入
    /// cost_details.estimate_usd 属性。
    #[test]
    fn estimator_flows_into_attempt_attribute() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (exporter, controller) = install_in_memory(1.0);
        let estimator: Estimator = Arc::new(|model, _vendor_type, input| {
            assert_eq!(model, "gpt-test");
            Some(match input {
                EstimateInput::Usage(u) => f64::from(u.prompt_tokens) / 100.0,
                EstimateInput::RequestBytes(bytes) => bytes as f64 / 200.0,
            })
        });
        let trace = RequestTrace::start(None, Some(estimator)).expect("trace");
        let sc = trace.span_context().clone();
        let usage = crate::protocol::TokenUsage {
            prompt_tokens: 100,
            ..Default::default()
        };
        let est_usage = trace.estimate_usd("gpt-test", "openai", EstimateInput::Usage(usage));
        let est_bytes = trace.estimate_usd("gpt-test", "openai", EstimateInput::RequestBytes(200));
        assert_eq!(est_usage, Some(1.0));
        assert_eq!(est_bytes, Some(1.0));
        let span = AttemptSpan::begin(Some(&sc), "vendor-a", "gpt-test", 1);
        span.finish(Some(200), None, true, est_usage);
        trace.end(true, None);
        let spans = flush_and_spans(&controller, &exporter);
        let attempt = spans
            .iter()
            .find(|s| s.name == ATTEMPT_SPAN_NAME)
            .expect("attempt");
        let got = attempt
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == "cost_details.estimate_usd")
            .and_then(attr_f64);
        assert_eq!(got, Some(1.0));
        uninstall();
    }
}
