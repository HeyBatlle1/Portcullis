//! # The console
//!
//! A read-only window onto the gate, for humans. It is served by the same
//! process but sits *outside* the agent's surface: nothing here is reachable
//! with an agent token, and nothing here can propose, approve or change policy.
//!
//! | route                  | what it does                                          |
//! |------------------------|-------------------------------------------------------|
//! | `GET  /console`        | the page                                              |
//! | `GET  /console/stream` | server-sent events: audit records, state, agent steps |
//! | `GET  /console/verify` | walks the whole audit chain and reports the head      |
//! | `GET  /console/audit.jsonl` | the chain, one record per line, to verify yourself |
//! | `POST /console/run`    | runs the demo scenario once (rate limited)            |
//!
//! `run` is the one thing here with an effect, and the effect is bounded: it
//! plays the owner approving one invoice, then starts the demo agent, which
//! reaches the money only through `/mcp` like any other agent.

use crate::{money::parse_amount, Clock, SharedAudit};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use portcullis_demo::{scenario::Scenario, AgentEvent};
use portcullis_executor::{Gatehouse, Rail};
use portcullis_kernel::{
    forecast, hex, AuditRecord, Commitment, CommitmentId, CommitmentState, RecipientId, TimeWindow,
    Timestamp,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, Notify};

const PAGE: &str = include_str!("../console/index.html");

/// Seconds between demo runs, so one visitor cannot monopolise it.
pub const RUN_COOLDOWN: u64 = 30;
/// Hard wall-clock limit on one run.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// Fan-out of everything the page shows. Audit records go out as they are
/// chained; agent steps as they happen; state whenever either changes.
pub struct Hub {
    tx: broadcast::Sender<Arc<str>>,
    changed: Notify,
    /// The latest run's steps, so a page opened mid-run can catch up.
    run_log: Mutex<Vec<Value>>,
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Hub {
            tx,
            changed: Notify::new(),
            run_log: Mutex::new(Vec::new()),
        })
    }

    /// Everything the page would receive, from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    fn send(&self, v: Value) {
        let _ = self.tx.send(Arc::from(v.to_string()));
    }

    /// Called by the audit sink after each append.
    pub fn record(&self, r: &AuditRecord, len: usize) {
        self.send(json!({ "ch": "audit", "record": r, "len": len }));
        self.changed.notify_one();
    }

    fn agent(&self, e: &AgentEvent) {
        let v = serde_json::to_value(e).unwrap_or_default();
        {
            let mut log = self.run_log.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(e, AgentEvent::Started { .. }) {
                log.clear();
            }
            log.push(v.clone());
        }
        self.send(json!({ "ch": "agent", "event": v }));
    }
}

/// What the demo runner needs. Absent when no model key is configured; the
/// console still works, it just cannot start a run.
pub struct Demo {
    /// The first is the default.
    pub scenarios: Vec<Scenario>,
    pub models: Vec<String>,
    pub openrouter_key: String,
    pub agent_token: String,
    pub mcp_url: String,
    pub daily_runs: u32,
    /// Chat-completions endpoint; `portcullis_demo::OPENROUTER` in production.
    pub model_url: String,
}

#[derive(Default)]
struct Runs {
    count: u64,
    busy: bool,
    last_start: Timestamp,
    day: u64,
    today: u32,
}

pub struct Console<R: Rail + 'static> {
    gate: Arc<Gatehouse<R>>,
    chain: SharedAudit,
    hub: Arc<Hub>,
    clock: Clock,
    audit: crate::AuditSink,
    demo: Option<Demo>,
    runs: Mutex<Runs>,
}

impl<R: Rail + 'static> Console<R> {
    pub fn new(
        gate: Arc<Gatehouse<R>>,
        chain: SharedAudit,
        hub: Arc<Hub>,
        clock: Clock,
        audit: crate::AuditSink,
        demo: Option<Demo>,
    ) -> Arc<Self> {
        let c = Arc::new(Console {
            gate,
            chain,
            hub,
            clock,
            audit,
            demo,
            runs: Mutex::new(Runs::default()),
        });
        // State goes out whenever the chain moves. Coalesced: a burst of
        // records produces one snapshot.
        let me = c.clone();
        tokio::spawn(async move {
            loop {
                me.hub.changed.notified().await;
                let s = me.state().await;
                me.hub.send(json!({ "ch": "state", "state": s }));
            }
        });
        c
    }

    /// The gate as a human reads it. Every number is computed by the kernel.
    pub async fn state(&self) -> Value {
        let now = (self.clock)();
        self.gate
            .with_state(|s| {
                let l = &s.ledger;
                let amt = |m: u64| l.amount(m).decimal_string();
                let commitments: Vec<Value> = l
                    .commitments
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id, "recipient": c.recipient, "amount": amt(c.earmarked),
                            "state": c.state, "opens": c.window.from, "closes": c.window.until,
                            "drawable": c.is_drawable(now), "memo": c.memo,
                        })
                    })
                    .collect();
                let fc: Vec<Value> = forecast(l, now)
                    .into_iter()
                    .map(|f| json!({ "id": f.id, "amount": amt(f.earmarked), "due_by": f.due_by, "funded": f.funded }))
                    .collect();
                let executions: Vec<Value> = s
                    .executions()
                    .map(|(id, e)| json!({ "proposal": id, "execution": e }))
                    .collect();
                json!({
                    "now": now,
                    "currency": l.asset.symbol,
                    "balance": amt(l.balance),
                    "reserved": amt(l.reserved_total()),
                    "encumbered": amt(l.encumbered(now)),
                    "available": amt(l.available(now)),
                    "commitments": commitments,
                    "forecast": fc,
                    "executions": executions,
                    "policy": {
                        "version": s.policy.version,
                        "head": hex(&s.policy.head),
                        "rules": s.policy.rules,
                        "standing": s.policy.standing.iter().map(|r| json!({
                            "recipient": r.recipient, "active_from": r.active_from,
                        })).collect::<Vec<_>>(),
                    },
                })
            })
            .await
    }

    fn chain_status(&self) -> Value {
        let c = self.chain.lock().unwrap_or_else(|p| p.into_inner());
        let verdict = match c.verify() {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": format!("{e:?}") }),
        };
        json!({ "len": c.len(), "head": hex(&c.head()), "verify": verdict })
    }

    fn demo_info(&self) -> Value {
        let runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        match &self.demo {
            None => json!({ "enabled": false }),
            Some(d) => json!({
                "enabled": true,
                "models": d.models,
                "hostile": portcullis_demo::hostile::ID,
                "scenarios": d.scenarios.iter().map(|s| json!({
                    "id": s.id, "title": s.title, "summary": s.summary,
                })).collect::<Vec<_>>(),
                "busy": runs.busy,
                "runs": runs.count,
                "cooldown": RUN_COOLDOWN,
            }),
        }
    }

    async fn hello(&self) -> Value {
        let records: Vec<AuditRecord> = {
            let c = self.chain.lock().unwrap_or_else(|p| p.into_inner());
            let r = c.records();
            r[r.len().saturating_sub(500)..].to_vec()
        };
        let run_log = self
            .hub
            .run_log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        json!({
            "ch": "hello",
            "chain": self.chain_status(),
            "records": records,
            "state": self.state().await,
            "demo": self.demo_info(),
            "run_log": run_log,
        })
    }

    /// Start one demo run. The owner approves this run's invoice (withdrawing
    /// the previous run's, if nobody paid it), then the agent works the inbox.
    async fn start(
        self: &Arc<Self>,
        model: Option<String>,
        scenario: Option<String>,
    ) -> Result<Value, (StatusCode, String)> {
        let Some(demo) = &self.demo else {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "demo agent not configured".into(),
            ));
        };
        let scenario = match scenario {
            None => &demo.scenarios[0],
            Some(id) => demo.scenarios.iter().find(|s| s.id == id).ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("scenario {id:?} not offered"),
                )
            })?,
        };
        let hostile = portcullis_demo::hostile::ID;
        let model = match model {
            Some(m) if demo.models.contains(&m) || m == hostile => m,
            Some(m) => return Err((StatusCode::BAD_REQUEST, format!("model {m:?} not offered"))),
            None => demo.models[0].clone(),
        };
        let now = (self.clock)();
        let run = {
            let mut r = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            if r.busy {
                return Err((StatusCode::CONFLICT, "a run is in progress".into()));
            }
            if r.count > 0 && now < r.last_start + RUN_COOLDOWN {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("next run in {}s", r.last_start + RUN_COOLDOWN - now),
                ));
            }
            let day = now / 86_400;
            if r.day != day {
                r.day = day;
                r.today = 0;
            }
            if r.today >= demo.daily_runs {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    "daily demo limit reached".into(),
                ));
            }
            r.busy = true;
            r.today += 1;
            r.count += 1;
            r.last_start = now;
            r.count
        };

        if let Err(e) = self.approve(demo, scenario, run, now).await {
            self.runs.lock().unwrap_or_else(|p| p.into_inner()).busy = false;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
        }

        let me = self.clone();
        let cfg = portcullis_demo::Config {
            run,
            mcp_url: demo.mcp_url.clone(),
            agent_token: demo.agent_token.clone(),
            openrouter_key: demo.openrouter_key.clone(),
            model: model.clone(),
            model_url: demo.model_url.clone(),
        };
        let inbox = scenario.inbox(run);
        let scenario_owned = scenario.clone();
        tokio::spawn(async move {
            let hub = me.hub.clone();
            let emit = move |e: AgentEvent| hub.agent(&e);
            let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                if cfg.model == portcullis_demo::hostile::ID {
                    Box::pin(portcullis_demo::hostile::run(
                        cfg,
                        scenario_owned,
                        emit.clone(),
                    ))
                } else {
                    Box::pin(portcullis_demo::run(cfg, inbox, emit.clone()))
                };
            if tokio::time::timeout(RUN_TIMEOUT, fut).await.is_err() {
                emit(AgentEvent::Failed {
                    run,
                    error: "run timed out".into(),
                });
            }
            me.runs.lock().unwrap_or_else(|p| p.into_inner()).busy = false;
            me.hub.send(json!({ "ch": "demo", "demo": me.demo_info() }));
        });
        self.hub
            .send(json!({ "ch": "demo", "demo": self.demo_info() }));
        Ok(json!({
            "run": run, "model": model, "scenario": scenario.id, "invoice": scenario.invoice(run),
        }))
    }

    /// The human half of the scenario, recorded in the chain like everything else.
    async fn approve(
        &self,
        demo: &Demo,
        s: &Scenario,
        run: u64,
        now: Timestamp,
    ) -> Result<(), String> {
        // Every scenario numbers invoices the same way, so the previous run's
        // bill is found by number whichever scenario it came from.
        if run > 1 {
            let prev = CommitmentId::new(demo.scenarios[0].invoice(run - 1));
            if self.gate.withdraw(&prev).await {
                (self.audit)(json!({
                    "kind": "approval",
                    "at": now,
                    "action": "withdrawn",
                    "commitment": prev,
                    "by": "owner",
                    "note": "previous demo run left this invoice unpaid; approval withdrawn before the next run",
                }));
            }
        }
        let recipient = RecipientId::parse(&s.vendor_payee)
            .map_err(|e| format!("scenario vendor_payee: {e:?}"))?;
        let earmarked =
            parse_amount(&s.amount, 2).map_err(|e| format!("scenario amount: {e:?}"))?;
        let id = CommitmentId::new(s.invoice(run));
        let c = Commitment {
            id: id.clone(),
            recipient: recipient.clone(),
            earmarked,
            window: TimeWindow::new(now.saturating_sub(60), now + s.payable_days * 86_400),
            state: CommitmentState::Open,
            memo: format!(
                "Northline Glass Supply - {} (Meridian Tower IGUs)",
                s.invoice(run)
            ),
        };
        self.gate.schedule(c.clone()).await?;
        (self.audit)(json!({
            "kind": "approval",
            "at": now,
            "action": "approved",
            "by": "owner",
            "commitment": c,
        }));
        Ok(())
    }
}

#[derive(Deserialize)]
struct RunReq {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    scenario: Option<String>,
}

pub fn routes<R: Rail + 'static>(console: Arc<Console<R>>) -> Router {
    Router::new()
        .route("/console", get(|| async { Html(PAGE) }))
        .route(
            "/",
            get(|| async { axum::response::Redirect::temporary("/console") }),
        )
        .route("/console/stream", get(stream::<R>))
        .route("/console/verify", get(verify::<R>))
        .route("/console/audit.jsonl", get(export::<R>))
        .route("/console/run", post(run::<R>))
        .with_state(console)
}

async fn stream<R: Rail + 'static>(State(c): State<Arc<Console<R>>>) -> Response {
    let rx = c.hub.tx.subscribe();
    let hello = c.hello().await;
    let first = futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(SseEvent::default().data(hello.to_string()))
    });
    let rest = futures_util::stream::unfold(rx, |mut rx| async move {
        let data: Arc<str> = match rx.recv().await {
            Ok(msg) => msg,
            // A slow page missed some messages. Tell it to reload rather than
            // show a gap.
            Err(broadcast::error::RecvError::Lagged(_)) => Arc::from(r#"{"ch":"lagged"}"#),
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(SseEvent::default().data(&*data)), rx))
    });
    use futures_util::StreamExt;
    Sse::new(first.chain(rest))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn verify<R: Rail + 'static>(State(c): State<Arc<Console<R>>>) -> Json<Value> {
    Json(c.chain_status())
}

async fn export<R: Rail + 'static>(State(c): State<Arc<Console<R>>>) -> Response {
    let body: String = {
        let chain = c.chain.lock().unwrap_or_else(|p| p.into_inner());
        chain
            .records()
            .iter()
            .map(|r| serde_json::to_string(r).unwrap_or_default() + "\n")
            .collect()
    };
    ([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

async fn run<R: Rail + 'static>(
    State(c): State<Arc<Console<R>>>,
    body: Option<Json<RunReq>>,
) -> Response {
    let (model, scenario) = body
        .map(|Json(b)| (b.model, b.scenario))
        .unwrap_or_default();
    match c.start(model, scenario).await {
        Ok(v) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Err((code, msg)) => (code, Json(json!({ "error": msg }))).into_response(),
    }
}
