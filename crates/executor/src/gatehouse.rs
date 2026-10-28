//! The gatehouse: the one place the ledger lives, and the loop that turns
//! adjudications into payments.
//!
//! Everything that mutates the ledger takes one lock. No lock is held across a
//! network call — the pattern is lock, decide what to send, unlock, talk to the
//! rail, lock, record what happened. The kernel's `executable()` is checked
//! under the lock immediately before every send.
//!
//! The rule the whole loop is built around: **only a definitive answer from the
//! rail releases money.** A timeout, a 5xx, a body we can't read — the money
//! may already have moved, so the reservation stays, and we ask again with the
//! same idempotency key. When we can no longer safely ask again, the payment is
//! marked unresolved and held for a human. Holding money we didn't need to is
//! an inconvenience. Releasing money that already left is a double-spend.

use crate::rail::{Observed, Rail, Step, Submitted};
use portcullis_kernel::{
    adjudicate, Adjudication, Ledger, NotExecutable, Policy, Proposal, ProposalId, Timestamp,
};
use std::collections::BTreeMap;
use tokio::sync::Mutex;

/// Attempts at an ambiguous submission before handing it to a human.
pub const MAX_ATTEMPTS: u32 = 5;

/// Where one allowed payment is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Execution {
    /// Not yet confirmed to exist at the rail. `attempts` counts sends whose
    /// outcome was unknown; while it is zero, nothing has ever been sent.
    Pending { attempts: u32 },
    /// The rail holds it; the money has not visibly left.
    InFlight { batch_id: String },
    /// The money left our balance. Still watched, because an unclaimed payout
    /// can come back.
    Settled { batch_id: String },
    /// Nothing more will happen.
    Done { batch_id: String },
    /// Never paid; reservation released.
    Released { reason: String },
    /// We cannot establish what happened and can no longer safely find out.
    /// The reservation is held. A human reconciles against the rail.
    Unresolved { reason: String },
}

/// Something worth writing to the audit log or showing on the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Submitted {
        proposal: ProposalId,
        batch_id: String,
    },
    Retrying {
        proposal: ProposalId,
        attempt: u32,
        reason: String,
    },
    Settled {
        proposal: ProposalId,
        batch_id: String,
    },
    Released {
        proposal: ProposalId,
        reason: String,
    },
    CreditedBack {
        proposal: ProposalId,
        minor: u64,
    },
    Unresolved {
        proposal: ProposalId,
        reason: String,
    },
    Anomaly {
        proposal: ProposalId,
        detail: String,
    },
}

pub struct State {
    pub ledger: Ledger,
    pub policy: Policy,
    adjudications: BTreeMap<ProposalId, Adjudication>,
    executions: BTreeMap<ProposalId, Execution>,
}

impl State {
    pub fn execution(&self, id: &ProposalId) -> Option<&Execution> {
        self.executions.get(id)
    }
    pub fn executions(&self) -> impl Iterator<Item = (&ProposalId, &Execution)> {
        self.executions.iter()
    }
}

pub struct Gatehouse<R> {
    state: Mutex<State>,
    rail: R,
}

enum Work {
    Submit(portcullis_kernel::Payment, u32),
    Observe(ProposalId, String, bool),
}

impl<R: Rail> Gatehouse<R> {
    pub fn new(ledger: Ledger, policy: Policy, rail: R) -> Self {
        Gatehouse {
            state: Mutex::new(State {
                ledger,
                policy,
                adjudications: BTreeMap::new(),
                executions: BTreeMap::new(),
            }),
            rail,
        }
    }

    /// Decide a proposal and, if allowed, queue it. The decision is final the
    /// moment this returns; whether the money moves is the executor's problem.
    pub async fn propose(&self, proposal: &Proposal, now: Timestamp) -> Adjudication {
        let mut s = self.state.lock().await;
        let State {
            ledger,
            policy,
            adjudications,
            executions,
        } = &mut *s;
        let adj = adjudicate(ledger, policy, proposal, now);
        if adj.decision().is_allowed() {
            adjudications.insert(proposal.id.clone(), adj.clone());
            executions.insert(proposal.id.clone(), Execution::Pending { attempts: 0 });
        }
        adj
    }

    /// Install a newly replayed policy. Pending payments decided under the old
    /// one will refuse to send on the next pass.
    pub async fn set_policy(&self, policy: Policy) {
        self.state.lock().await.policy = policy;
    }

    /// The rail, for inspection. Read-only.
    pub fn rail(&self) -> &R {
        &self.rail
    }

    pub async fn with_state<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&*self.state.lock().await)
    }

    /// One pass over every live payment: send what needs sending, ask about
    /// what is in flight, record what the rail says. Call it on a timer.
    pub async fn drive(&self, now: Timestamp) -> Vec<Event> {
        let mut events = Vec::new();

        // ── 1. Under the lock: decide what to do, without doing it. ──
        let work: Vec<Work> = {
            let mut s = self.state.lock().await;
            let mut work = Vec::new();
            let ids: Vec<ProposalId> = s.executions.keys().cloned().collect();
            for id in ids {
                match s.executions[&id].clone() {
                    Execution::Pending { attempts } => {
                        let check = s.adjudications[&id]
                            .executable(&s.policy, &s.ledger)
                            .cloned();
                        match check {
                            Ok(payment) => work.push(Work::Submit(payment, attempts)),
                            Err(why) => {
                                let ev = self::refuse_to_send(&mut s, &id, attempts, why);
                                events.push(ev);
                            }
                        }
                    }
                    Execution::InFlight { batch_id } => {
                        work.push(Work::Observe(id, batch_id, false))
                    }
                    Execution::Settled { batch_id } => work.push(Work::Observe(id, batch_id, true)),
                    Execution::Done { .. }
                    | Execution::Released { .. }
                    | Execution::Unresolved { .. } => {}
                }
            }
            work
        };

        // ── 2. Unlocked: talk to the rail. ──
        for w in work {
            match w {
                Work::Submit(payment, attempts) => {
                    let outcome = self.rail.submit(&payment).await;
                    let mut s = self.state.lock().await;
                    events.push(record_submit(&mut s, &payment.proposal, attempts, outcome));
                }
                Work::Observe(id, batch_id, settled) => {
                    let Observed::Status(status) = self.rail.observe(&batch_id).await else {
                        continue; // learned nothing; change nothing
                    };
                    let mut s = self.state.lock().await;
                    if let Some(ev) = apply_step(
                        &mut s,
                        &id,
                        &batch_id,
                        crate::rail::step(status, settled),
                        now,
                    ) {
                        events.push(ev);
                    }
                }
            }
        }
        events
    }
}

/// The kernel says don't send. What that means depends on whether we ever have.
fn refuse_to_send(s: &mut State, id: &ProposalId, attempts: u32, why: NotExecutable) -> Event {
    if attempts == 0 {
        // Never sent: nothing exists at the rail. Releasing is exact.
        let reason = format!("not executable: {why:?}");
        let _ = s.ledger.release(id);
        s.executions.insert(
            id.clone(),
            Execution::Released {
                reason: reason.clone(),
            },
        );
        Event::Released {
            proposal: id.clone(),
            reason,
        }
    } else {
        // An earlier send's outcome is unknown. Sending again might *create*
        // the payment under a policy that no longer allows it; releasing might
        // free money that already left. Neither. Hold it for a human.
        let reason = format!("unknown outcome after {attempts} attempt(s), then {why:?}");
        s.executions.insert(
            id.clone(),
            Execution::Unresolved {
                reason: reason.clone(),
            },
        );
        Event::Unresolved {
            proposal: id.clone(),
            reason,
        }
    }
}

fn record_submit(s: &mut State, id: &ProposalId, attempts: u32, outcome: Submitted) -> Event {
    match outcome {
        Submitted::Accepted { batch_id } => {
            s.executions.insert(
                id.clone(),
                Execution::InFlight {
                    batch_id: batch_id.clone(),
                },
            );
            Event::Submitted {
                proposal: id.clone(),
                batch_id,
            }
        }
        Submitted::Rejected { reason } if attempts == 0 => {
            let _ = s.ledger.release(id);
            s.executions.insert(
                id.clone(),
                Execution::Released {
                    reason: reason.clone(),
                },
            );
            Event::Released {
                proposal: id.clone(),
                reason,
            }
        }
        // A rejection *after* an unknown attempt proves only that this retry
        // created nothing — not that the earlier one didn't.
        Submitted::Rejected { reason } => {
            let reason = format!("rejected after {attempts} unknown attempt(s): {reason}");
            s.executions.insert(
                id.clone(),
                Execution::Unresolved {
                    reason: reason.clone(),
                },
            );
            Event::Unresolved {
                proposal: id.clone(),
                reason,
            }
        }
        Submitted::Unknown { reason } => {
            let attempt = attempts + 1;
            if attempt >= MAX_ATTEMPTS {
                let reason = format!("{attempt} attempts, outcome still unknown: {reason}");
                s.executions.insert(
                    id.clone(),
                    Execution::Unresolved {
                        reason: reason.clone(),
                    },
                );
                Event::Unresolved {
                    proposal: id.clone(),
                    reason,
                }
            } else {
                s.executions
                    .insert(id.clone(), Execution::Pending { attempts: attempt });
                Event::Retrying {
                    proposal: id.clone(),
                    attempt,
                    reason,
                }
            }
        }
    }
}

fn apply_step(
    s: &mut State,
    id: &ProposalId,
    batch_id: &str,
    step: Step,
    now: Timestamp,
) -> Option<Event> {
    let b = batch_id.to_string();
    match step {
        Step::Wait => None,
        Step::Settle { final_ } => match s.ledger.settle(id, now) {
            Ok(_) => {
                let next = if final_ {
                    Execution::Done {
                        batch_id: b.clone(),
                    }
                } else {
                    Execution::Settled {
                        batch_id: b.clone(),
                    }
                };
                s.executions.insert(id.clone(), next);
                Some(Event::Settled {
                    proposal: id.clone(),
                    batch_id: b,
                })
            }
            Err(e) => Some(Event::Anomaly {
                proposal: id.clone(),
                detail: format!("settle refused by ledger: {e:?}"),
            }),
        },
        Step::Release => {
            let _ = s.ledger.release(id);
            let reason = format!("rail reported the payout did not complete ({b})");
            s.executions.insert(
                id.clone(),
                Execution::Released {
                    reason: reason.clone(),
                },
            );
            Some(Event::Released {
                proposal: id.clone(),
                reason,
            })
        }
        Step::CreditBack => {
            let minor = s
                .ledger
                .settled
                .iter()
                .find(|x| &x.proposal == id)
                .map(|x| x.minor)
                .unwrap_or(0);
            s.ledger.credit(minor);
            s.executions
                .insert(id.clone(), Execution::Done { batch_id: b });
            Some(Event::CreditedBack {
                proposal: id.clone(),
                minor,
            })
        }
        Step::Done => {
            s.executions
                .insert(id.clone(), Execution::Done { batch_id: b });
            None
        }
        Step::Anomaly => Some(Event::Anomaly {
            proposal: id.clone(),
            detail: format!("impossible transition reported for {b}"),
        }),
    }
}
