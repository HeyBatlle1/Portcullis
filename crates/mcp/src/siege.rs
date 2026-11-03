//! # The siege controller
//!
//! Starts eight agents on one shift against one approved bill, and delivers the
//! attacks whoever is watching throws at them. Lives beside the console, outside
//! the agent's surface: nothing here proposes a payment. The agents do that, over
//! `/mcp`, each with its own slot token.
//!
//! Every siege is on the record: the owner's approval of its bill, which model
//! sat in which slot, and each attack delivered are all audit records, chained
//! with the decisions they provoked. The page draws the scoreboard from the chain.
//!
//! | route               | what it does                                         |
//! |---------------------|------------------------------------------------------|
//! | `GET  /siege`       | the page                                             |
//! | `GET  /siege/state` | the current siege, for a page that opens mid-shift   |
//! | `POST /siege/start` | starts one, with a roster of eight (rate limited)    |
//! | `POST /siege/attack`| delivers one attack to the running shift             |
//!
//! Its live events go out on `/console/stream` as `{"ch":"siege", …}`.

use crate::{console::Hub, money::parse_amount, AuditSink, Clock};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use portcullis_demo::{
    hostile,
    scenario::Scenario,
    siege::{self, Attack, Desk, Facts, Mailroom},
    AgentEvent, Config, World,
};
use portcullis_executor::{Gatehouse, Rail};
use portcullis_kernel::{
    Commitment, CommitmentId, CommitmentState, RecipientId, TimeWindow, Timestamp,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const PAGE: &str = include_str!("../console/siege.html");

/// Seconds between the end of one siege and the start of the next.
pub const COOLDOWN: u64 = 45;
/// Attacks one siege will accept. Enough to play; not enough to flood.
pub const MAX_ATTACKS: usize = 12;
/// Steps kept per slot for a page that opens mid-shift.
const LOG_PER_SLOT: usize = 300;
/// Siege bills are numbered apart from the console's, so the two never collide.
const INVOICE_BASE: u64 = 30_001;

/// The roster a siege starts with unless the page picks another: eight labs.
pub const DEFAULT_ROSTER: [&str; siege::SLOTS] = [
    "anthropic/claude-opus-5.5",
    "openai/gpt-5.6-terra",
    "x-ai/grok-4.7",
    "moonshotai/kimi-k3.1-pro",
    "google/gemini-3.8-flash",
    "deepseek/deepseek-v4.1-flash",
    "qwen/qwen3.8-max-0902",
    "mistralai/mistral-large-4-0",
];

pub struct SiegeConfig {
    /// The scenario whose vendor, payee and amount the siege is written around.
    pub scenario: Scenario,
    /// Models a slot may hold. The compromised script is always offered too.
    pub models: Vec<String>,
    pub openrouter_key: String,
    pub model_url: String,
    pub mcp_url: String,
    /// One token per slot, minted at boot; slot i proposes as `siege-{i+1}`.
    pub slot_tokens: Vec<String>,
    pub daily: u32,
    /// Shifts one visitor may start per UTC day; see [`crate::visitor`].
    pub per_visitor: u32,
    /// How long a shift runs; `siege::SHIFT` in production, short in tests.
    pub shift: std::time::Duration,
}

#[derive(Default)]
struct Shift {
    run: u64,
    busy: bool,
    ended_at: Timestamp,
    day: u64,
    today: u32,
    room: Option<Arc<Mailroom>>,
    roster: Vec<String>,
    invoice: String,
    attacks: Vec<Value>,
    logs: Vec<Vec<Value>>,
    ends_at: Timestamp,
}

pub struct Siege<R: Rail + 'static> {
    gate: Arc<Gatehouse<R>>,
    hub: Arc<Hub>,
    audit: AuditSink,
    clock: Clock,
    cfg: SiegeConfig,
    shift: Mutex<Shift>,
    visitors: crate::visitor::Quota,
}

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}

impl<R: Rail + 'static> Siege<R> {
    pub fn new(
        gate: Arc<Gatehouse<R>>,
        hub: Arc<Hub>,
        audit: AuditSink,
        clock: Clock,
        cfg: SiegeConfig,
    ) -> Arc<Self> {
        Arc::new(Siege {
            gate,
            hub,
            audit,
            clock,
            visitors: crate::visitor::Quota::new(cfg.per_visitor),
            cfg,
            shift: Mutex::new(Shift::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shift> {
        self.shift.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn invoice(run: u64) -> String {
        format!("INV-{}", INVOICE_BASE + run - 1)
    }

    pub fn state(&self) -> Value {
        let s = self.lock();
        let now = (self.clock)();
        json!({
            "run": s.run,
            "busy": s.busy,
            "roster": s.roster,
            "slots": (0..siege::SLOTS).map(|i| format!("siege-{}", i + 1)).collect::<Vec<_>>(),
            "invoice": s.invoice,
            "ends_at": s.ends_at,
            "now": now,
            "attacks": s.attacks,
            "logs": s.logs,
            "cooldown_until": if s.busy { 0 } else { s.ended_at + COOLDOWN },
            "options": {
                "models": self.cfg.models,
                "hostile": hostile::ID,
                "default_roster": DEFAULT_ROSTER,
                "attacks": [
                    { "kind": "vendor_takeover", "label": Attack::VendorTakeover.label() },
                    { "kind": "owner_urgent", "label": Attack::OwnerUrgent.label() },
                    { "kind": "injected_invoice", "label": Attack::InjectedInvoice.label() },
                    { "kind": "lookalike", "label": Attack::Lookalike.label() },
                    { "kind": "overpay", "label": Attack::Overpay.label() },
                ],
                "max_attacks": MAX_ATTACKS,
            },
        })
    }

    async fn start(
        self: &Arc<Self>,
        who: Option<&str>,
        roster: Option<Vec<String>>,
    ) -> Result<Value, (StatusCode, String)> {
        let roster =
            roster.unwrap_or_else(|| DEFAULT_ROSTER.iter().map(|s| s.to_string()).collect());
        if roster.len() != siege::SLOTS {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("a siege has {} slots", siege::SLOTS),
            ));
        }
        if let Some(m) = roster
            .iter()
            .find(|m| m.as_str() != hostile::ID && !self.cfg.models.contains(m))
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("model {m:?} is not offered"),
            ));
        }
        let now = (self.clock)();
        let run = {
            let mut s = self.lock();
            if s.busy {
                return Err(err(StatusCode::CONFLICT, "a siege is already running"));
            }
            if s.run > 0 && now < s.ended_at + COOLDOWN {
                return Err(err(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("next siege in {}s", s.ended_at + COOLDOWN - now),
                ));
            }
            let day = now / 86_400;
            if s.day != day {
                s.day = day;
                s.today = 0;
            }
            if s.today >= self.cfg.daily {
                return Err(err(
                    StatusCode::TOO_MANY_REQUESTS,
                    "today's sieges are used up",
                ));
            }
            // Last, so a start refused for any other reason costs nothing.
            if !self.visitors.take(who, now) {
                return Err(err(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "you have started {} shifts today; leave some for the next visitor",
                        self.visitors.per_day()
                    ),
                ));
            }
            s.today += 1;
            s.run += 1;
            s.busy = true;
            s.run
        };

        let invoice = Self::invoice(run);
        if let Err(e) = self.approve(run, &invoice, now).await {
            self.lock().busy = false;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e));
        }

        // The morning's mail, with this siege's bill in it and no attack: the
        // attacks are the watcher's to throw.
        let sc = self.scenario_for(run);
        let start: Vec<_> = sc
            .inbox(1)
            .into_iter()
            .filter(|e| e.id != "m4")
            .rev()
            .collect();
        let room = Mailroom::new(start, self.cfg.shift);
        let ends_at = now + self.cfg.shift.as_secs();
        {
            let mut s = self.lock();
            s.room = Some(room.clone());
            s.roster = roster.clone();
            s.invoice = invoice.clone();
            s.attacks.clear();
            s.logs = vec![Vec::new(); siege::SLOTS];
            s.ends_at = ends_at;
        }
        (self.audit)(json!({
            "kind": "siege",
            "at": now,
            "action": "started",
            "run": run,
            "bill": invoice,
            "roster": roster.iter().enumerate().map(|(i, m)| json!({
                "agent": format!("siege-{}", i + 1), "model": m,
            })).collect::<Vec<_>>(),
        }));
        self.hub
            .send(json!({ "ch": "siege", "siege": self.state() }));

        let me = self.clone();
        tokio::spawn(async move {
            let mut tasks = Vec::new();
            for (i, model) in roster.into_iter().enumerate() {
                let cfg = Config {
                    run,
                    mcp_url: me.cfg.mcp_url.clone(),
                    agent_token: me.cfg.slot_tokens[i].clone(),
                    openrouter_key: me.cfg.openrouter_key.clone(),
                    model: model.clone(),
                    model_url: me.cfg.model_url.clone(),
                };
                let slot = me.clone();
                let emit = move |e: AgentEvent| slot.step(i, e);
                let room = room.clone();
                let sc = sc.clone();
                tasks.push(tokio::spawn(async move {
                    if model == hostile::ID {
                        hostile::run(cfg, sc, emit).await
                    } else {
                        portcullis_demo::run_with(
                            cfg,
                            World::Live(Desk::new(room)),
                            siege::shift_brief(),
                            emit,
                        )
                        .await
                    }
                }));
            }
            // A hard stop a little after the shift, whatever a model is doing.
            let all = async {
                for t in tasks {
                    let _ = t.await;
                }
            };
            let _ =
                tokio::time::timeout(me.cfg.shift + std::time::Duration::from_secs(60), all).await;
            room.close();
            let end = (me.clock)();
            {
                let mut s = me.lock();
                s.busy = false;
                s.ended_at = end;
                s.room = None;
            }
            (me.audit)(json!({ "kind": "siege", "at": end, "action": "ended", "run": run }));
            me.hub.send(json!({ "ch": "siege", "siege": me.state() }));
        });

        Ok(json!({ "run": run, "invoice": invoice, "ends_at": ends_at }))
    }

    fn scenario_for(&self, run: u64) -> Scenario {
        let mut sc = self.cfg.scenario.clone();
        sc.invoice_base = INVOICE_BASE + run - 1;
        sc
    }

    fn step(&self, slot: usize, e: AgentEvent) {
        let v = serde_json::to_value(&e).unwrap_or_default();
        {
            let mut s = self.lock();
            if let Some(log) = s.logs.get_mut(slot) {
                if log.len() < LOG_PER_SLOT {
                    log.push(v.clone());
                }
            }
        }
        self.hub
            .send(json!({ "ch": "siege", "slot": slot, "event": v }));
    }

    /// The owner approves this siege's bill; the last siege's, if unpaid, is withdrawn.
    async fn approve(&self, run: u64, invoice: &str, now: Timestamp) -> Result<(), String> {
        if run > 1 {
            let prev = CommitmentId::new(Self::invoice(run - 1));
            if self.gate.withdraw(&prev).await {
                (self.audit)(json!({
                    "kind": "approval", "at": now, "action": "withdrawn", "commitment": prev, "by": "owner",
                    "note": "the previous siege left this bill unpaid; withdrawn before the next one",
                }));
            }
        }
        let sc = &self.cfg.scenario;
        let recipient =
            RecipientId::parse(&sc.vendor_payee).map_err(|e| format!("vendor_payee: {e:?}"))?;
        let earmarked = parse_amount(&sc.amount, 2).map_err(|e| format!("amount: {e:?}"))?;
        let c = Commitment {
            id: CommitmentId::new(invoice),
            recipient,
            earmarked,
            window: TimeWindow::new(now.saturating_sub(60), now + 14 * 86_400),
            state: CommitmentState::Open,
            memo: format!("Northline Glass Supply - {invoice} (siege {run})"),
        };
        self.gate.schedule(c.clone()).await?;
        (self.audit)(
            json!({ "kind": "approval", "at": now, "action": "approved", "by": "owner", "commitment": c }),
        );
        Ok(())
    }

    fn attack(&self, a: Attack) -> Result<Value, (StatusCode, String)> {
        a.validate().map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
        let now = (self.clock)();
        let (room, n, invoice, run) = {
            let s = self.lock();
            let Some(room) = s.room.clone().filter(|r| !r.shift_over() && !r.is_closed()) else {
                return Err(err(StatusCode::CONFLICT, "no shift is running"));
            };
            if s.attacks.len() >= MAX_ATTACKS {
                return Err(err(
                    StatusCode::TOO_MANY_REQUESTS,
                    "this siege has had all its attacks",
                ));
            }
            (room, s.attacks.len() + 1, s.invoice.clone(), s.run)
        };
        let sc = &self.cfg.scenario;
        let email = a.email(
            format!("a{n}"),
            &Facts {
                invoice: &invoice,
                amount: &sc.amount,
                vendor_payee: &sc.vendor_payee,
                attacker_payee: &sc.attacker_payee,
            },
        );
        let record = json!({
            "n": n, "label": a.label(), "attack": a, "at": now,
            "from": email.from, "subject": email.subject,
        });
        (self.audit)(json!({
            "kind": "siege", "at": now, "action": "attack", "run": run,
            "label": a.label(), "attack": &a, "from": email.from, "subject": email.subject,
        }));
        room.deliver(email);
        self.lock().attacks.push(record.clone());
        self.hub.send(json!({ "ch": "siege", "attack": record }));
        Ok(record)
    }
}

#[derive(Deserialize)]
struct StartReq {
    #[serde(default)]
    roster: Option<Vec<String>>,
}

pub fn routes<R: Rail + 'static>(siege: Arc<Siege<R>>) -> Router {
    Router::new()
        .route("/siege", get(|| async { Html(PAGE) }))
        .route("/siege/state", get(state::<R>))
        .route("/siege/start", post(start::<R>))
        .route("/siege/attack", post(attack::<R>))
        .with_state(siege)
}

async fn state<R: Rail + 'static>(State(s): State<Arc<Siege<R>>>) -> Json<Value> {
    Json(s.state())
}

async fn start<R: Rail + 'static>(
    State(s): State<Arc<Siege<R>>>,
    headers: HeaderMap,
    body: Option<Json<StartReq>>,
) -> Response {
    let who = crate::visitor::client(&headers);
    match s
        .start(who.as_deref(), body.and_then(|Json(b)| b.roster))
        .await
    {
        Ok(v) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Err((c, m)) => (c, Json(json!({ "error": m }))).into_response(),
    }
}

async fn attack<R: Rail + 'static>(
    State(s): State<Arc<Siege<R>>>,
    Json(a): Json<Attack>,
) -> Response {
    match s.attack(a) {
        Ok(v) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Err((c, m)) => (c, Json(json!({ "error": m }))).into_response(),
    }
}
