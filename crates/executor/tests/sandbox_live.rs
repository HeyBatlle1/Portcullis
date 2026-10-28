//! One real round trip against the PayPal sandbox. Ignored by default — it
//! needs credentials and a network, and CI has neither.
//!
//! ```sh
//! set -a; source .env; set +a
//! cargo test -p portcullis-executor --test sandbox_live -- --ignored --nocapture
//! ```
//!
//! Needs `PAYPAL_ENV=sandbox`, `PAYPAL_CLIENT_ID`, `PAYPAL_CLIENT_SECRET`, and
//! `PAYPAL_SANDBOX_RECEIVER` — the email of a sandbox *personal* account.

use portcullis_executor::*;
use portcullis_kernel::*;

#[tokio::test]
#[ignore]
async fn one_dollar_through_the_whole_gate() {
    let creds = Credentials::from_env().expect("sandbox credentials");
    let receiver = std::env::var("PAYPAL_SANDBOX_RECEIVER").expect("PAYPAL_SANDBOX_RECEIVER");
    let to = RecipientId::parse(&receiver).expect("receiver parses");

    let now = paypal::unix_now();
    let policy = Policy {
        standing: vec![StandingRecipient {
            recipient: to.clone(),
            added_at: now,
            active_from: now,
        }],
        rules: vec![Rule::PerTransactionMax { minor: 500 }],
        version: 1,
        head: [0; 32],
    };
    let gate = Gatehouse::new(
        Ledger::new(AssetId::usd(), 10_000),
        policy,
        PayPal::new(Endpoint::sandbox(), creds),
    );

    // Unique per run: the id is the idempotency key.
    let id = format!("live-{now}");
    let p = Proposal {
        id: ProposalId::new(&id),
        agent: AgentId::new("sandbox-check"),
        recipient: receiver.clone(),
        amount: Amount::new(AssetId::usd(), 100),
        proposed_at: now,
        against: None,
        purpose: "sandbox round trip".into(),
    };
    let adj = gate.propose(&p, now).await;
    println!("decision: {:?}", adj.decision());
    assert!(adj.decision().is_allowed());

    for i in 0..12 {
        for ev in gate.drive(paypal::unix_now()).await {
            println!("[{i}] {ev:?}");
        }
        let ex = gate
            .with_state(|s| s.execution(&ProposalId::new(&id)).cloned())
            .await
            .unwrap();
        if matches!(
            ex,
            Execution::Done { .. }
                | Execution::Settled { .. }
                | Execution::Released { .. }
                | Execution::Unresolved { .. }
        ) {
            println!("final: {ex:?}");
            assert!(matches!(
                ex,
                Execution::Done { .. } | Execution::Settled { .. }
            ));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    panic!("no terminal state within a minute — check the sandbox dashboard");
}
