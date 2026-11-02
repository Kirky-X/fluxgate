// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OTLP 桥接的 exposition 解析层。
//!
//! 单一真源为 `sdforge::metrics::global_registry().render()`（与 `/metrics`
//! 端点同源）。本模块把 Prometheus text exposition 解析为结构化样本供桥接
//! 层注册到 SdkMeterProvider。约束：畸形行计数不中断、summary 家族跳过并
//! 计数、无 TYPE 行的序列 fail-open 按 gauge 处理。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry::metrics::{AsyncInstrument, MeterProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{ExporterBuildError, MetricExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

use crate::utils::cost_tracker::{CostSnapshot, CostTracker};

/// 样本标签集，保持 exposition 中的出现顺序。
pub type Labels = Vec<(String, String)>;

/// 解析出的单个指标样本。histogram 的桶值为 Prometheus 累积计数原样透传，
/// `le` 保留原文（含 `+Inf`）。
#[derive(Debug, Clone, PartialEq)]
pub enum MetricSample {
    Counter {
        name: String,
        value: f64,
        labels: Labels,
    },
    Gauge {
        name: String,
        value: f64,
        labels: Labels,
    },
    Histogram {
        name: String,
        labels: Labels,
        buckets: Vec<(String, u64)>,
        sum: f64,
        count: u64,
    },
}

/// 解析统计：跳过的 summary 采样行数与畸形行数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    pub skipped_summary_lines: usize,
    pub malformed_lines: usize,
}

/// 解析 exposition 文本为样本序列（不关心统计口径时使用）。
pub fn parse_exposition(text: &str) -> Vec<MetricSample> {
    parse_exposition_with_stats(text).0
}

/// 完整解析：样本序列 + 跳过/畸形统计。
///
/// 两遍法：先收集 `# TYPE` 行建立家族类型表，再逐行解析采样行。histogram
/// 按（基名，去 le 标签集）分组聚合 `_bucket`/`_sum`/`_count`；summary 家族
/// 采样行跳过并计数；无 TYPE 行或未知类型的序列 fail-open 按 gauge 输出。
pub fn parse_exposition_with_stats(text: &str) -> (Vec<MetricSample>, ParseStats) {
    let mut stats = ParseStats::default();
    let types = collect_types(text);
    let mut samples = Vec::new();

    struct Histo {
        labels: Labels,
        buckets: Vec<(String, u64)>,
        sum: f64,
        count: u64,
    }
    let mut groups: Vec<(String, Histo)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, rest) = split_name(line);
        if name.is_empty() {
            stats.malformed_lines += 1;
            continue;
        }
        let (labels, after) = match rest.strip_prefix('{') {
            Some(braced) => match parse_labels(braced) {
                Some((labels, after)) => (labels, after),
                None => {
                    stats.malformed_lines += 1;
                    continue;
                }
            },
            None => (Labels::new(), rest),
        };
        let value = match first_token(after.trim_start()).0.parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                stats.malformed_lines += 1;
                continue;
            }
        };

        let kind_of = |n: &str| types.get(n).copied();
        let le = labels
            .iter()
            .find(|(k, _)| k == "le")
            .map(|(_, v)| v.clone());

        if let Some(base) = name.strip_suffix("_bucket").filter(|_| le.is_some()) {
            match kind_of(base) {
                Some("histogram") => {
                    let group_labels: Labels =
                        labels.iter().filter(|(k, _)| k != "le").cloned().collect();
                    let key = group_key(base, &group_labels);
                    let idx = *index.entry(key).or_insert_with(|| {
                        groups.push((
                            base.to_string(),
                            Histo {
                                labels: group_labels,
                                buckets: Vec::new(),
                                sum: 0.0,
                                count: 0,
                            },
                        ));
                        groups.len() - 1
                    });
                    groups[idx]
                        .1
                        .buckets
                        .push((le.expect("checked above"), value as u64));
                    continue;
                }
                Some("summary") => {
                    stats.skipped_summary_lines += 1;
                    continue;
                }
                _ => {}
            }
        } else if let Some(base) = name
            .strip_suffix("_sum")
            .or_else(|| name.strip_suffix("_count"))
        {
            match kind_of(base) {
                Some("histogram") => {
                    let key = group_key(base, &labels);
                    let idx = *index.entry(key).or_insert_with(|| {
                        groups.push((
                            base.to_string(),
                            Histo {
                                labels: labels.clone(),
                                buckets: Vec::new(),
                                sum: 0.0,
                                count: 0,
                            },
                        ));
                        groups.len() - 1
                    });
                    if name.ends_with("_sum") {
                        groups[idx].1.sum = value;
                    } else {
                        groups[idx].1.count = value as u64;
                    }
                    continue;
                }
                Some("summary") => {
                    stats.skipped_summary_lines += 1;
                    continue;
                }
                _ => {}
            }
        }

        match kind_of(name) {
            Some("counter") => samples.push(MetricSample::Counter {
                name: name.to_string(),
                value,
                labels,
            }),
            Some("summary") => stats.skipped_summary_lines += 1,
            _ => samples.push(MetricSample::Gauge {
                name: name.to_string(),
                value,
                labels,
            }),
        }
    }

    for (base, h) in groups {
        samples.push(MetricSample::Histogram {
            name: base,
            labels: h.labels,
            buckets: h.buckets,
            sum: h.sum,
            count: h.count,
        });
    }
    (samples, stats)
}

/// 收集 `# TYPE <名> <类型>` 行（HELP/UNIT/注释忽略）。
fn collect_types(text: &str) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix("TYPE") else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(char::is_whitespace) else {
            continue;
        };
        let Some((name, kind)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        map.insert(name.trim(), kind.trim());
    }
    map
}

/// 切出指标名与其余部分（`{` 或空白为界）。
fn split_name(line: &str) -> (&str, &str) {
    match line.find(['{', ' ', '\t']) {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    }
}

/// 取首个空白分隔 token。
fn first_token(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

type CharPeek<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

fn skip_spaces(it: &mut CharPeek<'_>) {
    while matches!(it.peek(), Some((_, c)) if c.is_whitespace()) {
        it.next();
    }
}

/// 解析标签对直至 `}`（入参不含前导 `{`，含 `\\` `\"` `\n` 转义），返回
/// 标签集与 `}` 之后的余文；未闭合/格式非法返回 None。
fn parse_labels(braced: &str) -> Option<(Labels, &str)> {
    let mut labels = Labels::new();
    let mut it = braced.char_indices().peekable();
    loop {
        skip_spaces(&mut it);
        match it.peek().copied() {
            Some((_, '}')) => {
                it.next();
                let pos = it.peek().map_or(braced.len(), |(i, _)| *i);
                return Some((labels, &braced[pos..]));
            }
            None => return None,
            _ => {}
        }
        let mut key = String::new();
        while let Some((_, c)) = it.peek() {
            if c.is_alphanumeric() || *c == '_' {
                key.push(*c);
                it.next();
            } else {
                break;
            }
        }
        if key.is_empty() {
            return None;
        }
        skip_spaces(&mut it);
        if !matches!(it.next(), Some((_, '='))) {
            return None;
        }
        skip_spaces(&mut it);
        if !matches!(it.next(), Some((_, '"'))) {
            return None;
        }
        let mut value = String::new();
        loop {
            match it.next() {
                Some((_, '"')) => break,
                Some((_, '\\')) => match it.next() {
                    Some((_, '\\')) => value.push('\\'),
                    Some((_, '"')) => value.push('"'),
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, other)) => {
                        value.push('\\');
                        value.push(other);
                    }
                    None => return None,
                },
                Some((_, c)) => value.push(c),
                None => return None,
            }
        }
        labels.push((key, value));
        skip_spaces(&mut it);
        match it.peek().copied() {
            Some((_, ',')) => {
                it.next();
            }
            Some((_, '}')) => {}
            _ => return None,
        }
    }
}

/// histogram 分组键：基名 + 排序后的标签对（控制字符分隔防碰撞）。
fn group_key(base: &str, labels: &[(String, String)]) -> String {
    let mut pairs: Vec<&(String, String)> = labels.iter().collect();
    pairs.sort();
    let joined: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}\u{1}{v}")).collect();
    format!("{base}\u{0}{}", joined.join("\u{2}"))
}

/// OTLP 导出参数（`[otlp]` 配置段映射而来，见 T004）。
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub endpoint: String,
    pub interval_secs: u64,
    pub timeout_secs: u64,
}

/// 装配裁决：Disabled = 运行时半边关（不建 provider/线程）；Ready = 可 spawn
/// 轮询任务；Failed = exporter 构建失败（fail-open：停用导出，服务不受影响，
/// 调用方负责 i18n 告警）。
#[derive(Debug)]
pub enum BridgeSpawn {
    Disabled,
    Ready(Arc<OtlpBridge>),
    Failed(String),
}

/// 装配期一次性裁决（启动期启用与否即定；enabled=false 不建任何 OTel 资源）。
pub fn bridge_from_section(
    section: &crate::config::OtlpSection,
    cost_tracker: Arc<CostTracker>,
) -> BridgeSpawn {
    if !section.enabled {
        return BridgeSpawn::Disabled;
    }
    match OtlpBridge::new(
        BridgeOptions {
            endpoint: section.endpoint.clone(),
            interval_secs: section.interval_secs,
            timeout_secs: section.timeout_secs,
        },
        cost_tracker,
    ) {
        Ok(bridge) => BridgeSpawn::Ready(Arc::new(bridge)),
        Err(e) => BridgeSpawn::Failed(e.to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeriesKind {
    Counter,
    Gauge,
}

/// 单个时间序列点：标签对（保序）+ 值。
#[derive(Debug, Clone)]
struct SeriesPoint {
    attrs: Vec<(String, String)>,
    value: f64,
}

/// OTLP 桥：把 `/metrics` 渲染文本（单一真源）解析为观测型仪表回读缓存，
/// CostTracker 快照单独出 `fluxgate.cost_usd_total`（counter）与
/// `fluxgate.cost_usd`（gauge）。成本点集：无 label 总点 + `{vendor}` 点 +
/// `{model}` 点（快照只有边际维度，无配对维度）。
pub struct OtlpBridge {
    provider: SdkMeterProvider,
    meter: opentelemetry::metrics::Meter,
    store: Arc<Mutex<HashMap<String, Vec<SeriesPoint>>>>,
    registered: Mutex<HashSet<String>>,
    cost_tracker: Arc<CostTracker>,
    /// 导出一轮失败累计（接收端不可达 / 非 2xx / 超时）。fail-open 观测面：
    /// 桥自身持有，随 `update_from_text` 以 gauge 形式并入导出，不进请求路径。
    export_fail_total: std::sync::atomic::AtomicU64,
}

impl OtlpBridge {
    /// 构建真实 OTLP exporter（http-proto + blocking client）并装配 provider。
    pub fn new(
        options: BridgeOptions,
        cost_tracker: Arc<CostTracker>,
    ) -> Result<Self, ExporterBuildError> {
        let exporter = MetricExporter::builder()
            .with_http()
            .with_endpoint(options.endpoint)
            .with_protocol(Protocol::HttpBinary)
            .with_timeout(Duration::from_secs(options.timeout_secs))
            .build()?;
        let reader = PeriodicReader::builder(exporter)
            .with_interval(Duration::from_secs(options.interval_secs))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        Ok(Self::with_provider(provider, cost_tracker))
    }

    /// 以既有 provider 装配（测试注入 InMemoryMetricExporter 用）。
    fn with_provider(provider: SdkMeterProvider, cost_tracker: Arc<CostTracker>) -> Self {
        let meter = provider.meter("fluxgate.otlp");
        let bridge = Self {
            provider,
            meter,
            store: Arc::new(Mutex::new(HashMap::new())),
            registered: Mutex::new(HashSet::new()),
            cost_tracker,
            export_fail_total: std::sync::atomic::AtomicU64::new(0),
        };
        bridge.register_cost_instruments();
        bridge
    }

    /// 解析 exposition 文本并刷新观测缓存（新名懒注册仪表）。
    ///
    /// 每次全量替换 store：消失的序列回调读不到点即不再导出；已注册名不重复
    /// 注册（回调读最新点集）。
    pub fn update_from_text(&self, text: &str) {
        let (samples, _stats) = parse_exposition_with_stats(text);
        let mut fresh: Vec<(String, SeriesKind)> = Vec::new();
        {
            let mut store = self.lock_store();
            store.clear();
            for sample in &samples {
                for (name, kind, point) in decompose(sample) {
                    match store.get_mut(&name) {
                        Some(points) => points.push(point),
                        None => {
                            store.insert(name.clone(), vec![point]);
                            fresh.push((name, kind));
                        }
                    }
                }
            }
        }
        for (name, kind) in fresh {
            self.ensure_registered(&name, kind);
        }
        self.publish_export_fail();
    }

    /// 把本桥的导出失败计数并入 store（`fluxgate.otlp_export_fail_total`），
    /// 使接收端能观测到「桥自己失效」这件事——否则端点挂掉后该计数永远
    /// 只存在于进程内存。
    fn publish_export_fail(&self) {
        let total = self.export_fail_total();
        let name = "fluxgate.otlp_export_fail_total";
        let is_new = {
            let mut store = self.lock_store();
            store
                .insert(
                    name.to_string(),
                    vec![SeriesPoint {
                        attrs: Vec::new(),
                        value: total as f64,
                    }],
                )
                .is_none()
        };
        if is_new {
            self.ensure_registered(name, SeriesKind::Counter);
        }
    }

    /// 清空观测缓存：运行期关闭导出时调用，使下一轮 reader 采集无点可发。
    pub fn clear_series(&self) {
        self.lock_store().clear();
    }

    /// 从默认文本源（/metrics 同源渲染）拉取一次。
    pub fn poll_once(&self) {
        self.update_from_text(&sdforge::metrics::global_registry().render());
    }

    /// 立即触发一次采集+导出（停机 flush 复用）。
    pub fn force_flush(&self) -> OTelSdkResult {
        let result = self.provider.force_flush();
        if result.is_err() {
            self.record_export_fail();
        }
        result
    }

    /// 停机：停止周期 reader 线程。
    pub fn shutdown(&self) -> OTelSdkResult {
        let result = self.provider.shutdown();
        if result.is_err() {
            self.record_export_fail();
        }
        result
    }

    /// 导出一轮失败计数（fail-open：只观测，不改控制流）。
    fn record_export_fail(&self) {
        self.export_fail_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// 本桥累计导出失败次数。
    pub fn export_fail_total(&self) -> u64 {
        self.export_fail_total
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn lock_store(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<SeriesPoint>>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_registered(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 首次出现的序列名注册观测仪表（回调回读 store 最新点集）。
    fn ensure_registered(&self, name: &str, kind: SeriesKind) {
        if !self.lock_registered().insert(name.to_string()) {
            return;
        }
        let store = Arc::clone(&self.store);
        let owned = name.to_string();
        let observed = owned.clone();
        let observe = move |inst: &dyn AsyncInstrument<f64>| observe_entry(&store, &observed, inst);
        match kind {
            SeriesKind::Counter => {
                self.meter
                    .f64_observable_counter(owned)
                    .with_callback(observe)
                    .build();
            }
            SeriesKind::Gauge => {
                self.meter
                    .f64_observable_gauge(owned)
                    .with_callback(observe)
                    .build();
            }
        }
    }

    /// CostTracker 快照 → 成本双仪表（累计 counter + 当前 gauge）。
    fn register_cost_instruments(&self) {
        let tracker = Arc::clone(&self.cost_tracker);
        self.meter
            .f64_observable_counter("fluxgate.cost_usd_total")
            .with_description("Cumulative upstream cost in USD")
            .with_callback(move |inst| observe_cost(inst, &tracker.snapshot()))
            .build();
        let tracker = Arc::clone(&self.cost_tracker);
        self.meter
            .f64_observable_gauge("fluxgate.cost_usd")
            .with_description("Upstream cost in USD")
            .with_callback(move |inst| observe_cost(inst, &tracker.snapshot()))
            .build();
    }
}

/// provider/meter 不实现 Debug，且含运行时线程句柄；只暴露可观测的配置面。
impl std::fmt::Debug for OtlpBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtlpBridge")
            .field("series", &self.lock_store().len())
            .field("registered", &self.lock_registered().len())
            .finish_non_exhaustive()
    }
}

fn observe_entry(
    store: &Mutex<HashMap<String, Vec<SeriesPoint>>>,
    name: &str,
    inst: &dyn AsyncInstrument<f64>,
) {
    let guard = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(points) = guard.get(name) {
        for point in points {
            let attrs: Vec<KeyValue> = point
                .attrs
                .iter()
                .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
                .collect();
            inst.observe(point.value, &attrs);
        }
    }
}

fn observe_cost(inst: &dyn AsyncInstrument<f64>, snap: &CostSnapshot) {
    inst.observe(snap.total_usd, &[]);
    for (vendor, usd) in &snap.by_vendor {
        inst.observe(*usd, &[KeyValue::new("vendor", vendor.clone())]);
    }
    for (model, usd) in &snap.by_model {
        inst.observe(*usd, &[KeyValue::new("model", model.clone())]);
    }
}

/// 样本 → OTLP 序列（`fluxgate.` 前缀字面拼接；histogram 分解为
/// `_bucket`/`_sum`/`_count` 三 gauge 序列，le 作标签保留原文）。
fn decompose(sample: &MetricSample) -> Vec<(String, SeriesKind, SeriesPoint)> {
    const PREFIX: &str = "fluxgate.";
    match sample {
        MetricSample::Counter {
            name,
            value,
            labels,
        } => vec![(
            format!("{PREFIX}{name}"),
            SeriesKind::Counter,
            SeriesPoint {
                attrs: labels.clone(),
                value: *value,
            },
        )],
        MetricSample::Gauge {
            name,
            value,
            labels,
        } => vec![(
            format!("{PREFIX}{name}"),
            SeriesKind::Gauge,
            SeriesPoint {
                attrs: labels.clone(),
                value: *value,
            },
        )],
        MetricSample::Histogram {
            name,
            labels,
            buckets,
            sum,
            count,
        } => {
            let mut out = Vec::with_capacity(buckets.len() + 2);
            for (le, cnt) in buckets {
                let mut attrs = labels.clone();
                attrs.push(("le".to_string(), le.clone()));
                out.push((
                    format!("{PREFIX}{name}_bucket"),
                    SeriesKind::Gauge,
                    SeriesPoint {
                        attrs,
                        value: *cnt as f64,
                    },
                ));
            }
            out.push((
                format!("{PREFIX}{name}_sum"),
                SeriesKind::Gauge,
                SeriesPoint {
                    attrs: labels.clone(),
                    value: *sum,
                },
            ));
            out.push((
                format!("{PREFIX}{name}_count"),
                SeriesKind::Gauge,
                SeriesPoint {
                    attrs: labels.clone(),
                    value: *count as f64,
                },
            ));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counter_gauge_histogram() {
        let text = concat!(
            "# HELP test_requests_total Total HTTP requests.\n",
            "# TYPE test_requests_total counter\n",
            "test_requests_total{route=\"/api\",method=\"POST\",status=\"200\"} 42\n",
            "# TYPE test_queue_depth gauge\n",
            "test_queue_depth{host=\"a\"} 3.5\n",
            "# TYPE test_latency_seconds histogram\n",
            "test_latency_seconds_bucket{route=\"/api\",le=\"0.1\"} 1\n",
            "test_latency_seconds_bucket{route=\"/api\",le=\"+Inf\"} 5\n",
            "test_latency_seconds_sum{route=\"/api\"} 0.35\n",
            "test_latency_seconds_count{route=\"/api\"} 5\n",
        );
        let samples = parse_exposition(text);
        assert_eq!(
            samples,
            vec![
                MetricSample::Counter {
                    name: "test_requests_total".into(),
                    value: 42.0,
                    labels: vec![
                        ("route".into(), "/api".into()),
                        ("method".into(), "POST".into()),
                        ("status".into(), "200".into()),
                    ],
                },
                MetricSample::Gauge {
                    name: "test_queue_depth".into(),
                    value: 3.5,
                    labels: vec![("host".into(), "a".into())],
                },
                MetricSample::Histogram {
                    name: "test_latency_seconds".into(),
                    labels: vec![("route".into(), "/api".into())],
                    buckets: vec![("0.1".into(), 1), ("+Inf".into(), 5)],
                    sum: 0.35,
                    count: 5,
                },
            ]
        );
    }

    #[test]
    fn summary_lines_are_skipped_and_counted() {
        let text = concat!(
            "# TYPE test_quantile summary\n",
            "test_quantile{quantile=\"0.5\"} 0.2\n",
            "test_quantile_sum 1.0\n",
            "test_quantile_count 3\n",
        );
        let (samples, stats) = parse_exposition_with_stats(text);
        assert!(samples.is_empty());
        assert_eq!(stats.skipped_summary_lines, 3);
        assert_eq!(stats.malformed_lines, 0);
    }

    #[test]
    fn malformed_lines_are_counted_not_fatal() {
        let text = concat!(
            "good_metric 1\n",
            "bad{unclosed 5\n",
            "no_value\n",
            "bad_value abc\n",
            "good_metric 2\n",
        );
        let (samples, stats) = parse_exposition_with_stats(text);
        assert_eq!(
            samples,
            vec![
                MetricSample::Gauge {
                    name: "good_metric".into(),
                    value: 1.0,
                    labels: vec![],
                },
                MetricSample::Gauge {
                    name: "good_metric".into(),
                    value: 2.0,
                    labels: vec![],
                },
            ]
        );
        assert_eq!(stats.malformed_lines, 3);
        assert_eq!(stats.skipped_summary_lines, 0);
    }

    #[test]
    fn untyped_series_default_to_gauge() {
        let text = "plain_metric 7\ntimed_metric 3 1699999999000\n";
        let samples = parse_exposition(text);
        assert_eq!(
            samples,
            vec![
                MetricSample::Gauge {
                    name: "plain_metric".into(),
                    value: 7.0,
                    labels: vec![],
                },
                MetricSample::Gauge {
                    name: "timed_metric".into(),
                    value: 3.0,
                    labels: vec![],
                },
            ]
        );
    }

    #[test]
    fn label_escape_sequences_unescaped() {
        let text = r#"weird_metric{path="C:\\tmp",msg="line1\nline2",quote="say \"hi\""} 1"#;
        let samples = parse_exposition(text);
        assert_eq!(
            samples,
            vec![MetricSample::Gauge {
                name: "weird_metric".into(),
                value: 1.0,
                labels: vec![
                    ("path".into(), "C:\\tmp".into()),
                    ("msg".into(), "line1\nline2".into()),
                    ("quote".into(), "say \"hi\"".into()),
                ],
            }]
        );
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

    const SDFORGE_TEXT: &str = concat!(
        "# HELP sdforge_http_requests_total Total HTTP requests.\n",
        "# TYPE sdforge_http_requests_total counter\n",
        "sdforge_http_requests_total{route=\"/x\",method=\"GET\",status=\"200\"} 3\n",
        "# HELP sdforge_http_request_duration_seconds HTTP request latency.\n",
        "# TYPE sdforge_http_request_duration_seconds histogram\n",
        "sdforge_http_request_duration_seconds_bucket{route=\"/x\",method=\"GET\",le=\"0.001\"} 1\n",
        "sdforge_http_request_duration_seconds_bucket{route=\"/x\",method=\"GET\",le=\"+Inf\"} 2\n",
        "sdforge_http_request_duration_seconds_sum{route=\"/x\",method=\"GET\"} 0.032\n",
        "sdforge_http_request_duration_seconds_count{route=\"/x\",method=\"GET\"} 2\n",
    );

    type Points = Vec<(Vec<(String, String)>, f64)>;

    fn attrs<'a>(raw: impl Iterator<Item = &'a KeyValue>) -> Vec<(String, String)> {
        raw.map(|kv| (kv.key.as_str().to_string(), kv.value.as_str().to_string()))
            .collect()
    }

    fn norm(mut pts: Points) -> Points {
        for (a, _) in &mut pts {
            a.sort();
        }
        pts.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)));
        pts
    }

    fn sum_points(rms: &[ResourceMetrics], name: &str) -> Option<Points> {
        for rm in rms {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() == name {
                        return match m.data() {
                            AggregatedMetrics::F64(MetricData::Sum(s)) => Some(
                                s.data_points()
                                    .map(|p| (attrs(p.attributes()), p.value()))
                                    .collect(),
                            ),
                            _ => None,
                        };
                    }
                }
            }
        }
        None
    }

    fn gauge_points(rms: &[ResourceMetrics], name: &str) -> Option<Points> {
        for rm in rms {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() == name {
                        return match m.data() {
                            AggregatedMetrics::F64(MetricData::Gauge(g)) => Some(
                                g.data_points()
                                    .map(|p| (attrs(p.attributes()), p.value()))
                                    .collect(),
                            ),
                            _ => None,
                        };
                    }
                }
            }
        }
        None
    }

    #[test]
    fn exporter_pushes_meter_samples() {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(Duration::from_secs(3600))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let tracker = Arc::new(CostTracker::new());
        tracker.record("openai", "gpt-x", 1.25);
        let bridge = OtlpBridge::with_provider(provider, Arc::clone(&tracker));

        bridge.update_from_text(SDFORGE_TEXT);
        bridge.force_flush().expect("flush should succeed");
        let finished = exporter.get_finished_metrics().expect("in-memory read");
        assert_eq!(finished.len(), 1);

        assert_eq!(
            norm(
                sum_points(&finished, "fluxgate.sdforge_http_requests_total")
                    .expect("counter bridged")
            ),
            norm(vec![(
                vec![
                    ("route".to_string(), "/x".to_string()),
                    ("method".to_string(), "GET".to_string()),
                    ("status".to_string(), "200".to_string()),
                ],
                3.0,
            )])
        );
        assert_eq!(
            norm(
                gauge_points(
                    &finished,
                    "fluxgate.sdforge_http_request_duration_seconds_bucket"
                )
                .expect("buckets bridged")
            ),
            norm(vec![
                (
                    vec![
                        ("route".to_string(), "/x".to_string()),
                        ("method".to_string(), "GET".to_string()),
                        ("le".to_string(), "0.001".to_string()),
                    ],
                    1.0,
                ),
                (
                    vec![
                        ("route".to_string(), "/x".to_string()),
                        ("method".to_string(), "GET".to_string()),
                        ("le".to_string(), "+Inf".to_string()),
                    ],
                    2.0,
                ),
            ])
        );
        assert_eq!(
            norm(
                gauge_points(
                    &finished,
                    "fluxgate.sdforge_http_request_duration_seconds_sum"
                )
                .expect("sum bridged")
            ),
            norm(vec![(
                vec![
                    ("route".to_string(), "/x".to_string()),
                    ("method".to_string(), "GET".to_string()),
                ],
                0.032,
            )])
        );
        assert_eq!(
            norm(
                gauge_points(
                    &finished,
                    "fluxgate.sdforge_http_request_duration_seconds_count"
                )
                .expect("count bridged")
            ),
            norm(vec![(
                vec![
                    ("route".to_string(), "/x".to_string()),
                    ("method".to_string(), "GET".to_string()),
                ],
                2.0,
            )])
        );
        assert_eq!(
            norm(sum_points(&finished, "fluxgate.cost_usd_total").expect("cost counter")),
            norm(vec![
                (vec![("vendor".to_string(), "openai".to_string())], 1.25),
                (vec![("model".to_string(), "gpt-x".to_string())], 1.25),
                (vec![], 1.25),
            ])
        );
        assert_eq!(
            norm(gauge_points(&finished, "fluxgate.cost_usd").expect("cost gauge")),
            norm(vec![
                (vec![("vendor".to_string(), "openai".to_string())], 1.25),
                (vec![("model".to_string(), "gpt-x".to_string())], 1.25),
                (vec![], 1.25),
            ])
        );

        bridge.shutdown().expect("shutdown");
    }

    /// 导出失败计数可观测且 fail-open：端点不可达时 flush/shutdown 报错只
    /// 累计计数，不 panic；计数随下一轮采集以 counter 形式出现在导出面。
    #[test]
    fn export_failures_are_counted_and_fail_open() {
        let section = crate::config::OtlpSection {
            enabled: true,
            endpoint: "http://127.0.0.1:9/v1/metrics".to_string(),
            interval_secs: 86_400,
            timeout_secs: 1,
        };
        let tracker = Arc::new(CostTracker::new());
        let bridge = match bridge_from_section(&section, tracker) {
            BridgeSpawn::Ready(bridge) => bridge,
            other => panic!("enabled 必须装配，got {other:?}"),
        };

        assert_eq!(bridge.export_fail_total(), 0, "初始为 0");
        // 端点不可达：flush 必失败，但不得 panic
        let _ = bridge.force_flush();
        assert!(
            bridge.export_fail_total() >= 1,
            "导出失败必须计数，实际 {}",
            bridge.export_fail_total()
        );

        // 计数并入导出序列（下一轮采集可见）
        bridge.update_from_text(SDFORGE_TEXT);
        let points = {
            let store = bridge.lock_store();
            store.get("fluxgate.otlp_export_fail_total").cloned()
        };
        let points = points.expect("失败计数必须进导出面");
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].attrs, Vec::new());
        assert!(points[0].value >= 1.0);

        let _ = bridge.shutdown();
    }
}
