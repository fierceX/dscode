//! 嵌入前端产物服务（build.rs 生成的 FILES 表 + SPA fallback）。

pub mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}
pub use assets::FILES;

use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") || path.ends_with(".mjs") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".map") {
        "application/json; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

fn lookup(path: &str) -> Option<&'static str> {
    assets::FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, content)| *content)
}

/// SPA fallback 服务：命中静态资源返回内容，否则返回 index.html（前端路由）。
pub async fn embedded_serve(uri: axum::http::Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    let candidate = if path.is_empty() { "index.html" } else { path };
    if let Some(content) = lookup(candidate) {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(candidate))
            .header(
                header::CACHE_CONTROL,
                if candidate == "index.html" {
                    "no-cache"
                } else {
                    "public, max-age=31536000, immutable"
                },
            )
            .body(Body::from(content.to_string()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    // SPA fallback：未知路径返回 index.html（前端路由接管）
    if let Some(index) = lookup("index.html") {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(index.to_string()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    StatusCode::NOT_FOUND.into_response()
}
