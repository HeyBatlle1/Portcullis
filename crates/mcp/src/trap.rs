//! The doormat. Scanners ask every public site for `/.env`, `/.git/config`,
//! `/wp-login.php` and the like. Portcullis has none of them, so a request for
//! one is never a visitor who got lost: it is somebody looking for a way in.
//! They get a plain answer instead of a 404, and the page counts them.
//!
//! What is kept: the path asked for (cut short) and the time. Not the IP, not
//! headers, not a body. Bounded: the last [`KEEP`] probes and a daily count.
//! Nothing here touches the gate, the ledger or the audit chain, so a flood of
//! probes cannot grow the record or slow a payment.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Probes kept for the page; older ones are dropped.
pub const KEEP: usize = 20;

#[derive(Default)]
struct Seen {
    day: u64,
    today: u64,
    total: u64,
    recent: VecDeque<(u64, String)>,
}

pub struct Trap {
    seen: Mutex<Seen>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Trap {
    pub fn new(clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::default(),
            clock,
        })
    }

    fn note(&self, path: &str) {
        let now = (self.clock)();
        let mut s = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if s.day != now / 86_400 {
            s.day = now / 86_400;
            s.today = 0;
        }
        s.today += 1;
        s.total += 1;
        if s.recent.len() == KEEP {
            s.recent.pop_back();
        }
        s.recent.push_front((now, shorten(path)));
    }
}

/// A probed path, made safe to keep and show: printable ASCII, at most 60
/// characters. Pages render it as text, never as markup.
fn shorten(path: &str) -> String {
    let clean: String = path
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(60)
        .collect();
    if clean.is_empty() {
        "/".into()
    } else {
        clean
    }
}

/// Paths nobody asks a payments demo for unless they are looking for a way in.
pub fn is_probe(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    const MARKS: &[&str] = &[
        "/.env",
        "/.git",
        "/.svn",
        "/.hg",
        "/.aws",
        "/.ssh",
        "/.ds_store",
        "/.htaccess",
        "/.htpasswd",
        "wp-",
        "xmlrpc",
        "phpmyadmin",
        "/pma",
        "/admin",
        "/administrator",
        "/actuator",
        "/server-status",
        "/cgi-bin",
        "/console/admin",
        "/debug",
        "/config.json",
        "/credentials",
        "/secrets",
        "/backup",
        "/vendor/phpunit",
        "/etc/passwd",
        "../",
        "%2e%2e",
    ];
    const ENDS: &[&str] = &[
        ".php", ".asp", ".aspx", ".jsp", ".sql", ".bak", ".zip", ".tar.gz", ".old",
    ];
    MARKS.iter().any(|m| p.contains(m)) || ENDS.iter().any(|e| p.ends_with(e))
}

const WE_SEE_YOU: &str = "We see you.

You asked for {path}. Portcullis has no admin page and no .env on disk, and
no secret a visitor can reach would move money on its own: the agent never
holds the PayPal credentials, and nothing is paid unless a policy you cannot
see allows the exact payee and amount.

This request was counted: the path and the time. No IP, no headers. It shows
on /siege, next to everyone else who tried today.

The code is public, so you don't need to guess:
https://github.com/HeyBatlle1/Portcullis
If you find something real, open an issue there. We would rather hear it from you.

  -- the gate
";

const FAKE_ENV: &str = "# We see you.
#
# This is not a .env. There is no .env on this server: its secrets live in the
# host's environment, and the agent never holds any of them.
# Your request was counted (path and time; no IP, no headers). See /siege.
#
# The code is public: https://github.com/HeyBatlle1/Portcullis
PAYPAL_ENV=sandbox
PAYPAL_CLIENT_ID=not-here
PAYPAL_CLIENT_SECRET=not-here-either
OPENROUTER_API_KEY=try-the-front-door
";

const ROBOTS: &str = "User-agent: *
Allow: /
# There is no admin page. Everything is in the repo:
# https://github.com/HeyBatlle1/Portcullis
Disallow: /admin
";

fn answer(path: &str) -> Response {
    let body = if path.to_ascii_lowercase().starts_with("/.env") {
        FAKE_ENV.to_string()
    } else {
        WE_SEE_YOU.replace("{path}", &shorten(path))
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// Every route nothing else claims. A probe is answered and counted; anything
/// else is a plain 404.
async fn fallback(State(t): State<Arc<Trap>>, req: Request) -> Response {
    let path = req.uri().path();
    if is_probe(path) {
        t.note(path);
        answer(path)
    } else {
        (StatusCode::NOT_FOUND, "not found\n").into_response()
    }
}

/// Today's count and the latest probes, for the page.
async fn caught(State(t): State<Arc<Trap>>) -> Json<serde_json::Value> {
    let now = (t.clock)();
    let s = t.seen.lock().unwrap_or_else(|p| p.into_inner());
    let today = if s.day == now / 86_400 { s.today } else { 0 };
    let recent: Vec<_> = s
        .recent
        .iter()
        .map(|(at, path)| json!({ "at": at, "path": path }))
        .collect();
    Json(json!({ "today": today, "total": s.total, "recent": recent, "now": now }))
}

pub fn routes(t: Arc<Trap>) -> Router {
    Router::new()
        .route("/caught", get(caught))
        .route("/robots.txt", get(|| async { ROBOTS }))
        .fallback(fallback)
        .with_state(t)
}
