//! What an agent may ask for, what it can be told, and the one object the
//! executor will act on.

use crate::ledger::Ledger;
use crate::policy::{Policy, RuleId};
use crate::types::{hex, AgentId, Amount, CommitmentId, Hash, ProposalId, RecipientId, Timestamp};
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// A request. It carries no authority and no signature, because it is not an
/// authorization — it is an agent saying what it would like to happen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub agent: AgentId,

    /// Where the agent would like the money to go, **as the agent wrote it**.
    ///
    /// Untrusted text, like `purpose`. The kernel parses it into a
    /// [`RecipientId`] and refuses with [`RuleId::RecipientMalformed`] if it
    /// cannot. If it parses, the canonical form is what gets compared and what
    /// gets paid — never this string.
    pub recipient: String,

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

impl Proposal {
    /// SHA-256 over a fixed, length-prefixed encoding of every field. Binds an
    /// adjudication to exactly what was proposed — including `purpose`, so the
    /// audit record of what the agent *said* cannot be swapped after the fact,
    /// even though it never influenced the decision.
    pub fn digest(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(b"portcullis.proposal.v1");
        let mut field = |b: &[u8]| {
            h.update((b.len() as u64).to_be_bytes());
            h.update(b);
        };
        field(self.id.as_str().as_bytes());
        field(self.agent.as_str().as_bytes());
        field(self.recipient.as_bytes());
        field(self.amount.asset.symbol.as_bytes());
        field(&[self.amount.asset.decimals]);
        field(&self.amount.minor.to_be_bytes());
        field(&self.proposed_at.to_be_bytes());
        match &self.against {
            None => field(&[0]),
            Some(c) => {
                field(&[1]);
                field(c.as_str().as_bytes());
            }
        }
        field(self.purpose.as_bytes());
        h.finalize().into()
    }
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

/// What the executor sends. Built by the kernel from the *authorized* recipient,
/// so the bytes on the wire are the bytes in policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Payment {
    /// Also the idempotency key at the rail. One proposal, at most one payment.
    pub proposal: ProposalId,
    pub to: RecipientId,
    pub amount: Amount,
}

/// A decision bound to the inputs that produced it.
///
/// **Only the kernel can make one.** The fields are private and there is no
/// `Deserialize`: within a process, holding an `Adjudication` is proof it came
/// out of [`crate::adjudicate`]. An executor in the same process cannot be handed
/// a forged one, and does not need a signature to know that. (Split the kernel
/// and executor across a process boundary and this stops being true — at that
/// point it needs a real signature, not this.)
///
/// It serializes, for the audit log. Reading that log back produces data, not
/// an `Adjudication` — a record cannot be re-promoted into authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Adjudication {
    proposal: ProposalId,
    #[serde(serialize_with = "as_hex")]
    proposal_digest: Hash,
    decision: Decision,
    #[serde(serialize_with = "as_hex")]
    policy_head: Hash,
    policy_version: u64,
    decided_at: Timestamp,
    payment: Option<Payment>,
}

fn as_hex<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex(h))
}

/// Why the executor must not act on an adjudication. Every variant means: do
/// not move money, release the reservation if there is one, and log this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotExecutable {
    /// It was a refusal. There is nothing to execute.
    Refused(RuleId),
    /// Policy has changed since the decision. A decision made under an older,
    /// possibly looser policy is not replayed under the new one; the agent may
    /// propose again and be judged by what is in force now.
    PolicyChanged { decided_under: Hash, current: Hash },
    /// The ledger holds no reservation for it: never reserved, or already
    /// settled or released. This is what stops a second execution.
    NoReservation,
    /// A reservation exists but its terms differ from the payment. Something
    /// edited the ledger. Do not guess which one is right.
    TermsMismatch,
}

impl Adjudication {
    pub(crate) fn new(
        proposal: &Proposal,
        decision: Decision,
        policy: &Policy,
        decided_at: Timestamp,
        payment: Option<Payment>,
    ) -> Self {
        Self {
            proposal: proposal.id.clone(),
            proposal_digest: proposal.digest(),
            decision,
            policy_head: policy.head,
            policy_version: policy.version,
            decided_at,
            payment,
        }
    }

    pub fn proposal(&self) -> &ProposalId {
        &self.proposal
    }
    pub fn proposal_digest(&self) -> &Hash {
        &self.proposal_digest
    }
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
    pub fn policy_head(&self) -> &Hash {
        &self.policy_head
    }
    pub fn policy_version(&self) -> u64 {
        self.policy_version
    }
    pub fn decided_at(&self) -> Timestamp {
        self.decided_at
    }
    pub fn payment(&self) -> Option<&Payment> {
        self.payment.as_ref()
    }

    /// The executor's precondition, as a pure function: is this safe to send
    /// *now*, against the policy and ledger held *now*?
    pub fn executable(&self, policy: &Policy, ledger: &Ledger) -> Result<&Payment, NotExecutable> {
        let payment = match (&self.decision, &self.payment) {
            (Decision::Refuse { rule }, _) => return Err(NotExecutable::Refused(*rule)),
            (Decision::Allow { .. }, Some(p)) => p,
            // Unreachable by construction; refused rather than assumed.
            (Decision::Allow { .. }, None) => return Err(NotExecutable::TermsMismatch),
        };
        if self.policy_head != policy.head {
            return Err(NotExecutable::PolicyChanged {
                decided_under: self.policy_head,
                current: policy.head,
            });
        }
        let r = ledger
            .reservation(&self.proposal)
            .ok_or(NotExecutable::NoReservation)?;
        if r.recipient != payment.to
            || r.minor != payment.amount.minor
            || payment.amount.asset != ledger.asset
        {
            return Err(NotExecutable::TermsMismatch);
        }
        Ok(payment)
    }
}
