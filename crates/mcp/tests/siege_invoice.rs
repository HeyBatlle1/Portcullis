//! A siege whose bill is a PayPal invoice, offline. A fake PayPal plays both
//! sides PayPal would: the vendor's Invoicing API and, through a scripted rail,
//! the payout that settles. Everything else is real.
//!
//! What it pins down:
//! * the vendor issues the bill under the number PayPal suggests, addressed to
//!   the payer, with lines that add up to the approved amount, and sends it;
//! * the agents' inbox carries that number and PayPal's link, and the owner's
//!   approval is for that number, so the bill the agents pay is the invoice;
//! * once the payout settles, and not before, the vendor records it against
//!   the invoice exactly once, naming the payout batch;
//! * the issue and the payment are in a chain that verifies.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use portcullis_executor::{
    invoicing::Line, Credentials, Endpoint, Gatehouse, Invoicer, ItemStatus, Observed, Rail,
    Submitted,
};
use portcullis_kernel::{Payment, Timestamp};
use portcullis_mcp::{
    app,
    console::Hub,
    seed::{expand, Seed},
    siege::{self, Billing, Siege, SiegeConfig},
    AgentTokens, Portcullis, SharedAudit,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T0: Timestamp = 1_793_750_400; // 2026-11-04
const VENDOR: &str = "sb-vendor@business.example.com";
const PAYER: &str = "sb-payer@business.example.com";
const INVOICE_ID: &str = "INV2-TEST-AAAA-BBBB";
const BATCH: &str = "BATCH5UXD2E8A7";

/// Pays at once and reports it settled, once the test says the money left.
struct SettleRail {
    settled: Arc<AtomicBool>,
}
impl Rail for SettleRail {
    async fn submit(&self, _: &Payment) -> Submitted {
        Submitted::Accepted {
            batch_id: BATCH.into(),
        }
    }
    async fn observe(&self, _: &str) -> Observed {
        Observed::Status(if self.settled.load(Ordering::SeqCst) {
            ItemStatus::Success
        } else {
            ItemStatus::Pending
        })
    }
}

#[derive(Clone, Default)]
struct Fake {
    created: Arc<Mutex<Vec<Value>>>,
    sent: Arc<Mutex<u32>>,
    payments: Arc<Mutex<Vec<Value>>>,
}

fn paypal(f: Fake) -> Router {
    Router::new()
        .route(
            "/v1/oauth2/token",
            post(|| async { Json(json!({ "access_token": "t", "expires_in": 3600 })) }),
        )
        .route(
            "/v2/invoicing/generate-next-invoice-number",
            post(|| async { Json(json!({ "invoice_number": "INV-30007" })) }),
        )
        .route(
            "/v2/invoicing/invoices",
            post(|State(f): State<Fake>, Json(b): Json<Value>| async move {
                let number = b["detail"]["invoice_number"].clone();
                f.created.lock().unwrap().push(b);
                (
                    axum::http::StatusCode::CREATED,
                    Json(json!({ "id": INVOICE_ID, "status": "DRAFT", "detail": { "invoice_number": number } })),
                )
            }),
        )
        .route(
            "/v2/invoicing/invoices/{id}/send",
            post(|State(f): State<Fake>, Path(id): Path<String>| async move {
                assert_eq!(id, INVOICE_ID);
                *f.sent.lock().unwrap() += 1;
                Json(json!({ "rel": "payer-view", "href": format!("https://www.sandbox.paypal.com/invoice/p/#{id}") }))
            }),
        )
        .route(
            "/v2/invoicing/invoices/{id}/payments",
            post(|State(f): State<Fake>, Path(id): Path<String>, Json(b): Json<Value>| async move {
                assert_eq!(id, INVOICE_ID);
                f.payments.lock().unwrap().push(b);
                Json(json!({ "payment_id": "EXTR-1" }))
            }),
        )
        .route(
            "/v2/invoicing/invoices/{id}",
            get(|State(f): State<Fake>| async move {
                let paid = !f.payments.lock().unwrap().is_empty();
                Json(json!({ "id": INVOICE_ID, "status": if paid { "MARKED_AS_PAID" } else { "SENT" } }))
            }),
        )
        .with_state(f)
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
}

/// Clerk 1 reads the invoice and pays it by the number printed on it. The rest
/// wait out the shift.
async fn model(Json(req): Json<Value>) -> Json<Value> {
    let msgs = req["messages"].as_array().cloned().unwrap_or_default();
    let tools: Vec<String> = msgs
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect();
    let last = tools.last().cloned().unwrap_or_default();
    let msg = if last.contains("shift_over") {
        json!({ "content": "Shift over." })
    } else if req["model"] != "scripted/clerk-1" {
        json!({ "content": null, "tool_calls": [call(&format!("w{}", tools.len()), "wait_for_mail", json!({}))] })
    } else if tools.is_empty() {
        json!({ "content": null, "tool_calls": [call("r", "read_email", json!({ "id": "m1" }))] })
    } else if tools.len() == 1 {
        let body: Value = serde_json::from_str(&last).unwrap();
        let text = body["body"].as_str().unwrap();
        let number = text
            .split_whitespace()
            .find(|w| w.starts_with("INV-"))
            .unwrap()
            .to_string();
        json!({ "content": null, "tool_calls": [call("p", "propose_payment", json!({
            "recipient": VENDOR, "amount": "1284.60", "currency": "USD",
            "commitment": number, "purpose": "Northline invoice",
        }))] })
    } else {
        json!({ "content": null, "tool_calls": [call(&format!("w{}", tools.len()), "wait_for_mail", json!({}))] })
    };
    let mut m = msg;
    m["role"] = json!("assistant");
    Json(json!({ "choices": [ { "message": m } ] }))
}

async fn serve(r: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    format!("http://{a}")
}

#[tokio::test]
async fn the_bill_is_a_paypal_invoice_and_reads_paid_once_the_payout_settles() {
    std::env::set_var("PAYPAL_SANDBOX_RECEIVER", VENDOR);
    let seed = Seed::parse(include_str!("../../../demo/seed.json")).unwrap();
    let scenario = portcullis_demo::scenario::Scenario::parse(
        &expand(include_str!("../../../demo/scenario.json")).unwrap(),
    )
    .unwrap();
    let lines: Vec<Line> = scenario
        .lines
        .iter()
        .map(|l| Line {
            name: l.name.clone(),
            quantity: l.quantity,
            unit: l.unit.clone(),
        })
        .collect();
    assert_eq!(
        portcullis_executor::invoicing::total_minor(&lines),
        Some(128_460),
        "the demo bill's lines add up to its amount"
    );
    let (ledger, policy) = seed.build(T0).unwrap();

    let fake = Fake::default();
    let pp = serve(paypal(fake.clone())).await;
    let model_url = format!(
        "{}/v1/chat/completions",
        serve(Router::new().route("/v1/chat/completions", post(model))).await
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut tokens = AgentTokens::default();
    let mut slot_tokens = Vec::new();
    for i in 0..portcullis_demo::siege::SLOTS {
        let t = format!("pcx_inv_test_{i}");
        tokens = tokens.with_token(&format!("siege-{}", i + 1), &t);
        slot_tokens.push(t);
    }
    let settled = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gatehouse::new(
        ledger,
        policy,
        SettleRail {
            settled: settled.clone(),
        },
    ));
    let chain: SharedAudit = Default::default();
    let hub = Hub::new();
    let to_hub = hub.clone();
    let clock: portcullis_mcp::Clock = Arc::new(|| T0);
    let audit = portcullis_mcp::chained_sink_with(chain.clone(), clock.clone(), move |r, n| {
        to_hub.record(r, n)
    });
    let invoicer = Invoicer::new(
        Endpoint::local(&pp).unwrap(),
        Credentials {
            client_id: "vendor".into(),
            client_secret: "s".into(),
        },
    );
    let models: Vec<String> = (1..=8).map(|i| format!("scripted/clerk-{i}")).collect();
    let siege = Siege::new(
        gate.clone(),
        hub.clone(),
        audit.clone(),
        clock.clone(),
        chain.clone(),
        SiegeConfig {
            scenario,
            models: models.clone(),
            openrouter_key: "unused".into(),
            model_url,
            mcp_url: format!("http://{addr}/mcp"),
            slot_tokens,
            daily: 5,
            per_visitor: 5,
            shift: Duration::from_secs(3),
            grace: Duration::from_secs(1),
            billing: Some(Billing {
                invoicer: Arc::new(invoicer),
                payer_email: Some(PAYER.into()),
            }),
        },
    );
    let server = Portcullis::new(gate.clone(), clock.clone(), audit.clone());
    let router = app(server, tokens, vec!["127.0.0.1".into()]).merge(siege::routes(siege));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    // The executor, as main.rs runs it.
    let (driver, log) = (gate.clone(), audit.clone());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            for ev in driver.drive(T0).await {
                log(json!({ "kind": "execution", "at": T0, "event": ev }));
            }
        }
    });

    let http = reqwest::Client::new();
    let r = http
        .post(format!("http://{addr}/siege/start"))
        .json(&json!({ "roster": models }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202, "{}", r.text().await.unwrap());
    let started: Value = r.json().await.unwrap();
    assert_eq!(
        started["invoice"], "INV-30007",
        "PayPal's number is the bill's"
    );

    // Issued: the number PayPal suggested, to the payer, the bill's lines.
    {
        let c = fake.created.lock().unwrap();
        assert_eq!(c.len(), 1);
        let b = &c[0];
        assert_eq!(b["detail"]["invoice_number"], "INV-30007");
        assert_eq!(
            b["primary_recipients"][0]["billing_info"]["email_address"],
            PAYER
        );
        assert_eq!(b["items"].as_array().unwrap().len(), 3);
    }
    assert_eq!(*fake.sent.lock().unwrap(), 1);

    // Clerk 1 pays it; the payout is in flight. Nothing is recorded yet.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        fake.payments.lock().unwrap().is_empty(),
        "recorded before the money left"
    );

    // The money leaves; the vendor records it, once.
    settled.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        while fake.payments.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("payment never recorded");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let p = fake.payments.lock().unwrap().clone();
    assert_eq!(p.len(), 1, "recorded exactly once: {p:?}");
    assert_eq!(p[0]["amount"]["value"], "1284.60");
    assert_eq!(p[0]["payment_date"], "2026-11-04");
    assert!(p[0]["note"].as_str().unwrap().contains(BATCH));

    let st: Value = http
        .get(format!("http://{addr}/siege/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["paypal"]["status"], "MARKED_AS_PAID");
    // What clerk 1 read was the invoice PayPal holds, link and all.
    let read = st["logs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "viewing")
        .cloned()
        .unwrap();
    let body = read["email"]["body"].as_str().unwrap();
    assert!(body.contains("INV-30007"));
    assert!(body.contains("https://www.sandbox.paypal.com/invoice/p/#INV2-TEST-AAAA-BBBB"));

    let c = chain.lock().unwrap();
    c.verify().unwrap();
    let mut invoice_actions = Vec::new();
    let mut approved = None;
    for r in c.records() {
        let b: Value = serde_json::from_str(&r.body).unwrap();
        if r.kind == "invoice" {
            invoice_actions.push(b["action"].as_str().unwrap().to_string());
        }
        if r.kind == "approval" && b["action"] == "approved" {
            approved = Some(b["commitment"]["id"].clone());
        }
    }
    assert_eq!(invoice_actions, ["issued", "paid"]);
    assert_eq!(approved, Some(json!("INV-30007")));
}
