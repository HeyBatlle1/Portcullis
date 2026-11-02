//! # The audit desk
//!
//! An AG Studio dashboard over the audit chain, at `/audit`. It is a second
//! read-only window, like `/console`: it reads the chain and the gate's state
//! and cannot propose, approve or change anything.
//!
//! Its AI agents run in the browser through AG Studio's Agent Framework. They
//! reach a model through [`proxy`], which holds the key, allows only the
//! configured models, caps output, and counts calls against a daily budget.
//! The agents' tools are Studio's own (read the data, build widgets) plus a
//! chain check; none of them can reach `/mcp`, so the agents that explain the
//! record have no path to the money the record describes.

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// OpenRouter's Responses API, which AG Studio's reference adapter speaks.
pub const RESPONSES: &str = "https://openrouter.ai/api/v1/responses";

/// Ceiling on one model turn, whatever the page asks for.
pub const MAX_OUTPUT_TOKENS: u64 = 4_000;

pub struct Studio {
    /// Built app: `index.html` plus `assets/`.
    pub dir: std::path::PathBuf,
    /// AG Studio licence key, if any. Public by design: it ships to the browser.
    pub license_key: Option<String>,
    /// Model key and the models the page may name. Empty means no AI.
    pub openrouter_key: Option<String>,
    pub models: Vec<String>,
    /// Model calls allowed per UTC day, across all viewers.
    pub daily_calls: u32,
    pub http: reqwest::Client,
    pub budget: Mutex<(u64, u32)>,
}

impl Studio {
    fn ai_enabled(&self) -> bool {
        self.openrouter_key.is_some() && !self.models.is_empty()
    }

    /// Take one call from today's budget, or say when it runs out.
    fn spend(&self, now: u64) -> Result<(), String> {
        let mut b = self.budget.lock().unwrap_or_else(|p| p.into_inner());
        let day = now / 86_400;
        if b.0 != day {
            *b = (day, 0);
        }
        if b.1 >= self.daily_calls {
            return Err("the audit desk's model budget for today is used up".into());
        }
        b.1 += 1;
        Ok(())
    }
}

pub fn routes(studio: Arc<Studio>) -> Router {
    Router::new()
        .route("/audit", get(index))
        .route("/audit/", get(index))
        .route("/audit/assets/{*file}", get(asset))
        .route("/audit/config", get(config))
        .route("/audit/ai/responses", post(proxy))
        // The app is one large script; gzip takes it from about 5.7 MB to 1.6 MB.
        // Event streams are left alone (tower-http does not compress them).
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(studio)
}

async fn index(State(s): State<Arc<Studio>>) -> Response {
    match tokio::fs::read(s.dir.join("index.html")).await {
        Ok(b) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], b).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            "the audit desk has not been built (npm run build in studio/)",
        )
            .into_response(),
    }
}

async fn asset(
    State(s): State<Arc<Studio>>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> Response {
    // Built assets have flat, hashed names. Anything with a path in it is not one.
    if file.is_empty()
        || !file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || file.starts_with('.')
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let kind = match file.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    match tokio::fs::read(s.dir.join("assets").join(&file)).await {
        Ok(b) => (
            [
                (header::CONTENT_TYPE, kind),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            b,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn config(State(s): State<Arc<Studio>>) -> Json<Value> {
    Json(json!({
        "licenseKey": s.license_key,
        "ai": s.ai_enabled(),
        "models": if s.ai_enabled() { s.models.clone() } else { vec![] },
    }))
}

fn error(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({ "error": { "message": message } }))).into_response()
}

/// Forward one Responses API call to the model, with the key added and the
/// request bounded. The reply streams straight back.
async fn proxy(State(s): State<Arc<Studio>>, Json(mut body): Json<Value>) -> Response {
    let Some(key) = s.openrouter_key.as_deref().filter(|_| s.ai_enabled()) else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the audit desk's AI is not configured",
        );
    };
    let model = body["model"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| s.models[0].clone());
    if !s.models.contains(&model) {
        return error(StatusCode::BAD_REQUEST, "that model is not offered here");
    }
    let now = portcullis_executor::paypal::unix_now();
    if let Err(e) = s.spend(now) {
        return error(StatusCode::TOO_MANY_REQUESTS, &e);
    }
    body["model"] = json!(model);
    let asked = body["max_output_tokens"]
        .as_u64()
        .unwrap_or(MAX_OUTPUT_TOKENS);
    body["max_output_tokens"] = json!(asked.min(MAX_OUTPUT_TOKENS));

    let upstream = s
        .http
        .post(RESPONSES)
        .bearer_auth(key)
        .header("X-Title", "Portcullis audit desk")
        .json(&body)
        .send()
        .await;
    match upstream {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let kind = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, kind)
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::from_stream(r.bytes_stream()))
                .unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, "bad upstream response"))
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "the model could not be reached"),
    }
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap_or_default()
}
