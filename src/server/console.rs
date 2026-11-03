// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 内置控制台前端（console feature）：rust-embed 嵌入 `web/console/dist`
//! 的 Vite 产物，`GET /console` 与 `/console/*` 提供静态服务；
//! 未命中路径回落 index.html（SPA 路由）。feature 关闭时本模块整体
//! cfg 掉，不产生任何路由与嵌入面。
//!
//! 安全面：handler 不做任何请求体处理；路径解析仅查表（embed 索引精确
//! 匹配），无 `..` 逃逸面。release 构建为编译期只读快照（零文件系统
//! 访问）；debug 构建下 rust-embed 默认改为运行时直读 dist 目录
//! （canonicalize + 前缀守卫，仍无逃逸面）——「嵌入快照」语义仅 release
//! 成立，debug 验证的是磁盘上的 dist。
//!
//! 缓存与协商：内容哈希（rust-embed sha256）作弱 ETag，命中
//! If-None-Match 回 304；哈希资产 immutable 长缓存、SPA 壳 no-cache；
//! 传输压缩由路由装配处的 CompressionLayer 承担（见 http.rs）。

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// 嵌入产物（web/console/dist）。folder 相对 Cargo.toml；
/// dist 缺位（npm 不可达且无预构建产物）时编译失败提示，
/// 而不是产出空 /console——静默 404 更难排查。
#[derive(RustEmbed)]
#[folder = "web/console/dist"]
struct ConsoleAssets;

const INDEX_HTML: &str = "index.html";

/// 弱 ETag：内容哈希十六进制（查表编码，零分配）。弱语义与传输压缩并存
/// （强 ETag 会在「缓存了 gzip 体、后续请求未协商压缩」的组合下错用 304）。
fn etag_of(file: &rust_embed::EmbeddedFile) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hash = file.metadata.sha256_hash();
    let mut hex = String::with_capacity(64);
    for byte in hash {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!("W/\"{hex}\"")
}

/// If-None-Match 命中判定：`*` 通配或列表内精确等值（弱 ETag 原样回传）。
fn not_modified(req_headers: &HeaderMap, etag: &str) -> bool {
    match req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => v.trim() == "*" || v.split(',').any(|c| c.trim() == etag),
        None => false,
    }
}

/// 哈希资产（Vite 内容哈希文件名）可长期缓存；SPA 壳与其它文件保持
/// 协商缓存。按「实际服务的资产键」判定而非请求路径——对已不存在的
/// 旧哈希资产回落 index.html 时，若按请求路径下发 immutable 会把 SPA
/// 壳永久缓存到那个 URL 上。
fn cache_control_for(served: &str) -> &'static str {
    if served.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn serve(path: &str, req_headers: &HeaderMap) -> Response {
    // 精确查表：命中返回文件；未命中（含目录形路径）一律 SPA fallback。
    // fallback 也缺位（dist 为空）时 404 并说明，不返回空 200。
    let (file, served) = match ConsoleAssets::get(path) {
        Some(f) => (f, path),
        None => match ConsoleAssets::get(INDEX_HTML) {
            Some(f) => (f, INDEX_HTML),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "console assets missing: run `npm run build` in web/console (or rebuild with npm available)",
                )
                    .into_response()
            }
        },
    };
    let etag = etag_of(&file);
    let cache = cache_control_for(served);
    // 304 判定前置：命中时不构建 body 路径的头数组与响应体
    if not_modified(req_headers, &etag) {
        // 304 无 body，缓存验证器与缓存策略头必须照常下发。纵深防御：
        // nosniff 防类型嗅探；frame-ancestors/XFO 挡登录壳被第三方 iframe
        // 嵌套（仅约束谁能嵌本页，不限页面自身资源加载，无破坏面）。
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::CACHE_CONTROL, cache.to_string()),
                (header::ETAG, etag),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "frame-ancestors 'self'".to_string(),
                ),
                (header::X_FRAME_OPTIONS, "DENY".to_string()),
            ],
        )
            .into_response();
    }
    let mime = file.metadata.mimetype();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
            (header::ETAG, etag),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "frame-ancestors 'self'".to_string(),
            ),
            (header::X_FRAME_OPTIONS, "DENY".to_string()),
        ],
        file.data,
    )
        .into_response()
}

/// GET /console → SPA 壳。
pub async fn console_index(req_headers: HeaderMap) -> Response {
    serve(INDEX_HTML, &req_headers)
}

/// GET /console/{*path} → 精确资产命中或 SPA fallback。
pub async fn console_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
    req_headers: HeaderMap,
) -> Response {
    serve(&path, &req_headers)
}
