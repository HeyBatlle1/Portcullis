//! The doormat answers scanners, counts them, keeps no IP, and stays bounded;
//! a real route is never trapped and a plain miss is a plain 404.

use axum::{routing::get, Router};
use portcullis_mcp::trap::{is_probe, routes, Trap, KEEP};
use serde_json::Value;
use std::sync::Arc;

async fn serve() -> String {
    let app = Router::new()
        .route("/console", get(|| async { "the console" }))
        .merge(routes(Trap::new(Arc::new(|| 1_794_400_000))));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

async fn get_text(url: &str) -> (u16, String) {
    let r = reqwest::get(url).await.unwrap();
    (r.status().as_u16(), r.text().await.unwrap())
}

#[tokio::test]
async fn scanners_are_answered_and_counted_and_nothing_else_is() {
    let base = serve().await;

    let (code, env) = get_text(&format!("{base}/.env")).await;
    assert_eq!(code, 200);
    assert!(env.starts_with("# We see you."));
    assert!(!env.contains("sk-"), "nothing that looks like a real key");

    let (code, body) = get_text(&format!("{base}/wp-login.php")).await;
    assert_eq!(code, 200);
    assert!(body.starts_with("We see you."));
    assert!(body.contains("/wp-login.php"));

    // A real route is never trapped; an ordinary miss is a 404 and not counted.
    assert_eq!(
        get_text(&format!("{base}/console")).await,
        (200, "the console".into())
    );
    assert_eq!(get_text(&format!("{base}/no-such-page")).await.0, 404);

    let caught: Value = reqwest::get(format!("{base}/caught"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caught["today"], 2);
    assert_eq!(caught["recent"][0]["path"], "/wp-login.php");
    assert_eq!(caught["recent"][1]["path"], "/.env");
    // What is kept: the path and the time. Nothing that identifies anyone.
    let keys: Vec<&str> = caught["recent"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["at", "path"]);
}

#[tokio::test]
async fn the_list_stays_bounded() {
    let base = serve().await;
    for i in 0..KEEP + 15 {
        get_text(&format!("{base}/admin/{i}")).await;
    }
    let caught: Value = reqwest::get(format!("{base}/caught"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caught["recent"].as_array().unwrap().len(), KEEP);
    assert_eq!(caught["today"], (KEEP + 15) as u64);
}

#[test]
fn what_counts_as_a_probe() {
    for p in [
        "/.env",
        "/.env.production",
        "/.git/config",
        "/wp-admin/install.php",
        "/phpmyadmin/",
        "/admin",
        "/actuator/env",
        "/backup.sql",
        "/static/../../etc/passwd",
        "/index.php",
    ] {
        assert!(is_probe(p), "{p}");
    }
    for p in [
        "/console",
        "/siege",
        "/audit",
        "/favicon.ico",
        "/caught",
        "/mcp",
        "/health",
    ] {
        assert!(!is_probe(p), "{p}");
    }
}
