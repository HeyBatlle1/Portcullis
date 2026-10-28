//! The executor's claims, against a scripted rail. The property that matters
//! most: money is released only when the rail has definitively said nothing
//! was created. Everything ambiguous holds.

use portcullis_executor::*;
use portcullis_kernel::*;
use std::collections::VecDeque;
use std::sync::Mutex;

const T0: Timestamp = 1_780_000_000;
const VENDOR: &str = "vendor@example.com";

#[derive(Default)]
struct Script {
    submits: VecDeque<Submitted>,
    observes: VecDeque<Observed>,
    sent: Vec<ProposalId>,
}

#[derive(Default)]
struct FakeRail(Mutex<Script>);

impl FakeRail {
    fn on_submit(self, s: Submitted) -> Self {
        self.0.lock().unwrap().submits.push_back(s);
        self
    }
    fn on_observe(self, o: ItemStatus) -> Self {
        self.0
            .lock()
            .unwrap()
            .observes
            .push_back(Observed::Status(o));
        self
    }
}

impl Rail for FakeRail {
    async fn submit(&self, p: &Payment) -> Submitted {
        let mut s = self.0.lock().unwrap();
        s.sent.push(p.proposal.clone());
        s.submits.pop_front().expect("unexpected submit")
    }
    async fn observe(&self, _: &str) -> Observed {
        let mut s = self.0.lock().unwrap();
        s.observes.pop_front().unwrap_or(Observed::Unknown {
            reason: "script exhausted".into(),
        })
    }
}

fn accepted() -> Submitted {
    Submitted::Accepted {
        batch_id: "BATCH1".into(),
    }
}
fn unknown() -> Submitted {
    Submitted::Unknown {
        reason: "timeout".into(),
    }
}

fn policy() -> Policy {
    Policy {
        standing: vec![StandingRecipient {
            recipient: RecipientId::parse(VENDOR).unwrap(),
            added_at: T0,
            active_from: T0,
        }],
        rules: vec![],
        version: 1,
        head: [1u8; 32],
    }
}

fn proposal(id: &str, to: &str, minor: u64) -> Proposal {
    Proposal {
        id: ProposalId::new(id),
        agent: AgentId::new("agent-1"),
        recipient: to.into(),
        amount: Amount::new(AssetId::usd(), minor),
        proposed_at: T0,
        against: None,
        purpose: String::new(),
    }
}

fn gate(rail: FakeRail) -> Gatehouse<FakeRail> {
    Gatehouse::new(Ledger::new(AssetId::usd(), 10_000), policy(), rail)
}

fn sent(g: &Gatehouse<FakeRail>) -> Vec<ProposalId> {
    g.rail().0.lock().unwrap().sent.clone()
}

async fn exec(g: &Gatehouse<FakeRail>, id: &str) -> Execution {
    g.with_state(|s| s.execution(&ProposalId::new(id)).cloned())
        .await
        .expect("execution exists")
}

async fn money(g: &Gatehouse<FakeRail>) -> (u64, u64) {
    g.with_state(|s| (s.ledger.balance, s.ledger.reserved_total()))
        .await
}

// ── the happy path ──────────────────────────────────────────────────────────

#[tokio::test]
async fn allowed_payment_is_sent_once_and_settles() {
    let g = gate(
        FakeRail::default()
            .on_submit(accepted())
            .on_observe(ItemStatus::Success),
    );
    assert!(g
        .propose(&proposal("p1", VENDOR, 4_200), T0)
        .await
        .decision()
        .is_allowed());
    assert_eq!(money(&g).await, (10_000, 4_200));

    g.drive(T0 + 1).await; // submit
    assert_eq!(
        exec(&g, "p1").await,
        Execution::InFlight {
            batch_id: "BATCH1".into()
        }
    );

    g.drive(T0 + 60).await; // observe SUCCESS
    assert_eq!(
        exec(&g, "p1").await,
        Execution::Done {
            batch_id: "BATCH1".into()
        }
    );
    assert_eq!(money(&g).await, (5_800, 0));
}

#[tokio::test]
async fn a_refusal_never_reaches_the_rail() {
    let g = gate(FakeRail::default()); // any submit would panic
    let adj = g
        .propose(&proposal("p1", "stranger@example.com", 100), T0)
        .await;
    assert!(!adj.decision().is_allowed());
    assert!(g.drive(T0 + 1).await.is_empty());
}

// ── definitive failure releases; ambiguity holds ────────────────────────────

#[tokio::test]
async fn a_definitive_rejection_releases_the_money() {
    let g = gate(FakeRail::default().on_submit(Submitted::Rejected {
        reason: "400 VALIDATION_ERROR".into(),
    }));
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    assert!(matches!(exec(&g, "p1").await, Execution::Released { .. }));
    assert_eq!(money(&g).await, (10_000, 0));
}

#[tokio::test]
async fn an_unknown_outcome_holds_the_money_and_retries_with_the_same_key() {
    let rail = FakeRail::default()
        .on_submit(unknown())
        .on_submit(accepted());
    let g = gate(rail);
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;

    g.drive(T0 + 1).await;
    assert_eq!(exec(&g, "p1").await, Execution::Pending { attempts: 1 });
    assert_eq!(
        money(&g).await,
        (10_000, 4_200),
        "reservation must survive a timeout"
    );

    g.drive(T0 + 30).await;
    assert_eq!(
        exec(&g, "p1").await,
        Execution::InFlight {
            batch_id: "BATCH1".into()
        }
    );
    // Both attempts carried the same idempotency key.
    assert_eq!(sent(&g), vec![ProposalId::new("p1"), ProposalId::new("p1")]);
}

#[tokio::test]
async fn unknown_then_rejected_is_unresolved_not_released() {
    // The retry created nothing. That says nothing about the first attempt.
    let g = gate(
        FakeRail::default()
            .on_submit(unknown())
            .on_submit(Submitted::Rejected {
                reason: "400".into(),
            }),
    );
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    g.drive(T0 + 2).await;
    assert!(matches!(exec(&g, "p1").await, Execution::Unresolved { .. }));
    assert_eq!(money(&g).await, (10_000, 4_200));
}

#[tokio::test]
async fn persistent_ambiguity_goes_to_a_human_with_the_money_held() {
    let mut rail = FakeRail::default();
    for _ in 0..MAX_ATTEMPTS {
        rail = rail.on_submit(unknown());
    }
    let g = gate(rail);
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    for i in 0..MAX_ATTEMPTS {
        g.drive(T0 + i as u64).await;
    }
    assert!(matches!(exec(&g, "p1").await, Execution::Unresolved { .. }));
    assert_eq!(money(&g).await, (10_000, 4_200));
    // And it stops asking.
    assert!(g.drive(T0 + 100).await.is_empty());
}

// ── policy changes between decision and send ────────────────────────────────

fn tightened() -> Policy {
    let mut p = policy();
    p.standing.clear();
    p.version = 2;
    p.head = [2u8; 32];
    p
}

#[tokio::test]
async fn a_policy_change_before_any_send_releases_without_sending() {
    let g = gate(FakeRail::default()); // any submit would panic
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.set_policy(tightened()).await;

    let ev = g.drive(T0 + 1).await;
    assert!(matches!(ev[..], [Event::Released { .. }]));
    assert_eq!(money(&g).await, (10_000, 0));
}

#[tokio::test]
async fn a_policy_change_after_an_unknown_send_neither_resends_nor_releases() {
    // Resending could create the payment under a policy that now forbids it.
    // Releasing could free money that already left. The only safe state is held.
    let g = gate(FakeRail::default().on_submit(unknown())); // a second submit would panic
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    g.set_policy(tightened()).await;

    g.drive(T0 + 2).await;
    assert!(matches!(exec(&g, "p1").await, Execution::Unresolved { .. }));
    assert_eq!(money(&g).await, (10_000, 4_200));
}

// ── what the rail reports afterwards ────────────────────────────────────────

#[tokio::test]
async fn a_failed_payout_releases() {
    let g = gate(
        FakeRail::default()
            .on_submit(accepted())
            .on_observe(ItemStatus::Failed),
    );
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    g.drive(T0 + 2).await;
    assert!(matches!(exec(&g, "p1").await, Execution::Released { .. }));
    assert_eq!(money(&g).await, (10_000, 0));
}

#[tokio::test]
async fn an_unclaimed_payout_settles_then_credits_back_when_returned() {
    let g = gate(
        FakeRail::default()
            .on_submit(accepted())
            .on_observe(ItemStatus::Unclaimed)
            .on_observe(ItemStatus::Returned),
    );
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    g.drive(T0 + 2).await;
    assert_eq!(
        exec(&g, "p1").await,
        Execution::Settled {
            batch_id: "BATCH1".into()
        }
    );
    assert_eq!(money(&g).await, (5_800, 0));

    let ev = g.drive(T0 + 30 * 86_400).await;
    assert!(matches!(ev[..], [Event::CreditedBack { minor: 4_200, .. }]));
    assert_eq!(money(&g).await, (10_000, 0));
    assert_eq!(
        exec(&g, "p1").await,
        Execution::Done {
            batch_id: "BATCH1".into()
        }
    );
}

#[tokio::test]
async fn an_unreadable_status_changes_nothing() {
    let g = gate(FakeRail::default().on_submit(accepted())); // observe → Unknown
    g.propose(&proposal("p1", VENDOR, 4_200), T0).await;
    g.drive(T0 + 1).await;
    assert!(g.drive(T0 + 2).await.is_empty());
    assert_eq!(
        exec(&g, "p1").await,
        Execution::InFlight {
            batch_id: "BATCH1".into()
        }
    );
    assert_eq!(money(&g).await, (10_000, 4_200));
}
