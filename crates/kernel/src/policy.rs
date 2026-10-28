//! What the human declared, ahead of time, deliberately.
//!
//! Rules are a closed set of named primitives. There is no scripting, no
//! expression language, and no way to add a rule kind without changing this file.
//! That is deliberate: a refusal must always be able to name the rule that caused
//! it, and a script that returns `false` has no name.

use crate::types::{AssetId, Duration, Hash, RecipientId, Timestamp};
use serde::{Deserialize, Serialize};

/// Every reason the kernel can refuse. Closed, exhaustive, and stable enough to
/// print on a screen or key a help page from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleId {
    /// The recipient string is not a payable address: non-ASCII, whitespace, or
    /// no grammar accepts it. Checked before anything else.
    RecipientMalformed,
    /// A proposal with this id has already been reserved or settled.
    DuplicateProposal,
    /// The proposal's asset is not this ledger's asset. The kernel does not convert.
    AssetMismatch,
    /// The asset is not in the policy's allowed set.
    AssetNotAllowed,
    /// The recipient is on no standing list and in no open commitment.
    RecipientNotAllowed,
    /// The recipient is allowed, but its timelock has not elapsed.
    RecipientNotYetActive,
    /// Spending it would eat into funds already earmarked for a commitment.
    FundsEncumbered,
    /// Not enough money in the account at all.
    InsufficientFunds,
    /// More than the per-transaction ceiling.
    ExceedsPerTransaction,
    /// More than the rolling-window ceiling.
    ExceedsRollingWindow,
    /// More transactions in the window than allowed, regardless of amount.
    ExceedsVelocity,
    /// Settling would take the balance below its floor.
    BreachesBalanceFloor,
    /// Outside the allowed time of day.
    OutsideTimeWindow,
    /// A commitment was named that does not exist.
    CommitmentNotFound,
    /// A commitment was named, but it is consumed or expired.
    CommitmentNotOpen,
    /// A commitment was named, but its window does not contain `now`.
    CommitmentWindowClosed,
    /// A commitment was named, but for a different recipient.
    CommitmentRecipientMismatch,
    /// A commitment was named, but the proposal is larger than its earmark.
    CommitmentAmountExceeded,
}

impl RuleId {
    /// One line, in the interface's voice, for the human reading the refusal.
    pub fn reason(&self) -> &'static str {
        match self {
            RuleId::RecipientMalformed => "recipient is not a well-formed address",
            RuleId::DuplicateProposal => "this proposal was already decided and paid or in flight",
            RuleId::AssetMismatch => "wrong asset for this ledger",
            RuleId::AssetNotAllowed => "asset is not allowed by policy",
            RuleId::RecipientNotAllowed => "recipient is not in the allowlist",
            RuleId::RecipientNotYetActive => "recipient is still within its timelock",
            RuleId::FundsEncumbered => "funds are earmarked for a scheduled payment",
            RuleId::InsufficientFunds => "not enough funds in the account",
            RuleId::ExceedsPerTransaction => "over the per-transaction limit",
            RuleId::ExceedsRollingWindow => "over the rolling spend limit",
            RuleId::ExceedsVelocity => "too many payments in the window",
            RuleId::BreachesBalanceFloor => "would take the balance below its floor",
            RuleId::OutsideTimeWindow => "outside the allowed hours",
            RuleId::CommitmentNotFound => "no such commitment",
            RuleId::CommitmentNotOpen => "commitment is already consumed or expired",
            RuleId::CommitmentWindowClosed => "commitment window is not open",
            RuleId::CommitmentRecipientMismatch => "commitment names a different recipient",
            RuleId::CommitmentAmountExceeded => "over the amount this commitment earmarked",
        }
    }
}

/// A rule primitive. All rules in a policy are AND-composed: a proposal must
/// satisfy every one of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rule {
    /// No single payment above this, in minor units.
    PerTransactionMax { minor: u64 },
    /// No more than this total across a trailing window.
    RollingWindowMax { minor: u64, window: Duration },
    /// No more than this many payments across a trailing window.
    VelocityMax { count: u32, window: Duration },
    /// Never settle below this balance, in minor units.
    BalanceFloor { minor: u64 },
    /// Only these assets.
    AssetAllowlist { assets: Vec<AssetId> },
    /// Minutes from UTC midnight, half-open. `from == until` means always.
    TimeOfDay { from_minute: u32, until_minute: u32 },
}

/// A recipient the agent may pay repeatedly, with no commitment naming it.
///
/// This is the high-privilege grant in the system. Prefer commitment-scoped
/// recipients, which expire with the payment they exist for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingRecipient {
    pub recipient: RecipientId,
    /// When the amendment that added it was signed.
    pub added_at: Timestamp,
    /// When it becomes spendable. The gap is the timelock, and it exists so a
    /// recipient added by a compromised agent's social engineering sits visible
    /// in the console for a day before it can be used.
    pub active_from: Timestamp,
}

impl StandingRecipient {
    pub fn is_active(&self, now: Timestamp) -> bool {
        now >= self.active_from
    }
}

/// The complete set of standing grants and constraints.
///
/// A policy is never edited in place at runtime. It is rebuilt by replaying the
/// signed amendment log — see [`crate::amendment`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub standing: Vec<StandingRecipient>,
    pub rules: Vec<Rule>,
    /// How many amendments produced this policy. For humans; a short number to
    /// print beside a decision.
    pub version: u64,
    /// Digest of the last log entry replayed into this policy. This, not
    /// `version`, is what an [`crate::Adjudication`] binds to: two different logs
    /// can have the same length, but not the same head.
    ///
    /// Neither one captures time. A policy replayed later can differ from one
    /// replayed earlier at the same head, because a timelocked grant matured in
    /// between. That is safe by construction — only *widening* waits on a
    /// timelock, so a decision made before maturity is never looser than the
    /// policy in force after it.
    pub head: Hash,
}

impl Policy {
    pub fn standing_for(&self, recipient: &RecipientId) -> Option<&StandingRecipient> {
        self.standing.iter().find(|s| &s.recipient == recipient)
    }
}
