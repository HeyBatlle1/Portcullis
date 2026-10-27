//! What is in the account, what is spoken for, and what is left.
//!
//! The one rule that makes earmarking real:
//!
//! ```text
//! available = balance - sum(open commitments)
//! ```
//!
//! An agent can only ever touch `available`. Without that subtraction an earmark
//! is a label on a sticky note.

use crate::types::{Amount, AssetId, CommitmentId, RecipientId, TimeWindow, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitmentState {
    /// Earmarked and spendable inside its window.
    Open,
    /// Already paid.
    Consumed,
    /// Window closed unfulfilled. Funds returned to the pool.
    Expired,
}

/// Money set aside for a named payment: this recipient, this much, this window.
///
/// A commitment is also a recipient grant scoped to itself. Paying a one-off
/// invoice should create one of these rather than add a standing recipient — the
/// recipient, the amount and the window then expire together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commitment {
    pub id: CommitmentId,
    pub recipient: RecipientId,
    /// Minor units, in the ledger's asset.
    pub earmarked: u64,
    pub window: TimeWindow,
    pub state: CommitmentState,
    /// Free text from the human who scheduled it. Shown, never matched against.
    pub memo: String,
}

impl Commitment {
    pub fn is_open(&self) -> bool {
        self.state == CommitmentState::Open
    }

    /// Open, and `now` falls inside the window.
    pub fn is_drawable(&self, now: Timestamp) -> bool {
        self.is_open() && self.window.contains(now)
    }
}

/// A payment that actually settled. Kept so rolling-window and velocity rules have
/// something to count; the kernel reads this and never writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settled {
    pub recipient: RecipientId,
    pub minor: u64,
    pub at: Timestamp,
}

/// One ledger per asset. Cross-asset arithmetic does not exist in this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub asset: AssetId,
    /// Everything pushed in, less everything settled. Minor units.
    pub balance: u64,
    pub commitments: Vec<Commitment>,
    pub settled: Vec<Settled>,
}

impl Ledger {
    pub fn new(asset: AssetId, balance: u64) -> Self {
        Self {
            asset,
            balance,
            commitments: Vec::new(),
            settled: Vec::new(),
        }
    }

    /// Total earmarked by commitments that are open and not past their window.
    ///
    /// A commitment whose window has closed stops encumbering even if nothing has
    /// marked it `Expired` yet, so a forgotten sweep cannot strand funds forever.
    pub fn encumbered(&self, now: Timestamp) -> u64 {
        self.commitments
            .iter()
            .filter(|c| c.is_open() && !c.window.has_closed(now))
            .map(|c| c.earmarked)
            .sum()
    }

    /// What the agent may actually draw on. Saturating: an over-committed ledger
    /// reports zero available rather than underflowing.
    pub fn available(&self, now: Timestamp) -> u64 {
        self.balance.saturating_sub(self.encumbered(now))
    }

    pub fn commitment(&self, id: &CommitmentId) -> Option<&Commitment> {
        self.commitments.iter().find(|c| &c.id == id)
    }

    /// The single open commitment this proposal could be drawing against, if
    /// exactly one matches. Ambiguity is not resolved by guessing: with two
    /// candidates this returns `None` and the agent must name one explicitly.
    pub fn auto_match(
        &self,
        recipient: &RecipientId,
        minor: u64,
        now: Timestamp,
    ) -> Option<&Commitment> {
        let mut found: Option<&Commitment> = None;
        for c in &self.commitments {
            if c.is_drawable(now) && c.recipient.matches(recipient) && minor <= c.earmarked {
                if found.is_some() {
                    return None;
                }
                found = Some(c);
            }
        }
        found
    }

    /// Total settled within the trailing `window` ending at `now`.
    pub fn settled_total_since(&self, since: Timestamp) -> u64 {
        self.settled
            .iter()
            .filter(|s| s.at >= since)
            .map(|s| s.minor)
            .sum()
    }

    /// Count of payments settled within the trailing window.
    pub fn settled_count_since(&self, since: Timestamp) -> u32 {
        self.settled.iter().filter(|s| s.at >= since).count() as u32
    }

    pub fn amount(&self, minor: u64) -> Amount {
        Amount::new(self.asset.clone(), minor)
    }
}

/// Whether each scheduled payment is covered by the balance, assuming they are
/// honoured in window order. Pure: the console reads this, it does not compute it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentForecast {
    pub id: CommitmentId,
    pub recipient: RecipientId,
    pub earmarked: u64,
    pub due_by: Timestamp,
    /// False once the running total passes the balance. This is the
    /// "payments 12 and 13 will not be funded" signal.
    pub funded: bool,
}

/// Walk the open commitments in due order and mark where the money runs out.
pub fn forecast(ledger: &Ledger, now: Timestamp) -> Vec<CommitmentForecast> {
    let mut open: Vec<&Commitment> = ledger
        .commitments
        .iter()
        .filter(|c| c.is_open() && !c.window.has_closed(now))
        .collect();
    open.sort_by_key(|c| c.window.until);

    let mut running = 0u64;
    open.into_iter()
        .map(|c| {
            running = running.saturating_add(c.earmarked);
            CommitmentForecast {
                id: c.id.clone(),
                recipient: c.recipient.clone(),
                earmarked: c.earmarked,
                due_by: c.window.until,
                funded: running <= ledger.balance,
            }
        })
        .collect()
}
