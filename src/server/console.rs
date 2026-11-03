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

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// 嵌入产物（web/console/dist）。folder 相对 Cargo.toml；
/// dist 缺位（npm 不可达且无预构建产物）时 compile_error 提示，
/// 而不是产出空 /console——静默 404 更难排查。
#[derive(RustEmbed)]
#[folder = "web/console/dist"]
struct ConsoleAssets;

const INDEX_HTML: &str = "index.html";

fn serve(path: &str) -> Response {
    // 精确查表：命中返回文件；未命中（含目录形路径）一律 SPA fallback。
    // fallback 也缺位（dist 为空）时 404 并说明，不返回空 200。
    // 缓存策略按「实际服务的资产键」判定而非请求路径——对已不存在的旧
    // 哈希资产回落 index.html 时，若按请求路径下发 immutable 会把 SPA 壳
    // 永久缓存到那个 URL 上。
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
    let mime = file.metadata.mimetype();
    // Vite 内容哈希资产：新构建即换文件名，index.html 引用随之失效，可
    // 长期缓存；SPA 壳保持协商缓存。纵深防御：nosniff 防类型嗅探，
    // frame-ancestors 挡登录壳被第三方 iframe 嵌套（仅约束谁能嵌本页，
    // 不限页面自身资源加载，无破坏面）。
    let cache = if served.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "frame-ancestors 'self'".to_string(),
            ),
        ],
        file.data,
    )
        .into_response()
}

/// GET /console → SPA 壳。
pub async fn console_index() -> Response {
    serve(INDEX_HTML)
}

/// GET /console/{*path} → 精确资产命中或 SPA fallback。
pub async fn console_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    serve(&path)
}
