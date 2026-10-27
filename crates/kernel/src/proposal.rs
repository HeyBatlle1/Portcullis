//! What an agent may ask for, and the two things it can be told.

use crate::policy::RuleId;
use crate::types::{AgentId, Amount, CommitmentId, ProposalId, RecipientId, Timestamp};
use serde::{Deserialize, Serialize};

/// A request. It carries no authority and no signature, because it is not an
/// authorization — it is an agent saying what it would like to happen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub agent: AgentId,
    pub recipient: RecipientId,
    pub amount: Amount,
    pub proposed_at: Timestamp,

    /// The commitment the agent claims to be drawing against, if it names one.
    ///
    /// Naming one is an assertion. If it does not hold, the proposal is refused
    /// rather than quietly falling through to discretionary funds — an agent that
    /// says "this is the November invoice" and is wrong should hear about it.
    pub against: Option<CommitmentId>,

    /// The agent's own words about why.
    ///
    /// **Untrusted, and never read by [`crate::decide`].** It exists to be shown
    /// to a human and stored in the audit log. Nothing in this crate parses it,
    /// pattern-matches it, or lets it influence a decision. If it ever becomes an
    /// input, an attacker who controls the agent's context controls the kernel.
    pub purpose: String,
}

/// How a proposal was allowed. Determines what the executor records and which
/// balance the settlement draws from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authorization {
    /// Drew against a commitment the human scheduled. Consumes it.
    Commitment(CommitmentId),
    /// Drew against available funds under the standing rules.
    Discretionary,
}

/// The kernel's answer. Two states, on purpose.
///
/// There is no `Held`, no `Pending`, and no human approval queue. A human
/// override would reduce every guarantee in this crate to the strength of
/// whatever someone clicked at 11pm, and an approval UI becomes a rubber stamp
/// within a week of real volume. The human acts when writing policy, when funding,
/// and when revoking — never while an agent waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow { via: Authorization },
    Refuse { rule: RuleId },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow { .. })
    }

    pub fn refused_by(&self) -> Option<RuleId> {
        match self {
            Decision::Refuse { rule } => Some(*rule),
            Decision::Allow { .. } => None,
        }
    }
}

/// A decision bound to the inputs that produced it.
///
/// The executor will not move money without one of these, and will not accept one
/// whose `policy_version` is not the policy it currently holds. That is what stops
/// a decision made under an old, looser policy from being replayed later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adjudication {
    pub proposal: ProposalId,
    pub decision: Decision,
    pub policy_version: u64,
    pub decided_at: Timestamp,
}
