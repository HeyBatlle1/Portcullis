//! The audit desk's model proxy, through its real route, against a fake model.
//! What it promises: the page reaches only the models and the function tools
//! it was given, a request is bounded in size, and one visitor cannot spend
//! the day's calls.

use axum::{extract::State, routing::post, Json, Router};
use portcullis_mcp::studio::{routes, Studio, MAX_OUTPUT_TOKENS, MAX_REQUEST_BYTES};
use portcullis_mcp::visitor::Quota;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<Value>>>;

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

/// A model that records what reached it and answers `{}`.
async fn fake_model() -> (String, Seen) {
    let seen: Seen = Default::default();
    let app = Router::new()
        .route(
            "/responses",
            post(|State(s): State<Seen>, Json(b): Json<Value>| async move {
                s.lock().unwrap().push(b);
                Json(json!({}))
            }),
        )
        .with_state(seen.clone());
    (format!("{}/responses", serve(app).await), seen)
}

async fn desk(per_visitor: u32) -> (String, Seen) {
    let (upstream, seen) = fake_model().await;
    let studio = Arc::new(Studio {
        dir: "does-not-exist".into(),
        license_key: None,
        openrouter_key: Some("test-key".into()),
        models: vec!["allowed/model".into()],
        daily_calls: 100,
        http: reqwest::Client::new(),
        budget: Default::default(),
        per_visitor: Quota::new(per_visitor),
        upstream,
    });
    (serve(routes(studio)).await, seen)
}

async fn ask(base: &str, ip: &str, body: &Value) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/audit/ai/responses"))
        .header("cf-connecting-ip", ip)
        .json(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn only_the_desks_own_function_tools_reach_the_model() {
    let (base, seen) = desk(10).await;
    let body = json!({
        "model": "allowed/model",
        "input": "what happened in siege 3?",
        "max_output_tokens": 1_000_000,
        "models": ["expensive/model"],
        "plugins": [{ "id": "web" }],
        "web_search_options": { "search_context_size": "high" },
        "tools": [
            { "type": "function", "name": "execute_query", "parameters": {} },
            { "type": "web_search_preview" },
            { "type": "code_interpreter" },
        ],
    });
    assert_eq!(ask(&base, "1.1.1.1", &body).await, 200);

    let got = seen.lock().unwrap()[0].clone();
    assert_eq!(got["model"], "allowed/model");
    assert_eq!(got["max_output_tokens"], MAX_OUTPUT_TOKENS);
    for k in ["models", "plugins", "web_search_options"] {
        assert!(got.get(k).is_none(), "{k} reached the model");
    }
    let tools = got["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert_eq!(tools[0]["name"], "execute_query");
    assert_eq!(got["input"], "what happened in siege 3?");
}

/// The upstream request is rebuilt from an allowlist: what the desk sends
/// passes, and nothing else does, however it is dressed up.
#[tokio::test]
async fn only_conversation_text_and_modest_reasoning_reach_the_model() {
    let (base, seen) = desk(10).await;
    let body = json!({
        "model": "allowed/model",
        "instructions": "You are the auditor.",
        "input": [
            { "type": "message", "role": "user", "content": [
                { "type": "input_text", "text": "what was refused?" },
                { "type": "input_image", "image_url": "https://attacker.example/huge.png", "detail": "high" },
                { "type": "input_file", "file_url": "https://attacker.example/doc.pdf" },
            ] },
            { "type": "function_call", "call_id": "c1", "name": "execute_query", "arguments": "{}" },
            { "type": "function_call_output", "call_id": "c1", "output": "[]" },
            { "type": "input_video", "video_url": "https://attacker.example/v.mp4" },
        ],
        "reasoning": { "effort": "xhigh", "max_tokens": 100000 },
        "service_tier": "priority",
        "provider": { "order": ["expensive"] },
        "trace": { "x": 1 },
        "metadata": { "a": "b" },
        "user": "someone",
        "background": true,
        "store": true,
        "tool_choice": { "type": "web_search_preview" },
        "tools": { "type": "web_search_preview" },
        "text": { "format": { "type": "text" }, "verbosity": "high" },
    });
    assert_eq!(ask(&base, "1.1.1.1", &body).await, 200);
    let got = seen.lock().unwrap()[0].clone();
    let mut keys: Vec<&str> = got
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "input",
            "instructions",
            "max_output_tokens",
            "model",
            "reasoning",
            "store",
            "stream",
            "text"
        ]
    );
    assert_eq!(got["store"], false);
    assert_eq!(got["reasoning"], json!({ "effort": "medium" }));
    assert_eq!(got["text"], json!({ "format": { "type": "text" } }));
    let input = got["input"].as_array().unwrap();
    assert_eq!(input.len(), 3, "the video item is gone: {input:#?}");
    assert_eq!(
        input[0]["content"],
        json!([{ "type": "input_text", "text": "what was refused?" }])
    );
    assert!(!got.to_string().contains("attacker.example"));

    // A model not on the list never leaves.
    let other = json!({ "model": "expensive/model", "input": "hi" });
    assert_eq!(ask(&base, "1.1.1.1", &other).await, 400);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_oversized_request_is_refused_before_it_costs_anything() {
    let (base, seen) = desk(10).await;
    let big = json!({ "model": "allowed/model", "input": "x".repeat(MAX_REQUEST_BYTES) });
    assert_eq!(ask(&base, "1.1.1.1", &big).await, 413);
    assert!(seen.lock().unwrap().is_empty());

    let fits = json!({ "model": "allowed/model", "input": "x".repeat(MAX_REQUEST_BYTES / 2) });
    assert_eq!(ask(&base, "1.1.1.1", &fits).await, 200);
}

#[tokio::test]
async fn one_visitor_cannot_spend_the_days_calls() {
    let (base, seen) = desk(2).await;
    let q = json!({ "model": "allowed/model", "input": "hi" });
    assert_eq!(ask(&base, "1.1.1.1", &q).await, 200);
    assert_eq!(ask(&base, "1.1.1.1", &q).await, 200);
    assert_eq!(ask(&base, "1.1.1.1", &q).await, 429);
    // Someone else still gets theirs.
    assert_eq!(ask(&base, "2.2.2.2", &q).await, 200);
    assert_eq!(seen.lock().unwrap().len(), 3);
}
