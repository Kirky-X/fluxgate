// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 出站 SSRF 防护核（netguard）：特殊网段判定 + 守护抓取。
//!
//! 信任边界：本模块只守「用户可控 URL」的抓取路径（/v1/fetch）；vendor
//! api_base 是管理员配置的可信面，不挂本判定（design Alternatives）。
//! 判定在 DNS 解析后、TCP 建连前完成，并把判过的 IP pin 进连接
//!（防 DNS rebinding 二跳）；重定向禁自动跟随。
//!
//! 段表取 magpie netguard 同源集合（RFC 6890 特殊用途段），纯 std 零依赖。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 守护抓取半边的独立导入（netguard 单开组合不编译本节，避免无用导入告警）
#[cfg(feature = "web-search")]
use std::collections::HashMap;
#[cfg(feature = "web-search")]
use std::sync::Arc;
#[cfg(feature = "web-search")]
use std::time::Duration;

/// 单地址判定结论。`Deny` 只携带静态段名 tag——任何渲染路径都不含原地址
/// 字符串（脱敏契约，R-net-002）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 公网地址，放行
    Allow,
    /// 特殊网段，拒绝（tag ∈ 段表静态串）
    Deny(&'static str),
}

impl Verdict {
    /// Deny 的段名 tag；Allow 返回 None。
    pub fn deny_tag(&self) -> Option<&'static str> {
        match self {
            Verdict::Allow => None,
            Verdict::Deny(tag) => Some(tag),
        }
    }
}

/// IPv4 特殊网段表：`(网络地址, 前缀, 段名 tag)`——magpie netguard 同源集合
///（RFC 6890 特殊用途段）。
const V4_RANGES: &[(Ipv4Addr, u8, &str)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8, "this_network"),
    (Ipv4Addr::new(10, 0, 0, 0), 8, "private"),
    (Ipv4Addr::new(100, 64, 0, 0), 10, "cgnat"),
    (Ipv4Addr::new(127, 0, 0, 0), 8, "loopback"),
    (Ipv4Addr::new(169, 254, 0, 0), 16, "link_local"),
    (Ipv4Addr::new(172, 16, 0, 0), 12, "private"),
    (Ipv4Addr::new(192, 0, 0, 0), 24, "ietf_protocol"),
    (Ipv4Addr::new(192, 0, 2, 0), 24, "documentation"),
    (Ipv4Addr::new(192, 88, 99, 0), 24, "6to4_relay"),
    (Ipv4Addr::new(192, 168, 0, 0), 16, "private"),
    (Ipv4Addr::new(198, 18, 0, 0), 15, "benchmarking"),
    (Ipv4Addr::new(198, 51, 100, 0), 24, "documentation"),
    (Ipv4Addr::new(203, 0, 113, 0), 24, "documentation"),
    (Ipv4Addr::new(224, 0, 0, 0), 4, "multicast"),
    (Ipv4Addr::new(240, 0, 0, 0), 4, "reserved"),
];

/// IPv6 特殊网段表（IPv4 组播维度由 ff00::/8 承接——R-net-001 组播拒绝的
/// v6 半边；设计清单外的唯一补集，规格组播要求同源）。`::/128`（未指定）、
/// `64:ff9b::/96`（NAT64 合成——有 NAT64 网关的环境可借此触达内网 v4）、
/// `100::/64`（discard-only）为同源清单的 v6 缺口补齐。
const V6_RANGES: &[(Ipv6Addr, u8, &str)] = &[
    (Ipv6Addr::LOCALHOST, 128, "loopback"),
    (Ipv6Addr::UNSPECIFIED, 128, "unspecified"),
    (
        Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0),
        7,
        "unique_local",
    ),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10, "link_local"),
    (
        Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0),
        32,
        "documentation",
    ),
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96, "v4_mapped"),
    (Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0, 0), 96, "nat64"),
    (
        Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 0),
        64,
        "discard_only",
    ),
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8, "multicast"),
];

/// 前缀掩码（host 位清零）；`prefix` 已由段表常量保证 ≤ 位宽。
fn v4_mask(prefix: u8) -> u32 {
    debug_assert!(prefix <= 32);
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn v6_mask(prefix: u8) -> u128 {
    debug_assert!(prefix <= 128);
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

/// 判定 `ip` 是否落在特殊用途网段（loopback/私网/链路本地/CGNAT/组播/
/// 保留/文档段）：命中返回 [`Verdict::Deny`]（含段名 tag），公网放行。
/// IPv4-mapped IPv6 整段拒绝（内嵌 v4 可能是私网，保守 deny）。
pub fn guard(ip: IpAddr) -> Verdict {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            for (net, prefix, tag) in V4_RANGES {
                if bits & v4_mask(*prefix) == u32::from(*net) & v4_mask(*prefix) {
                    return Verdict::Deny(tag);
                }
            }
            Verdict::Allow
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            for (net, prefix, tag) in V6_RANGES {
                if bits & v6_mask(*prefix) == u128::from(*net) & v6_mask(*prefix) {
                    return Verdict::Deny(tag);
                }
            }
            Verdict::Allow
        }
    }
}

// ---- 守护抓取（web-search 半边：fetch 依赖 reqwest，随 web-search 编译）----

/// 守护抓取结果：状态码 + Content-Type + 正文文本；3xx（禁自动跟随）时
/// `location` 携带上游 Location、`text` 为空。
#[cfg(feature = "web-search")]
#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub status: u16,
    pub content_type: String,
    pub text: String,
    pub location: Option<String>,
}

/// 守护抓取错误——映射面在 api 层（Blocked → 400 netguard-blocked 等）。
/// `Blocked` 只携带段名 tag（R-net-002 脱敏）；`Transport` 的 message 可能
/// 含目标 URL（公网判定通过后才产生，无内网地址外泄面）。
#[cfg(feature = "web-search")]
#[derive(Debug)]
pub enum FetchError {
    /// 全部解析地址均落特殊网段（tag 取首个 Deny 的段名）
    Blocked { tag: &'static str },
    /// URL 非法（非 http/https 或无 host）
    InvalidUrl,
    /// 上游超时（含连接与读全程）
    Timeout,
    /// 正文超出单次抓取上限
    TooLarge,
    /// Content-Type 不在白名单
    UnsupportedMedia,
    /// DNS/连接/其余传输层失败
    Transport { message: String },
}

/// DNS 解析缝：生产实现走 `tokio::net::lookup_host`；测试注入固定地址表
///（mock DNS 场景，R-net-003 的判定时点可观测性由此保证）。
#[cfg(feature = "web-search")]
#[async_trait::async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>>;
}

/// 生产解析器：系统 DNS 全地址枚举（端口占位 0，连接端口由 URL 决定）。
#[cfg(feature = "web-search")]
pub struct TokioResolver;

#[cfg(feature = "web-search")]
#[async_trait::async_trait]
impl Resolver for TokioResolver {
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((host, 0)).await?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// 地址判定缝：默认即 [`guard`]；测试注入恒 Allow 以便经 mockito 观测
/// pin/Host 语义（判定本体由 T001/T002 单测钉住）。
#[cfg(feature = "web-search")]
type Judge = Arc<dyn Fn(IpAddr) -> Verdict + Send + Sync>;

/// 单次抓取全程超时（连接 + 读正文）
#[cfg(feature = "web-search")]
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次抓取正文上限（读满即判超限）
#[cfg(feature = "web-search")]
const FETCH_MAX_BYTES: usize = 1024 * 1024;

/// Content-Type 白名单（mime 主类型精确匹配，参数忽略）
#[cfg(feature = "web-search")]
const FETCH_CONTENT_TYPES: &[&str] = &[
    "text/html",
    "text/plain",
    "text/markdown",
    "application/json",
];

/// 守护抓取器：DNS 全地址判定 → 首个 Allow 的 IP pin 建连（Host 保持域名，
/// 防 DNS rebinding 二跳）；重定向禁自动跟随。`clients` 缓存见 `pinned_client`。
#[cfg(feature = "web-search")]
pub struct GuardedFetcher {
    resolver: Arc<dyn Resolver>,
    judge: Judge,
    timeout: Duration,
    max_bytes: usize,
    clients: std::sync::Mutex<HashMap<String, Arc<reqwest::Client>>>,
    /// 插入序（front = 最旧）：容量到限逐出旧一半，保留近期 host 的连接池
    ///（整体清空会让 >1024 独立 host 时连接池反复重建抖动）。
    client_order: std::sync::Mutex<std::collections::VecDeque<String>>,
}

#[cfg(feature = "web-search")]
impl Default for GuardedFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "web-search")]
impl GuardedFetcher {
    /// 生产构造：tokio 解析 + [`guard`] 判定 + 10s/1MB 约束。
    pub fn new() -> Self {
        Self::with_parts(
            Arc::new(TokioResolver),
            Arc::new(guard),
            FETCH_TIMEOUT,
            FETCH_MAX_BYTES,
        )
    }

    /// 注入构造（测试/嵌入自定义判定面）：resolver 与 judge 语义见各自类型。
    pub fn with_parts(
        resolver: Arc<dyn Resolver>,
        judge: Judge,
        timeout: Duration,
        max_bytes: usize,
    ) -> Self {
        Self {
            resolver,
            judge,
            timeout,
            max_bytes,
            clients: std::sync::Mutex::new(HashMap::new()),
            client_order: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// 守护抓取：解析 → 全地址判定 → pin 建连 → 3xx 原样带出 / 白名单媒体
    /// 读正文（超限 [`FetchError::TooLarge`]）。
    pub async fn fetch(&self, url: &str) -> Result<FetchedPage, FetchError> {
        let url = reqwest::Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(FetchError::InvalidUrl);
        }
        let host = url.host_str().ok_or(FetchError::InvalidUrl)?.to_string();

        // 判定在解析后、建连前：全地址过 guard，取首个 Allow pin（防
        // rebinding 二跳）；全拒则带首个 Deny 的段名 tag 返回
        let addrs = self
            .resolver
            .resolve(&host)
            .await
            .map_err(|e| FetchError::Transport {
                message: e.to_string(),
            })?;
        let mut first_deny: Option<&'static str> = None;
        let mut pin_ip = None;
        for addr in addrs {
            match (self.judge)(addr) {
                Verdict::Allow => {
                    pin_ip = Some(addr);
                    break;
                }
                Verdict::Deny(tag) => {
                    if first_deny.is_none() {
                        first_deny = Some(tag);
                    }
                }
            }
        }
        let ip = pin_ip.ok_or(FetchError::Blocked {
            tag: first_deny.unwrap_or("unresolved"),
        })?;

        let client = self.pinned_client(&host, ip)?;
        let resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(map_transport_err)?;

        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        // 3xx：禁自动跟随（Policy::none 兜底 + 显式短路），Location 原样带出
        if (300..400).contains(&status) {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            return Ok(FetchedPage {
                status,
                content_type,
                text: String::new(),
                location,
            });
        }

        // 白名单在读正文前判定（mime 主类型精确匹配，参数忽略）
        let mime = content_type.split(';').next().unwrap_or("").trim();
        if !FETCH_CONTENT_TYPES.contains(&mime) {
            return Err(FetchError::UnsupportedMedia);
        }

        let text = read_capped(resp, self.max_bytes).await?;
        let text = if mime == "text/html" {
            strip_html(&text)
        } else {
            text
        };
        Ok(FetchedPage {
            status,
            content_type,
            text,
            location: None,
        })
    }

    /// 取（或惰性建）pin 到 `(host, ip)` 的一次性 Client：resolve 覆盖 DNS
    ///（Host 保持域名、URL 显式端口优先，端口 0 = scheme 常规端口——reqwest
    /// 0.13 实测语义；per-request resolve 该版本不存在），重定向禁自动跟随。
    /// 键含 ip：解析结果变化即新条目，pin 快照永不复用（rebinding 防线）。
    /// 容量守卫：host 基数由用户输入决定，达上限整体清空（有界内存；
    /// reqwest 0.13 无 per-request resolve，缓存是连接复用的唯一载体）。
    fn pinned_client(&self, host: &str, ip: IpAddr) -> Result<Arc<reqwest::Client>, FetchError> {
        const MAX_PINNED_CLIENTS: usize = 1024;
        let key = format!("{host}\u{0}{ip}");
        {
            let guard = self
                .clients
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(client) = guard.get(&key) {
                return Ok(client.clone());
            }
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .resolve(host, std::net::SocketAddr::new(ip, 0))
            .build()
            .map_err(|e| FetchError::Transport {
                message: e.to_string(),
            })?;
        let client = Arc::new(client);
        {
            let mut guard = self
                .clients
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !guard.contains_key(&key) {
                if guard.len() >= MAX_PINNED_CLIENTS {
                    // 容量到限逐出**旧一半**（插入序）：保留近期 host 的连接
                    // 池，避免整体清空后的重建抖动
                    let mut order = self
                        .client_order
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let evict = order.len().div_ceil(2);
                    for stale in order.drain(0..evict) {
                        guard.remove(&stale);
                    }
                }
                guard.insert(key.clone(), client.clone());
                self.client_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push_back(key);
            }
        }
        Ok(client)
    }
}

/// 传输层错误归一：超时单列（504 语义），其余归 Transport。
#[cfg(feature = "web-search")]
fn map_transport_err(e: reqwest::Error) -> FetchError {
    if e.is_timeout() {
        return FetchError::Timeout;
    }
    let mut message = e.to_string();
    message.truncate(300);
    FetchError::Transport { message }
}

/// 流式读正文并钉上限：读满 `max` 字节仍有剩余 → [`FetchError::TooLarge`]
///（全量缓冲前截停，不被超大响应拖垮内存）。
#[cfg(feature = "web-search")]
async fn read_capped(resp: reqwest::Response, max: usize) -> Result<String, FetchError> {
    use futures::Stream as _;
    let mut stream = std::pin::pin!(resp.bytes_stream());
    let mut body: Vec<u8> = Vec::with_capacity(max.min(64 * 1024));
    loop {
        match std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
            Some(Ok(chunk)) => {
                if body.len() + chunk.len() > max {
                    return Err(FetchError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Some(Err(e)) => return Err(map_transport_err(e)),
            None => break,
        }
    }
    String::from_utf8(body).map_err(|_| FetchError::Transport {
        message: crate::i18n::t("util-fetch-body-not-utf8"),
    })
}

/// 正文提取首版：HTML 标签剥离（`<…>` 跨度删除）+ 三常用实体解码
///（&amp; &lt; &gt;）——单遍扫描零中间串（顺带修正旧链式 replace 的二次
/// 解码：`&amp;lt;` 现解码一次为 `&lt;` 而非 `<`）；结构化提取
///（readability 级）为 Deferred 性质，复杂页面正文质量一般（design
/// Consequences 披露项）。
#[cfg(feature = "web-search")]
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        match rest.find('<') {
            Some(pos) => {
                push_decoded_text(&mut out, &rest[..pos]);
                match rest[pos..].find('>') {
                    Some(end) => rest = &rest[pos + end + 1..],
                    None => rest = "",
                }
            }
            None => {
                push_decoded_text(&mut out, rest);
                return out;
            }
        }
    }
}

/// 文本段落输出 + 实体就地解码（单遍；未知实体原样保留 `&`）。
#[cfg(feature = "web-search")]
fn push_decoded_text(out: &mut String, mut text: &str) {
    while let Some(amp) = text.find('&') {
        out.push_str(&text[..amp]);
        let tail = &text[amp..];
        if let Some(rest) = tail.strip_prefix("&amp;") {
            out.push('&');
            text = rest;
        } else if let Some(rest) = tail.strip_prefix("&lt;") {
            out.push('<');
            text = rest;
        } else if let Some(rest) = tail.strip_prefix("&gt;") {
            out.push('>');
            text = rest;
        } else {
            out.push('&');
            text = &tail[1..];
        }
    }
    out.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().expect("valid ipv4"))
    }

    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse().expect("valid ipv6"))
    }

    /// R-net-002：Deny 携段名 tag（10.x → private、127.x → loopback），
    /// 且任何渲染形态都不回显原地址字符串。
    #[test]
    fn deny_returns_range_tag_without_address_echo() {
        for (ip, want_tag) in [
            (v4("10.1.2.3"), "private"),
            (v4("172.20.0.9"), "private"),
            (v4("192.168.10.20"), "private"),
            (v4("127.0.0.1"), "loopback"),
            (v4("100.64.7.7"), "cgnat"),
            (v6("::1"), "loopback"),
        ] {
            let verdict = guard(ip);
            assert_eq!(
                verdict.deny_tag(),
                Some(want_tag),
                "{ip} 的拒绝 tag 必须是 {want_tag}"
            );
            // 脱敏契约：Deny 只携带 'static 段名——Debug 渲染（错误体仅有的
            // 取材面）不得含原地址串
            let rendered = format!("{verdict:?}");
            assert!(
                !rendered.contains(&ip.to_string()),
                "拒绝渲染不得回显原地址: {rendered}"
            );
        }
        assert_eq!(guard(v4("8.8.8.8")).deny_tag(), None, "Allow 无 tag");
    }

    /// R-net-001：段表内代表地址拒、公网 v4/v6 放行。
    #[test]
    fn special_ranges_denied_public_allowed() {
        let denied = [
            v4("127.0.0.1"),
            v4("10.0.0.1"),
            v4("169.254.1.1"),
            v4("192.168.1.1"),
            v4("100.64.1.1"),
            v6("::1"),
            v6("fc00::1"),
            // 逐段代表地址（R-net-001 验收：清单内每段至少一条）
            v4("0.0.0.1"),
            v4("172.16.0.1"),
            v4("172.31.255.254"),
            v4("192.0.0.1"),
            v4("192.0.2.1"),
            v4("192.88.99.1"),
            v4("198.18.0.1"),
            v4("198.19.255.254"),
            v4("198.51.100.1"),
            v4("203.0.113.1"),
            v4("224.0.0.1"),
            v4("240.0.0.1"),
            v4("255.255.255.255"),
            v6("fe80::1"),
            v6("febf::1"),
            v6("fdff::1"),
            v6("2001:db8::1"),
            v6("::ffff:10.0.0.1"),
            v6("ff02::1"),
            // v6 缺口补齐三段（未指定/NAT64 合成/discard-only）
            v6("::"),
            v6("64:ff9b::1.2.3.4"),
            v6("100::1"),
        ];
        for ip in denied {
            assert!(
                matches!(guard(ip), Verdict::Deny(_)),
                "{ip} 必须判为特殊网段拒绝"
            );
        }

        // 边界外侧放行（掩码 off-by-one 探针：紧邻段边界上一档的公网址）
        let allowed = [
            v4("8.8.8.8"),
            v4("1.1.1.1"),
            v6("2606:4700::"),
            v4("9.0.0.1"),
            v4("100.128.0.1"),
            v4("101.0.0.1"),
            v4("172.32.0.1"),
            v4("192.88.100.1"),
            v4("198.20.0.1"),
            v4("198.52.0.1"),
            v4("203.0.114.1"),
            v4("220.0.0.1"),
            v6("2001:4860::1"),
            v6("fe00::1"),
            v6("fec0::1"),
        ];
        for ip in allowed {
            assert_eq!(guard(ip), Verdict::Allow, "{ip} 是公网地址必须放行");
        }
    }
}

#[cfg(all(test, feature = "web-search"))]
mod fetch_tests {
    use super::*;

    /// 固定地址表解析器（mock DNS：判定时点与 pin 行为的可观测缝）
    struct FixedResolver(Vec<IpAddr>);

    #[async_trait::async_trait]
    impl Resolver for FixedResolver {
        async fn resolve(&self, _host: &str) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    /// R-net-003：私网解析全拒（tag 取首个 Deny）；公网放行且 Host 头保持
    /// 域名；301 不自动跟随、Location 原样带出。
    #[tokio::test]
    async fn private_host_resolving_blocked_public_allowed() {
        // 私网解析全拒（真 guard）：不发起连接，直接 Blocked
        let fetcher = GuardedFetcher::with_parts(
            Arc::new(FixedResolver(vec![
                IpAddr::from([10, 0, 0, 1]),
                IpAddr::from([192, 168, 0, 9]),
            ])),
            Arc::new(guard),
            Duration::from_secs(1),
            4096,
        );
        let err = fetcher
            .fetch("http://internal.example.test/path")
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::Blocked { tag: "private" }),
            "私网解析必须拒绝: {err:?}"
        );

        // 公网放行：判定缝恒 Allow、pin 到 mockito 环回；Host 头断言 =
        // 域名保持（resolve pin 不得把 Host 替换成 IP）
        let mut server = mockito::Server::new_async().await;
        let port: u16 = server.url().rsplit(':').next().unwrap().parse().unwrap();
        let host_mock = server
            .mock("GET", "/page")
            .match_header(
                "Host",
                mockito::Matcher::Exact(format!("public.example.test:{port}")),
            )
            .with_header("Content-Type", "text/plain")
            .with_body("ok")
            .create_async()
            .await;
        let fetcher = GuardedFetcher::with_parts(
            Arc::new(FixedResolver(vec![IpAddr::from([127, 0, 0, 1])])),
            Arc::new(|_| Verdict::Allow),
            Duration::from_secs(5),
            4096,
        );
        let url = format!("{}/page", server.url()).replace("127.0.0.1", "public.example.test");
        let page = fetcher.fetch(&url).await.expect("公网放行必须抓取成功");
        assert_eq!(page.status, 200);
        assert_eq!(page.text, "ok");
        host_mock.assert();

        // 301 不跟随：Location 原样带出（跟随则会拿到落点页 200）
        let redir_mock = server
            .mock("GET", "/redir")
            .with_status(301)
            .with_header("Location", "http://public.example.test/landing")
            .create_async()
            .await;
        let url = format!("{}/redir", server.url()).replace("127.0.0.1", "public.example.test");
        let page = fetcher.fetch(&url).await.expect("301 响应必须原样返回");
        assert_eq!(page.status, 301);
        assert_eq!(
            page.location.as_deref(),
            Some("http://public.example.test/landing")
        );
        redir_mock.assert();
    }

    /// T004：同 host 同解析结果的两次抓取复用同一只 pin Client
    ///（reqwest 连接池随之复用）；解析结果变化（host 同、ip 异）生成新条目，
    /// 防 pin 快照过期。缓存键含 ip，白盒断言条目数。
    #[tokio::test]
    async fn host_client_cache_reuses_connection() {
        struct SwappableResolver(std::sync::Mutex<Vec<IpAddr>>);
        #[async_trait::async_trait]
        impl Resolver for SwappableResolver {
            async fn resolve(&self, _host: &str) -> std::io::Result<Vec<IpAddr>> {
                Ok(self.0.lock().unwrap().clone())
            }
        }

        let resolver = Arc::new(SwappableResolver(std::sync::Mutex::new(vec![
            IpAddr::from([127, 0, 0, 1]),
        ])));
        let fetcher = GuardedFetcher::with_parts(
            resolver.clone(),
            Arc::new(|_| Verdict::Allow),
            Duration::from_secs(5),
            4096,
        );

        let mut server = mockito::Server::new_async().await;
        let page_mock = server
            .mock("GET", "/cache")
            .with_header("Content-Type", "text/plain")
            .with_body("hit")
            .expect(2)
            .create_async()
            .await;
        let url = format!("{}/cache", server.url()).replace("127.0.0.1", "cached.example.test");

        let first = fetcher.fetch(&url).await.expect("首次抓取成功");
        assert_eq!(first.text, "hit");
        {
            let clients = fetcher.clients.lock().unwrap();
            assert_eq!(clients.len(), 1, "同 host 同解析结果只建一只 Client");
        }

        let second = fetcher.fetch(&url).await.expect("二次抓取复用 Client");
        assert_eq!(second.text, "hit");
        {
            let clients = fetcher.clients.lock().unwrap();
            assert_eq!(clients.len(), 1, "命中路径不得新建条目");
        }
        page_mock.assert();

        // 解析结果变化：同 host 新 ip → 新 pin 条目（旧快照永不复用）
        *resolver.0.lock().unwrap() = vec![IpAddr::from([127, 0, 0, 2])];
        let _ = fetcher.fetch(&url).await;
        let clients = fetcher.clients.lock().unwrap();
        assert_eq!(clients.len(), 2, "解析结果变化必须生成新 pin 条目");
    }
}
