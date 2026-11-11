//! # The audit desk
//!
//! An AG Studio dashboard over the audit chain, at `/audit`. It is a second
//! read-only window, like `/console`: it reads the chain and the gate's state
//! and cannot propose, approve or change anything.
//!
//! Its AI agents run in the browser through AG Studio's Agent Framework. They
//! reach a model through [`proxy`], which holds the key, allows only the
//! configured models and their own function tools, caps the request and the
//! output, and counts calls per visitor and against a site-wide daily budget.
//! The agents' tools are Studio's own (read the data, build widgets) plus a
//! chain check; none of them can reach `/mcp`, so the agents that explain the
//! record have no path to the money the record describes.

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
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

/// Ceiling on one request to the model, which bounds the input tokens a call
/// can spend. The desk's real turns (instructions, tool schemas, a page of
/// chain records) are well under it; the default axum limit was 2 MB.
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;

/// Reasoning effort the desk may ask for; anything higher is lowered to the
/// last of these. AG Studio asks for "medium".
const EFFORTS: &[&str] = &["minimal", "low", "medium"];

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
    /// Model calls allowed per visitor per UTC day; see [`crate::visitor`].
    pub per_visitor: crate::visitor::Quota,
    /// Where model calls go: [`RESPONSES`], or a fake in tests.
    pub upstream: String,
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
        .route(
            "/audit/ai/responses",
            post(proxy).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES)),
        )
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

/// Build the request the model sees from only what the desk's agents send:
/// conversation text, their own function tools, and modest reasoning. An
/// allowlist, not a denylist: a field this does not name never leaves, so a
/// new provider feature cannot be switched on from the page. What is dropped:
/// images, files and audio in the input (each one bills, and a URL makes the
/// provider fetch it), server-side tools of any kind, provider routing,
/// service tiers, tracing, storage, and anything else not listed here.
pub fn sanitize(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(i) = body.get("instructions").and_then(Value::as_str) {
        out.insert("instructions".into(), json!(i));
    }
    out.insert("input".into(), input(body.get("input")));
    let tools: Vec<Value> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter(|t| t["type"] == "function" && t["name"].is_string())
                .map(|t| {
                    let mut f = json!({ "type": "function", "name": t["name"] });
                    for k in ["description", "parameters", "strict"] {
                        if let Some(v) = t.get(k) {
                            f[k] = v.clone();
                        }
                    }
                    f
                })
                .collect()
        })
        .unwrap_or_default();
    if !tools.is_empty() {
        out.insert("tools".into(), json!(tools));
        let choice = match body.get("tool_choice") {
            Some(Value::String(c)) if ["auto", "none", "required"].contains(&c.as_str()) => {
                json!(c)
            }
            Some(c) if c["type"] == "function" && c["name"].is_string() => {
                json!({ "type": "function", "name": c["name"] })
            }
            _ => json!("auto"),
        };
        out.insert("tool_choice".into(), choice);
        if let Some(p) = body.get("parallel_tool_calls").and_then(Value::as_bool) {
            out.insert("parallel_tool_calls".into(), json!(p));
        }
    }
    if let Some(f) = body.get("text").and_then(|t| t.get("format")) {
        if ["text", "json_object", "json_schema"].contains(&f["type"].as_str().unwrap_or("")) {
            out.insert("text".into(), json!({ "format": f }));
        }
    }
    let effort = body["reasoning"]["effort"].as_str().unwrap_or("medium");
    let effort = if EFFORTS.contains(&effort) {
        effort
    } else {
        "medium"
    };
    out.insert("reasoning".into(), json!({ "effort": effort }));
    if let Some(t) = body.get("temperature").and_then(Value::as_f64) {
        out.insert("temperature".into(), json!(t.clamp(0.0, 2.0)));
    }
    out.insert(
        "stream".into(),
        json!(body.get("stream").and_then(Value::as_bool).unwrap_or(true)),
    );
    out.insert("store".into(), json!(false));
    Value::Object(out)
}

/// The conversation, keeping only text and the desk's own tool traffic.
fn input(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => json!(s),
        Some(Value::Array(items)) => Value::Array(items.iter().filter_map(item).collect()),
        _ => json!(""),
    }
}

fn item(it: &Value) -> Option<Value> {
    let kind = it["type"].as_str().unwrap_or("message");
    match kind {
        "message" => {
            let role = it["role"].as_str()?;
            if !["user", "assistant", "system", "developer"].contains(&role) {
                return None;
            }
            let content = match &it["content"] {
                Value::String(s) => json!(s),
                Value::Array(parts) => Value::Array(
                    parts
                        .iter()
                        .filter(|p| {
                            ["input_text", "output_text", "refusal"]
                                .contains(&p["type"].as_str().unwrap_or(""))
                        })
                        .map(|p| {
                            let mut q = json!({ "type": p["type"] });
                            for k in ["text", "refusal"] {
                                if let Some(t) = p.get(k).filter(|t| t.is_string()) {
                                    q[k] = t.clone();
                                }
                            }
                            q
                        })
                        .collect(),
                ),
                _ => return None,
            };
            let mut m = json!({ "type": "message", "role": role, "content": content });
            for k in ["id", "status"] {
                if let Some(x) = it.get(k).filter(|x| x.is_string()) {
                    m[k] = x.clone();
                }
            }
            Some(m)
        }
        "function_call" => {
            let mut c = json!({ "type": "function_call" });
            for k in ["id", "call_id", "name", "arguments", "status"] {
                if let Some(x) = it.get(k).filter(|x| x.is_string()) {
                    c[k] = x.clone();
                }
            }
            Some(c)
        }
        "function_call_output" => {
            let out = it["output"].as_str()?;
            let mut c = json!({ "type": "function_call_output", "output": out });
            for k in ["id", "call_id", "status"] {
                if let Some(x) = it.get(k).filter(|x| x.is_string()) {
                    c[k] = x.clone();
                }
            }
            Some(c)
        }
        "reasoning" => {
            let summary: Vec<Value> = it["summary"]
                .as_array()
                .map(|s| {
                    s.iter()
                        .filter(|p| p["type"] == "summary_text" && p["text"].is_string())
                        .map(|p| json!({ "type": "summary_text", "text": p["text"] }))
                        .collect()
                })
                .unwrap_or_default();
            let mut r = json!({ "type": "reasoning", "summary": summary });
            if let Some(x) = it.get("id").filter(|x| x.is_string()) {
                r["id"] = x.clone();
            }
            Some(r)
        }
        _ => None,
    }
}

/// Forward one Responses API call to the model, with the key added and the
/// request bounded. The reply streams straight back.
async fn proxy(
    State(s): State<Arc<Studio>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
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
    if !body.is_object() {
        return error(StatusCode::BAD_REQUEST, "expected a JSON object");
    }
    let now = portcullis_executor::paypal::unix_now();
    // Judges skip the public allowances; see `visitor::is_judge`.
    if !crate::visitor::is_judge(&headers) {
        if let Err(e) = s.spend(now) {
            return error(StatusCode::TOO_MANY_REQUESTS, &e);
        }
        if !s
            .per_visitor
            .take(crate::visitor::client(&headers).as_deref(), now)
        {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "you have used today's questions for the audit desk; try again tomorrow",
            );
        }
    }
    let asked = body["max_output_tokens"]
        .as_u64()
        .unwrap_or(MAX_OUTPUT_TOKENS);
    let mut body = sanitize(&body);
    body["model"] = json!(model);
    body["max_output_tokens"] = json!(asked.min(MAX_OUTPUT_TOKENS));

    let upstream = s
        .http
        .post(&s.upstream)
        .bearer_auth(key)
        .header("X-Title", "Portcullis audit desk")
        .json(&body)
        .send()
        .await;
    match upstream {
        Ok(r) if !r.status().is_success() => {
            // The provider's own words can describe our account (credit
            // left, limits); the page gets a plain answer, the log the rest.
            let code = r.status().as_u16();
            let detail = r.text().await.unwrap_or_default();
            eprintln!(
                "audit desk: model provider answered {code}: {}",
                detail.chars().take(300).collect::<String>()
            );
            error(
                if code == 429 {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::BAD_GATEWAY
                },
                "the model provider could not answer this request",
            )
        }
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
