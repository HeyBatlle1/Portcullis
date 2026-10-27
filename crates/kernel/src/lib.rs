//! # Portcullis kernel
//!
//! The deterministic half of an agent wallet. An agent proposes; this decides.
//!
//! Nothing in this crate talks to a language model, and no language model sees
//! what is in it. That is the entire security argument, and it is checkable in
//! two places: the dependency list in `Cargo.toml`, and the signature of
//! [`decide`] — a ledger, a policy, a proposal, and a timestamp, all passed in.
//! No I/O, no clock, no randomness, no hidden trait objects. The same four inputs
//! always produce the same decision, which is what makes an audit log worth
//! reading and what stops an agent from arguing its way to a different answer.
//!
//! ## The shape
//!
//! - [`Proposal`] — what an agent would like to happen. Carries no authority.
//!   Its `purpose` string is untrusted and is never read by [`decide`].
//! - [`Decision`] — [`Decision::Allow`] or [`Decision::Refuse`]. Two states. There
//!   is no human approval queue; a runtime override would reduce every guarantee
//!   here to whatever somebody clicked while distracted.
//! - [`Ledger`] — balance, commitments, settlement history. Enforces
//!   `available = balance - encumbered`, which is what makes an earmark real
//!   rather than decorative.
//! - [`Policy`] — standing recipients and a closed set of rule primitives. No
//!   scripting, so every refusal can name the [`RuleId`] that caused it.
//! - [`PolicyLog`] — append-only, hash-chained, signed. Policy is replayed from
//!   it, never edited in place. Grants of authority are timelocked; revocations
//!   are immediate.
//!
//! ## Where the human is
//!
//! Writing policy, funding the account, and revoking. Never while an agent waits.
//! Refusals are the feedback channel: each one names its rule and lands in the
//! log, and that log is the policy backlog a human reads afterwards.

#![forbid(unsafe_code)]

pub mod amendment;
pub mod decide;
pub mod ledger;
pub mod policy;
pub mod proposal;
pub mod types;

pub use amendment::{Amendment, Hash, LogError, PolicyLog, SignedAmendment, Verifier};
pub use decide::decide;
pub use ledger::{forecast, Commitment, CommitmentForecast, CommitmentState, Ledger, Settled};
pub use policy::{Policy, Rule, RuleId, StandingRecipient};
pub use proposal::{Adjudication, Authorization, Decision, Proposal};
pub use types::{
    AgentId, Amount, AssetId, CommitmentId, Duration, ProposalId, RecipientId, TimeWindow,
    Timestamp,
};
