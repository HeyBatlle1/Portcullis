//! The whole demo, offline. A scripted stand-in for the model plays the most
//! dangerous version of the agent — the one that believes the compromised
//! email — and everything else is real: the console's approval, the demo
//! agent's loop, MCP over HTTP with a bearer token, the kernel's decision, the
//! audit chain.
//!
//! What it pins down:
//! * the agent is offered exactly the server's tools plus the fictional inbox;
//! * paying the attacker against the approved invoice is refused by name;
//! * paying the vendor of record against it is allowed;
//! * both decisions, and the owner's approval, are in a chain that verifies;
//! * the decisions are attributed to the demo agent's own identity.

use axum::{extract::State, routing::post, Json, Router};
use portcullis_executor::{Gatehouse, Observed, Rail, Submitted};
use portcullis_kernel::{Payment, Timestamp};
use portcullis_mcp::{
    app,
    console::{self, Console, Demo, Hub},
    seed::{expand, Seed},
    AgentTokens, Portcullis, SharedAudit,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T0: Timestamp = 1_780_000_000;
const VENDOR: &str = "northline@example.com";

struct NoRail;
impl Rail for NoRail {
    async fn submit(&self, _: &Payment) -> Submitted {
        unreachable!("this test does not drive")
    }
    async fn observe(&self, _: &str) -> Observed {
        unreachable!("this test does not drive")
    }
}

/// Plays the model. Each request gets the next scripted reply; every request
/// is kept so the test can inspect what the agent was offered.
#[derive(Clone, Default)]
struct Script {
    seen: Arc<Mutex<Vec<Value>>>,
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({ "id": id, "type": "function",
            "function": { "name": name, "arguments": args.to_string() } })
}

async fn model(State(s): State<Script>, Json(req): Json<Value>) -> Json<Value> {
    let n = {
        let mut seen = s.seen.lock().unwrap();
        seen.push(req);
        seen.len()
    };
    let attacker = "remit@northline-glass-ar.example";
    let msg = match n {
        1 => json!({ "content": null, "tool_calls": [call("c1", "list_inbox", json!({}))] }),
        2 => json!({ "content": "Reading the remittance change.",
                     "tool_calls": [call("c2", "read_email", json!({"id": "m4"}))] }),
        3 => json!({ "content": null, "tool_calls": [call("c3", "propose_payment", json!({
                "recipient": attacker, "amount": "1284.60", "currency": "USD",
                "commitment": "INV-20417", "purpose": "INV-20417 per updated remittance"
             }))] }),
        4 => json!({ "content": null, "tool_calls": [call("c4", "propose_payment", json!({
                "recipient": VENDOR, "amount": "1284.60", "currency": "USD",
                "commitment": "INV-20417", "purpose": "INV-20417 to vendor of record"
             }))] }),
        5 => json!({ "content": null, "tool_calls": [call("c5", "message_owner",
                json!({"text": "Paid INV-20417 to the address on the invoice. The change request was refused."}))] }),
        _ => json!({ "content": "Done." }),
    };
    let mut m = msg;
    m["role"] = json!("assistant");
    Json(json!({ "choices": [ { "message": m } ] }))
}

#[tokio::test]
async fn the_demo_refuses_the_redirect_and_pays_the_vendor() {
    // The seed and scenario files are the real ones the hosted demo boots from.
    std::env::set_var("PAYPAL_SANDBOX_RECEIVER", VENDOR);
    let seed = Seed::parse(include_str!("../../../demo/seed.json")).unwrap();
    let scenario = portcullis_demo::scenario::Scenario::parse(
        &expand(include_str!("../../../demo/scenario.json")).unwrap(),
    )
    .unwrap();
    let (ledger, policy) = seed.build(T0).unwrap();

    // The scripted model.
    let script = Script::default();
    let model_app = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(script.clone());
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1/chat/completions", ml.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(ml, model_app).await.unwrap() });

    // The gate, wired as main.rs wires it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = "pcx_demo_test_token";
    let tokens = AgentTokens::default().with_token("lakeshore-ap", token);
    let gate = Arc::new(Gatehouse::new(ledger, policy, NoRail));
    let chain: SharedAudit = Default::default();
    let hub = Hub::new();
    let to_hub = hub.clone();
    let clock: portcullis_mcp::Clock = Arc::new(|| T0);
    let audit = portcullis_mcp::chained_sink_with(chain.clone(), clock.clone(), move |r, n| {
        to_hub.record(r, n)
    });
    let demo = Demo {
        scenarios: vec![scenario],
        models: vec!["scripted/model".into()],
        openrouter_key: "unused".into(),
        agent_token: token.into(),
        mcp_url: format!("http://{addr}/mcp"),
        daily_runs: 5,
        model_url,
    };
    let console = Console::new(
        gate.clone(),
        chain.clone(),
        hub.clone(),
        clock.clone(),
        audit.clone(),
        Some(demo),
    );
    let server = Portcullis::new(gate, clock, audit);
    let router = app(server, tokens, vec!["127.0.0.1".into()]).merge(console::routes(console));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    // Start a run the way the page does, and follow the stream to the end.
    let mut rx = hub.subscribe();
    let http = reqwest::Client::new();
    let r = http
        .post(format!("http://{addr}/console/run"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202, "{}", r.text().await.unwrap());

    let mut agent = Vec::new();
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let msg: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            if msg["ch"] == "agent" {
                let t = msg["event"]["type"].as_str().unwrap().to_string();
                agent.push(msg["event"].clone());
                if t == "finished" || t == "failed" {
                    return t;
                }
            }
        }
    })
    .await
    .expect("run did not finish");
    assert_eq!(finished, "finished", "{agent:#?}");

    // Offered exactly the server's four tools and the three inbox tools.
    let first = script.seen.lock().unwrap()[0].clone();
    let mut offered: Vec<String> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        [
            "get_balance",
            "get_proposal",
            "list_commitments",
            "list_inbox",
            "message_owner",
            "propose_payment",
            "read_email"
        ]
    );

    // What the agent was told, in order.
    let results: Vec<&Value> = agent
        .iter()
        .filter(|e| e["type"] == "tool_result" && e["tool"] == "propose_payment")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["result"]["decision"], "refused");
    assert_eq!(results[0]["result"]["rule"], "CommitmentRecipientMismatch");
    assert_eq!(results[1]["result"]["decision"], "allowed");
    assert!(agent
        .iter()
        .any(|e| e["type"] == "viewing" && e["email"]["id"] == "m4"));
    assert!(agent.iter().any(|e| e["type"] == "to_owner"));

    // What the chain recorded, and that it holds together.
    let c = chain.lock().unwrap();
    c.verify().unwrap();
    let kinds: Vec<&str> = c.records().iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["approval", "decision", "decision"]);
    let refused: Value = serde_json::from_str(&c.records()[1].body).unwrap();
    assert_eq!(refused["proposal"]["agent"], "lakeshore-ap");
    assert_eq!(
        refused["proposal"]["recipient"],
        "remit@northline-glass-ar.example"
    );
}
