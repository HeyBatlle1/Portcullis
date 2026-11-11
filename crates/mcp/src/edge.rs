//! What every response and every state-changing request passes through.
//!
//! Two jobs. First, refuse cross-site writes: a page on another site must not
//! be able to make a visitor's browser start a run, a shift or a model call on
//! that visitor's allowance. Browsers say where a request came from
//! (`Sec-Fetch-Site`, `Origin`); a request that says another site is refused.
//! Requests from tools (curl, an MCP client) send neither and are judged by
//! everything else as before. The pages also send JSON, which a cross-site
//! form cannot, and the handlers require it.
//!
//! Second, headers the pages should always have: no framing by other sites
//! (clickjacking the attack buttons), no MIME sniffing, no referrer leaking,
//! HTTPS only.

use axum::{
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Whether a request is a write that a browser says came from another site.
pub fn cross_site_write(req: &Request) -> bool {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    // MCP clients are programs, and rmcp checks their Host itself.
    if req.uri().path().starts_with("/mcp") {
        return false;
    }
    let h = req.headers();
    if let Some(site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" {
            return true;
        }
    }
    if let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = h
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let from = origin.split("://").nth(1).unwrap_or("");
        if origin == "null" || from != host {
            return true;
        }
    }
    false
}

pub async fn harden(req: Request, next: Next) -> Response {
    if cross_site_write(&req) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "cross-site requests cannot start anything here; use the page itself"
            })),
        )
            .into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, "x-content-type-options", "nosniff");
    set(h, "x-frame-options", "DENY");
    set(h, "referrer-policy", "no-referrer");
    set(
        h,
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    );
    // Framing only; each page carries its own full policy. Two policies are
    // both enforced, so this cannot loosen a page's.
    set(h, "content-security-policy", "frame-ancestors 'none'");
    res
}
