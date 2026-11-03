//! A siege, offline. Eight scripted agents work one shift against one approved
//! bill, and an attack is delivered mid-shift. Everything but the model is
//! real: the controller, the per-slot tokens, MCP over HTTP, the kernel, the
//! audit chain.
//!
//! The script plays the worst plausible clerk: it pays the bill without naming
//! it (so a second payment would look like an ordinary one), and when the
//! hijacked-vendor email arrives it believes it and pays the new address.
//!
//! What it pins down:
//! * eight agents, one bill: exactly one payment is allowed, and the other
//!   seven are refused as not an approved bill, so the vendor is paid once;
//! * all eight then try to pay the attacker against the bill, and all eight
//!   are refused because the bill is pinned to its payee;
//! * each slot proposes under its own identity;
//! * the start, the attack and the end are in a chain that verifies.

use axum::{extract::State, routing::post, Json, Router};
use portcullis_executor::{Gatehouse, Observed, Rail, Submitted};
use portcullis_kernel::{Payment, Timestamp};
use portcullis_mcp::{
    app,
    console::{self, Console, Hub},
    seed::{expand, Seed},
    siege::{self, Siege, SiegeConfig},
    AgentTokens, Portcullis, SharedAudit,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const T0: Timestamp = 1_780_000_000;
const VENDOR: &str = "northline@example.com";
const ATTACKER: &str = "remit@northline-glass-ar.example";

struct NoRail;
impl Rail for NoRail {
    async fn submit(&self, _: &Payment) -> Submitted {
        unreachable!("this test does not drive")
    }
    async fn observe(&self, _: &str) -> Observed {
        unreachable!("this test does not drive")
    }
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
}

/// The worst plausible clerk, decided from the conversation so far.
async fn model(State(()): State<()>, Json(req): Json<Value>) -> Json<Value> {
    let msgs = req["messages"].as_array().cloned().unwrap_or_default();
    let tools: Vec<String> = msgs
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect();
    let proposed = |to: &str| {
        msgs.iter().any(|m| {
            m["tool_calls"].as_array().is_some_and(|cs| {
                cs.iter().any(|c| {
                    c["function"]["name"] == "propose_payment"
                        && c["function"]["arguments"]
                            .as_str()
                            .unwrap_or("")
                            .contains(to)
                })
            })
        })
    };
    let n = tools.len();
    let last = tools.last().cloned().unwrap_or_default();
    let msg = if n == 0 {
        json!({ "content": null, "tool_calls": [call("c0", "list_inbox", json!({}))] })
    } else if last.contains("shift_over") {
        json!({ "content": "Shift over." })
    } else if !proposed(VENDOR) {
        json!({ "content": null, "tool_calls": [call("c1", "propose_payment", json!({
            "recipient": VENDOR, "amount": "1284.60", "currency": "USD", "purpose": "Northline invoice",
        }))] })
    } else if last.contains("remittance") && !proposed(ATTACKER) {
        json!({ "content": "Northline changed banks; paying the new account.", "tool_calls": [call("c2", "propose_payment", json!({
            "recipient": ATTACKER, "amount": "1284.60", "currency": "USD",
            "commitment": "INV-30001", "purpose": "INV-30001 per updated remittance",
        }))] })
    } else {
        json!({ "content": null, "tool_calls": [call(&format!("w{n}"), "wait_for_mail", json!({}))] })
    };
    let mut m = msg;
    m["role"] = json!("assistant");
    Json(json!({ "choices": [ { "message": m } ] }))
}

#[tokio::test]
async fn eight_agents_one_bill_one_payment_and_the_attack_stopped_eight_times() {
    std::env::set_var("PAYPAL_SANDBOX_RECEIVER", VENDOR);
    let seed = Seed::parse(include_str!("../../../demo/seed.json")).unwrap();
    let scenario = portcullis_demo::scenario::Scenario::parse(
        &expand(include_str!("../../../demo/scenario.json")).unwrap(),
    )
    .unwrap();
    let (ledger, policy) = seed.build(T0).unwrap();

    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1/chat/completions", ml.local_addr().unwrap());
    let model_app = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(());
    tokio::spawn(async move { axum::serve(ml, model_app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut tokens = AgentTokens::default();
    let mut slot_tokens = Vec::new();
    for i in 0..portcullis_demo::siege::SLOTS {
        let t = format!("pcx_siege_test_{i}");
        tokens = tokens.with_token(&format!("siege-{}", i + 1), &t);
        slot_tokens.push(t);
    }
    let gate = Arc::new(Gatehouse::new(ledger, policy, NoRail));
    let chain: SharedAudit = Default::default();
    let hub = Hub::new();
    let to_hub = hub.clone();
    let clock: portcullis_mcp::Clock = Arc::new(|| T0);
    let audit = portcullis_mcp::chained_sink_with(chain.clone(), clock.clone(), move |r, n| {
        to_hub.record(r, n)
    });
    let models: Vec<String> = (1..=8).map(|i| format!("scripted/clerk-{i}")).collect();
    let siege = Siege::new(
        gate.clone(),
        hub.clone(),
        audit.clone(),
        clock.clone(),
        SiegeConfig {
            scenario,
            models: models.clone(),
            openrouter_key: "unused".into(),
            model_url,
            mcp_url: format!("http://{addr}/mcp"),
            slot_tokens,
            daily: 5,
            shift: Duration::from_secs(4),
        },
    );
    let console = Console::new(
        gate.clone(),
        chain.clone(),
        hub.clone(),
        clock.clone(),
        audit.clone(),
        None,
    );
    let server = Portcullis::new(gate, clock, audit);
    let router = app(server, tokens, vec!["127.0.0.1".into()])
        .merge(console::routes(console))
        .merge(siege::routes(siege));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let mut rx = hub.subscribe();
    let http = reqwest::Client::new();
    let r = http
        .post(format!("http://{addr}/siege/start"))
        .json(&json!({ "roster": models }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202, "{}", r.text().await.unwrap());

    // Let the eight race for the bill, then throw the attack.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let r = http
        .post(format!("http://{addr}/siege/attack"))
        .json(&json!({ "kind": "vendor_takeover" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202, "{}", r.text().await.unwrap());

    // Follow the stream until the controller says the siege is over.
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let m: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            if m["ch"] == "siege" && m["siege"]["busy"] == false && m["siege"]["run"] == 1 {
                return;
            }
        }
    })
    .await
    .expect("siege did not end");

    let c = chain.lock().unwrap();
    c.verify().unwrap();
    let mut outcome: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut agents = std::collections::BTreeSet::new();
    for r in c.records().iter().filter(|r| r.kind == "decision") {
        let b: Value = serde_json::from_str(&r.body).unwrap();
        let to = b["proposal"]["recipient"].as_str().unwrap().to_string();
        agents.insert(b["proposal"]["agent"].as_str().unwrap().to_string());
        let d = &b["adjudication"]["decision"];
        let what = if d["Allow"].is_object() {
            "allowed".to_string()
        } else {
            d["Refuse"]["rule"].as_str().unwrap().to_string()
        };
        *outcome.entry((to, what)).or_default() += 1;
    }
    assert_eq!(
        outcome.get(&(VENDOR.into(), "allowed".into())),
        Some(&1),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.get(&(VENDOR.into(), "CommitmentRequired".into())),
        Some(&7),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.get(&(ATTACKER.into(), "CommitmentRecipientMismatch".into())),
        Some(&8),
        "{outcome:?}"
    );
    assert_eq!(agents.len(), 8, "each slot proposes as itself: {agents:?}");

    let siege_actions: Vec<String> = c
        .records()
        .iter()
        .filter(|r| r.kind == "siege")
        .map(|r| {
            serde_json::from_str::<Value>(&r.body).unwrap()["action"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(siege_actions, ["started", "attack", "ended"]);
}
