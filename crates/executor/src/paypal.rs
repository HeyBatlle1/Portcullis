//! PayPal Payouts, sandbox only.
//!
//! Three parts, split so the parts that matter are testable without a network:
//!
//! - [`payout_request`] — pure. A [`Payment`] in, the exact JSON body and
//!   idempotency key out, or the reason it cannot be sent at all.
//! - [`classify_create`] / [`classify_status`] — pure. An HTTP status and body
//!   in, a [`Submitted`] / [`Observed`] out. This is where "nothing happened"
//!   is told apart from "we don't know", and that line is the one that loses
//!   money if it is drawn wrong.
//! - [`PayPal`] — the thin HTTP shell around them: OAuth, token refresh,
//!   headers.

use crate::rail::{ItemStatus, Observed, Rail, Submitted};
use portcullis_kernel::{Payment, RecipientKind};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// The only remote PayPal host this crate will talk to.
///
/// There is no live-API constant anywhere in this crate, and no configuration
/// that can supply one. Pointing Portcullis at real money is a code change,
/// made on purpose, in review — not an environment variable.
pub const SANDBOX: &str = "https://api-m.sandbox.paypal.com";

/// Where requests go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint(String);

impl Endpoint {
    pub fn sandbox() -> Self {
        Endpoint(SANDBOX.to_string())
    }

    /// A loopback server, for tests. Anything that is not loopback is refused.
    pub fn local(base: &str) -> Result<Self, String> {
        let ok = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|p| base.starts_with(p));
        if ok {
            Ok(Endpoint(base.trim_end_matches('/').to_string()))
        } else {
            Err(format!("refusing non-loopback endpoint: {base}"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Client credentials. `Debug` never prints the secret.
#[derive(Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

impl Credentials {
    /// From `PAYPAL_CLIENT_ID` / `PAYPAL_CLIENT_SECRET`. Also requires
    /// `PAYPAL_ENV=sandbox`, so a `.env` copied from somewhere else fails loudly
    /// instead of quietly.
    pub fn from_env() -> Result<Self, String> {
        let get = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
        if get("PAYPAL_ENV")? != "sandbox" {
            return Err("PAYPAL_ENV must be exactly \"sandbox\"".into());
        }
        let c = Credentials {
            client_id: get("PAYPAL_CLIENT_ID")?,
            client_secret: get("PAYPAL_CLIENT_SECRET")?,
        };
        if c.client_id.is_empty() || c.client_secret.is_empty() {
            return Err("PayPal credentials are empty".into());
        }
        Ok(c)
    }
}

// ── the request body ────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CreatePayout {
    pub sender_batch_header: BatchHeader,
    pub items: Vec<Item>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct BatchHeader {
    pub sender_batch_id: String,
    pub recipient_type: &'static str,
    pub email_subject: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub recipient_type: &'static str,
    pub receiver: String,
    pub amount: Money,
    pub sender_item_id: String,
    pub note: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub value: String,
    pub currency: String,
}

/// Why a payment can never be sent through Payouts. Each one is decided before
/// any request is made, so it is always a safe release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unpayable {
    /// An EVM address. Payouts cannot send to one; that is the PYUSD adapter's job.
    NotAPayPalRecipient,
    /// Only currencies whose precision we have checked against the asset.
    Currency(String),
    /// The proposal id is the idempotency key and the item id: 1–63 of
    /// `[A-Za-z0-9_-]`, because PayPal caps `sender_item_id` at 63.
    BadProposalId,
}

/// Currencies we will send, and the precision PayPal expects for each. The
/// asset's declared decimals must agree, or `1.00` might go out as `100`.
const CURRENCIES: &[(&str, u8)] = &[("USD", 2)];

/// The one fixed note on every payout. The agent's `purpose` never reaches the
/// recipient: it is attacker-controllable text, and a payment memo is a
/// delivery channel straight into someone else's inbox.
pub const NOTE: &str = "Payment via Portcullis";

/// Pure. Build the exact request for a payment, or say why there is none.
pub fn payout_request(p: &Payment) -> Result<CreatePayout, Unpayable> {
    let recipient_type = match p.to.kind() {
        RecipientKind::Email => "EMAIL",
        RecipientKind::Phone => "PHONE",
        RecipientKind::PayPalId => "PAYPAL_ID",
        RecipientKind::Evm => return Err(Unpayable::NotAPayPalRecipient),
    };
    let symbol = p.amount.asset.symbol.as_str();
    if !CURRENCIES
        .iter()
        .any(|(c, d)| *c == symbol && *d == p.amount.asset.decimals)
    {
        return Err(Unpayable::Currency(symbol.to_string()));
    }
    let id = p.proposal.as_str();
    if id.is_empty()
        || id.len() > 63
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Unpayable::BadProposalId);
    }
    Ok(CreatePayout {
        sender_batch_header: BatchHeader {
            sender_batch_id: id.to_string(),
            recipient_type,
            email_subject: "You have a payment".into(),
        },
        items: vec![Item {
            recipient_type,
            receiver: p.to.as_str().to_string(),
            amount: Money {
                value: p.amount.decimal_string(),
                currency: symbol.to_string(),
            },
            sender_item_id: id.to_string(),
            note: NOTE.into(),
        }],
    })
}

// ── reading responses ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateResponse {
    batch_header: CreatedHeader,
}

#[derive(Deserialize)]
struct CreatedHeader {
    payout_batch_id: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Deserialize)]
struct Link {
    href: String,
}

/// Pure. Decide what a create-payout response established.
///
/// The rule: anything we cannot positively identify as "nothing was created"
/// is `Unknown`. A 201 we cannot parse is `Unknown`, not `Rejected`, because
/// PayPal said it *created* something.
pub fn classify_create(status: u16, body: &str) -> Submitted {
    match status {
        200 | 201 => match serde_json::from_str::<CreateResponse>(body) {
            Ok(r) => Submitted::Accepted {
                batch_id: r.batch_header.payout_batch_id,
            },
            Err(e) => Submitted::Unknown {
                reason: format!("{status} with unreadable body: {e}"),
            },
        },
        // Rate-limited or throttled: nothing processed, but retrying beats
        // releasing — the agent did nothing wrong.
        429 => Submitted::Unknown {
            reason: "rate limited".into(),
        },
        400..=499 => {
            let e: ErrorBody = serde_json::from_str(body).unwrap_or(ErrorBody {
                name: String::new(),
                message: body.chars().take(200).collect(),
                links: Vec::new(),
            });
            // A duplicate sender_batch_id is rejected with a link to the
            // original payout. That is not a failure — it is the rail telling
            // us an earlier attempt landed.
            if let Some(id) = e.links.iter().find_map(|l| batch_id_from_href(&l.href)) {
                return Submitted::Accepted { batch_id: id };
            }
            Submitted::Rejected {
                reason: format!("{status} {}: {}", e.name, e.message),
            }
        }
        _ => Submitted::Unknown {
            reason: format!("HTTP {status}"),
        },
    }
}

fn batch_id_from_href(href: &str) -> Option<String> {
    let tail = href.split("/v1/payments/payouts/").nth(1)?;
    let id: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (!id.is_empty()).then_some(id)
}

#[derive(Deserialize)]
struct StatusResponse {
    batch_header: StatusHeader,
    #[serde(default)]
    items: Vec<StatusItem>,
}

#[derive(Deserialize)]
struct StatusHeader {
    batch_status: String,
}

#[derive(Deserialize)]
struct StatusItem {
    transaction_status: String,
}

/// Pure. Read a payout-batch status response. One item per batch, always —
/// [`payout_request`] never sends more.
pub fn classify_status(status: u16, body: &str) -> Observed {
    if status != 200 {
        return Observed::Unknown {
            reason: format!("HTTP {status}"),
        };
    }
    let r: StatusResponse = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return Observed::Unknown {
                reason: format!("unreadable status: {e}"),
            }
        }
    };
    if let Some(item) = r.items.first() {
        return match item.transaction_status.as_str() {
            "PENDING" => Observed::Status(ItemStatus::Pending),
            "ONHOLD" => Observed::Status(ItemStatus::OnHold),
            "SUCCESS" => Observed::Status(ItemStatus::Success),
            "UNCLAIMED" => Observed::Status(ItemStatus::Unclaimed),
            "FAILED" => Observed::Status(ItemStatus::Failed),
            "BLOCKED" => Observed::Status(ItemStatus::Blocked),
            "RETURNED" => Observed::Status(ItemStatus::Returned),
            "REFUNDED" => Observed::Status(ItemStatus::Refunded),
            "REVERSED" => Observed::Status(ItemStatus::Reversed),
            other => Observed::Unknown {
                reason: format!("unrecognized item status {other:?}"),
            },
        };
    }
    match r.batch_header.batch_status.as_str() {
        "DENIED" | "CANCELED" => Observed::Status(ItemStatus::Failed),
        // Batch accepted, items not materialized yet.
        _ => Observed::Status(ItemStatus::Pending),
    }
}

// ── the HTTP shell ──────────────────────────────────────────────────────────

struct Token {
    bearer: String,
    good_until: SystemTime,
}

/// Refresh this long before PayPal says the token expires (they last up to
/// ~9 hours). A request should never race an expiry.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);

pub struct PayPal {
    endpoint: Endpoint,
    creds: Credentials,
    http: reqwest::Client,
    token: Mutex<Option<Token>>,
}

impl PayPal {
    pub fn new(endpoint: Endpoint, creds: Credentials) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds with static config");
        PayPal {
            endpoint,
            creds,
            http,
            token: Mutex::new(None),
        }
    }

    async fn bearer(&self) -> Result<String, String> {
        let mut slot = self.token.lock().await;
        if let Some(t) = slot.as_ref() {
            if SystemTime::now() < t.good_until {
                return Ok(t.bearer.clone());
            }
        }
        #[derive(Deserialize)]
        struct Grant {
            access_token: String,
            expires_in: u64,
        }
        let resp = self
            .http
            .post(format!("{}/v1/oauth2/token", self.endpoint.as_str()))
            .basic_auth(&self.creds.client_id, Some(&self.creds.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|e| format!("token request: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("token request: HTTP {}", resp.status()));
        }
        let g: Grant = resp.json().await.map_err(|e| format!("token body: {e}"))?;
        let life = Duration::from_secs(g.expires_in).saturating_sub(TOKEN_MARGIN);
        *slot = Some(Token {
            bearer: g.access_token.clone(),
            good_until: SystemTime::now() + life,
        });
        Ok(g.access_token)
    }

    async fn forget_token(&self) {
        *self.token.lock().await = None;
    }

    async fn post_once(&self, body: &CreatePayout, key: &str) -> Submitted {
        let bearer = match self.bearer().await {
            Ok(b) => b,
            // No token means no request went out: nothing can have been created.
            // Still Unknown rather than Rejected, so a transient auth outage
            // retries instead of bouncing every payment.
            Err(e) => return Submitted::Unknown { reason: e },
        };
        let sent = self
            .http
            .post(format!("{}/v1/payments/payouts", self.endpoint.as_str()))
            .bearer_auth(bearer)
            .header("PayPal-Request-Id", key)
            .json(body)
            .send()
            .await;
        match sent {
            Ok(r) => {
                let status = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                classify_create(status, &text)
            }
            // The request may have reached PayPal before the connection died.
            Err(e) => Submitted::Unknown {
                reason: format!("transport: {e}"),
            },
        }
    }
}

impl Rail for PayPal {
    async fn submit(&self, payment: &Payment) -> Submitted {
        let body = match payout_request(payment) {
            Ok(b) => b,
            Err(why) => {
                return Submitted::Rejected {
                    reason: format!("unpayable: {why:?}"),
                }
            }
        };
        let key = payment.proposal.as_str();
        match self.post_once(&body, key).await {
            // A stale token gets one refresh. The retry carries the same
            // idempotency key, so it cannot double-pay.
            Submitted::Rejected { reason } if reason.starts_with("401") => {
                self.forget_token().await;
                self.post_once(&body, key).await
            }
            other => other,
        }
    }

    async fn observe(&self, batch_id: &str) -> Observed {
        if !batch_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Observed::Unknown {
                reason: "malformed batch id".into(),
            };
        }
        let bearer = match self.bearer().await {
            Ok(b) => b,
            Err(e) => return Observed::Unknown { reason: e },
        };
        let r = self
            .http
            .get(format!(
                "{}/v1/payments/payouts/{batch_id}",
                self.endpoint.as_str()
            ))
            .bearer_auth(bearer)
            .send()
            .await;
        match r {
            Ok(r) => {
                let status = r.status().as_u16();
                if status == 401 {
                    self.forget_token().await;
                }
                let text = r.text().await.unwrap_or_default();
                classify_status(status, &text)
            }
            Err(e) => Observed::Unknown {
                reason: format!("transport: {e}"),
            },
        }
    }
}

/// Seconds since the epoch, for callers that need a kernel `Timestamp`.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
