//! PayPal's webhook is a doorbell: it wakes the settlement loop and carries
//! nothing the loop believes.

use portcullis_mcp::{doorbell, edge};
use std::time::Duration;

async fn serve() -> (String, std::sync::Arc<doorbell::Doorbell>) {
    let bell = doorbell::Doorbell::new();
    let app = doorbell::routes(bell.clone()).layer(axum::middleware::from_fn(edge::harden));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), bell)
}

async fn rang(bell: &doorbell::Doorbell) -> bool {
    tokio::time::timeout(Duration::from_millis(200), bell.rung())
        .await
        .is_ok()
}

#[tokio::test]
async fn a_webhook_rings_once_however_many_arrive_and_its_body_is_never_read() {
    let (base, bell) = serve().await;
    assert!(!rang(&bell).await, "quiet until someone knocks");

    let c = reqwest::Client::new();
    // What PayPal sends, what a stranger sends, and nothing at all: the same
    // answer, because none of it is read.
    for body in [
        r#"{"event_type":"PAYMENT.PAYOUTS-ITEM.SUCCEEDED","resource":{"payout_item_id":"X"}}"#,
        r#"{"event_type":"PAYMENT.PAYOUTS-ITEM.SUCCEEDED","resource":{"transaction_status":"SUCCESS","payout_batch_id":"FORGED"}}"#,
        "not json at all",
        "",
    ] {
        let r = c
            .post(format!("{base}{}", doorbell::PATH))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    assert!(rang(&bell).await, "the loop is woken");
    assert!(!rang(&bell).await, "four knocks were one ring");
}

#[tokio::test]
async fn only_a_post_rings() {
    let (base, bell) = serve().await;
    let r = reqwest::get(format!("{base}{}", doorbell::PATH))
        .await
        .unwrap();
    assert_eq!(r.status(), 405);
    assert!(!rang(&bell).await);
}
