//! # Portcullis MCP server
//!
//! The only surface an agent touches. Read the tool list below and you have
//! read everything an agent can do:
//!
//! | tool               | effect                                              |
//! |--------------------|-----------------------------------------------------|
//! | `propose_payment`  | asks; the kernel answers Allow or Refuse            |
//! | `get_proposal`     | reads the status of one of *your own* proposals     |
//! | `get_balance`      | reads what you may draw on                          |
//! | `list_commitments` | reads the scheduled payments you may draw against   |
//!
//! There is no `add_recipient`, `approve`, `raise_limit`, `refund`, or policy
//! write of any kind. Not permission-gated: absent. You cannot call a function
//! that does not exist.
//!
//! Agents authenticate with a bearer token whose SHA-256 is configured on the
//! server. The token identifies the agent and grants exactly one thing — the
//! ability to propose. It is checked in an HTTP layer *in front of* MCP, so an
//! unauthenticated request never reaches tool code.

#![forbid(unsafe_code)]

pub mod money;
pub mod seed;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use portcullis_executor::{Gatehouse, Rail};
use portcullis_kernel::{
    AgentId, Amount, AssetId, CommitmentId, Decision, Proposal, ProposalId, Timestamp,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Extension, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Longest `purpose` accepted. It is untrusted text that lands in the audit
/// log; there is no reason to let an agent write a novel into it.
pub const MAX_PURPOSE: usize = 1000;

/// The clock, injected so tests are deterministic.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// Where decisions are written. The binary prints JSON lines; tests collect.
pub type AuditSink = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

// ── auth ────────────────────────────────────────────────────────────────────

/// SHA-256 of a bearer token → the agent it identifies.
#[derive(Clone, Default)]
pub struct AgentTokens(Arc<HashMap<String, AgentId>>);

pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl AgentTokens {
    /// From `name:sha256hex,name:sha256hex`. Raw tokens are never configured.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut m = HashMap::new();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (name, hash) = entry
                .split_once(':')
                .ok_or_else(|| format!("expected name:sha256hex, got {entry:?}"))?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("{name}: not a sha256 hex digest"));
            }
            m.insert(hash.to_ascii_lowercase(), AgentId::new(name));
        }
        if m.is_empty() {
            return Err("no agent tokens configured".into());
        }
        Ok(AgentTokens(Arc::new(m)))
    }

    pub fn identify(&self, token: &str) -> Option<&AgentId> {
        self.0.get(&token_hash(token))
    }
}

/// Axum middleware: no valid bearer token, no MCP. On success the agent's
/// identity rides along in the request extensions for the tools to read.
pub async fn require_agent(
    State(tokens): State<AgentTokens>,
    mut req: Request,
    next: Next,
) -> Response {
    let agent = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| tokens.identify(t.trim()))
        .cloned();
    match agent {
        Some(a) => {
            req.extensions_mut().insert(a);
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "agent token required").into_response(),
    }
}

// ── tool parameters ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProposePayment {
    /// Where the money should go: an email address, an E.164 phone number
    /// (`+13175550123`), or a 13-character PayPal payer id. Exactly as written;
    /// nothing is trimmed or corrected.
    pub recipient: String,
    /// Decimal amount as a string, e.g. `"12.50"`. No symbols, signs or exponents.
    pub amount: String,
    /// ISO currency code, e.g. `"USD"`.
    pub currency: String,
    /// If this pays a scheduled commitment (see `list_commitments`), its id.
    /// Naming one is an assertion: if it does not match, the proposal is
    /// refused rather than paid from discretionary funds.
    #[serde(default)]
    pub commitment: Option<String>,
    /// Why. Shown to the human and logged. It is never read by the decision.
    pub purpose: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetProposal {
    pub proposal_id: String,
}

// ── the server ──────────────────────────────────────────────────────────────

pub struct Portcullis<R: Rail + 'static> {
    gate: Arc<Gatehouse<R>>,
    clock: Clock,
    audit: AuditSink,
    /// Which agent made which proposal, so an agent can only read its own.
    owners: Arc<Mutex<HashMap<ProposalId, (AgentId, Decision)>>>,
    tool_router: ToolRouter<Self>,
}

// By hand: `derive(Clone)` would demand `R: Clone`, and the rail is behind an Arc.
impl<R: Rail + 'static> Clone for Portcullis<R> {
    fn clone(&self) -> Self {
        Self {
            gate: self.gate.clone(),
            clock: self.clock.clone(),
            audit: self.audit.clone(),
            owners: self.owners.clone(),
            tool_router: self.tool_router.clone(),
        }
    }
}

fn agent_of(parts: &http::request::Parts) -> Result<AgentId, ErrorData> {
    parts
        .extensions
        .get::<AgentId>()
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("unauthenticated request reached a tool", None))
}

#[tool_router]
impl<R: Rail + 'static> Portcullis<R> {
    pub fn new(gate: Arc<Gatehouse<R>>, clock: Clock, audit: AuditSink) -> Self {
        Self {
            gate,
            clock,
            audit,
            owners: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Propose a payment. You cannot authorize payments: a deterministic \
policy you cannot see or change decides, and the answer is final. 'allowed' means the \
payment is queued for settlement (check it with get_proposal). 'refused' names the rule \
that refused it; re-proposing the same thing will be refused the same way."
    )]
    async fn propose_payment(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ProposePayment>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        if p.purpose.chars().count() > MAX_PURPOSE {
            return Err(ErrorData::invalid_params(
                format!("purpose is limited to {MAX_PURPOSE} characters"),
                None,
            ));
        }
        let ledger_asset = self.gate.with_state(|s| s.ledger.asset.clone()).await;
        // A foreign currency is proposed as-is so the kernel refuses it by
        // name and it lands in the log, rather than vanishing as a protocol error.
        let asset = if p.currency == ledger_asset.symbol {
            ledger_asset.clone()
        } else {
            AssetId::new(p.currency.clone(), ledger_asset.decimals)
        };
        let minor = money::parse_amount(&p.amount, asset.decimals).map_err(|e| {
            ErrorData::invalid_params(format!("amount {:?}: {e:?}", p.amount), None)
        })?;

        let now = (self.clock)();
        let proposal = Proposal {
            // Assigned here, never by the agent: an agent choosing its own id
            // could collide with, or replay, one it has seen.
            id: ProposalId::new(uuid::Uuid::new_v4().to_string()),
            agent: agent.clone(),
            recipient: p.recipient,
            amount: Amount::new(asset, minor),
            proposed_at: now,
            against: p.commitment.map(CommitmentId::new),
            purpose: p.purpose,
        };
        let adj = self.gate.propose(&proposal, now).await;
        let decision = adj.decision().clone();
        self.owners
            .lock()
            .await
            .insert(proposal.id.clone(), (agent, decision.clone()));

        (self.audit)(json!({
            "kind": "decision",
            "at": now,
            "proposal": &proposal,
            "adjudication": &adj,
        }));

        let body = match &decision {
            Decision::Allow { .. } => json!({
                "proposal_id": proposal.id,
                "decision": "allowed",
                "status": "queued for settlement",
            }),
            Decision::Refuse { rule } => json!({
                "proposal_id": proposal.id,
                "decision": "refused",
                "rule": rule,
                "reason": rule.reason(),
            }),
        };
        Ok(CallToolResult::structured(body))
    }

    #[tool(description = "Status of one of your own proposals.")]
    async fn get_proposal(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(GetProposal { proposal_id }): Parameters<GetProposal>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        let id = ProposalId::new(proposal_id);
        let owned = self.owners.lock().await.get(&id).cloned();
        // Someone else's proposal and a nonexistent one look the same.
        let Some((owner, decision)) = owned.filter(|(o, _)| o == &agent) else {
            return Ok(CallToolResult::error(vec![
                rmcp::model::ContentBlock::text("no such proposal"),
            ]));
        };
        let _ = owner;
        let execution = self.gate.with_state(|s| s.execution(&id).cloned()).await;
        Ok(CallToolResult::structured(json!({
            "proposal_id": id,
            "decision": match &decision { Decision::Allow{..} => "allowed", Decision::Refuse{..} => "refused" },
            "rule": decision.refused_by(),
            "execution": execution,
        })))
    }

    #[tool(
        description = "What you may currently draw on, after scheduled payments \
and in-flight payments are set aside."
    )]
    async fn get_balance(&self) -> Result<CallToolResult, ErrorData> {
        let now = (self.clock)();
        let (asset, available) = self
            .gate
            .with_state(|s| (s.ledger.asset.clone(), s.ledger.available(now)))
            .await;
        Ok(CallToolResult::structured(json!({
            "currency": asset.symbol,
            "available": Amount::new(asset, available).decimal_string(),
        })))
    }

    #[tool(
        description = "Scheduled payments you may draw against by naming them in \
propose_payment's `commitment` field. Each one is a specific recipient, amount and window."
    )]
    async fn list_commitments(&self) -> Result<CallToolResult, ErrorData> {
        let now = (self.clock)();
        let list = self
            .gate
            .with_state(|s| {
                s.ledger
                    .commitments
                    .iter()
                    .filter(|c| c.is_open() && !c.window.has_closed(now))
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "recipient": c.recipient,
                            "amount": s.ledger.amount(c.earmarked).decimal_string(),
                            "currency": s.ledger.asset.symbol,
                            "opens_at": c.window.from,
                            "closes_at": c.window.until,
                            "open_now": c.window.contains(now),
                            "memo": c.memo,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .await;
        Ok(CallToolResult::structured(json!({ "commitments": list })))
    }
}

#[tool_handler]
impl<R: Rail + 'static> ServerHandler for Portcullis<R> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("portcullis", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Portcullis holds the money. You may propose payments and read balances. \
                 You cannot approve, add recipients, or change limits — those tools do not exist.",
            )
    }
}

// ── wiring ──────────────────────────────────────────────────────────────────

/// The full HTTP app: auth layer in front, MCP at `/mcp`, a health check at `/`.
pub fn app<R: Rail + 'static>(
    server: Portcullis<R>,
    tokens: AgentTokens,
    allowed_hosts: Vec<String>,
) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts);
    let mcp: StreamableHttpService<Portcullis<R>, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
    axum::Router::new()
        .nest_service("/mcp", mcp)
        .layer(axum::middleware::from_fn_with_state(tokens, require_agent))
        .route("/health", axum::routing::get(|| async { "ok" }))
}
