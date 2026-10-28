//! What is in the account, what is spoken for, what is in flight, and what is
//! left.
//!
//! ```text
//! unreserved = balance − reserved          money not already on its way out
//! available  = unreserved − encumbered     money the agent may draw on
//! ```
//!
//! `encumbered` is what makes an earmark real rather than a sticky note.
//! `reserved` is what makes a decision real before the payment rail confirms it:
//! an Allow takes its money off the table the instant it is issued, so ten
//! proposals fired in the same second are judged against ten successively
//! smaller balances, not ten copies of the same one.

use crate::proposal::Authorization;
use crate::types::{Amount, AssetId, CommitmentId, ProposalId, RecipientId, TimeWindow, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitmentState {
    /// Earmarked and spendable inside its window.
    Open,
    /// A payment against it is in flight. Not drawable again; its money is held
    /// by the reservation rather than the earmark.
    Drawing,
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

/// An allowed payment that has not yet settled. Created only by
/// [`crate::adjudicate`], at the same instant as the Allow, so there is no gap in
/// which the same money can be allowed twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub proposal: ProposalId,
    pub recipient: RecipientId,
    pub minor: u64,
    pub at: Timestamp,
    pub via: Authorization,
}

/// A payment that actually settled. Kept so rolling-window and velocity rules have
/// something to count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settled {
    pub proposal: ProposalId,
    pub recipient: RecipientId,
    pub minor: u64,
    pub at: Timestamp,
}

/// Why a settlement transition was refused. Each is a bookkeeping fault the
/// executor must surface, never paper over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerError {
    /// No reservation for this proposal. Either it was never allowed, or it has
    /// already been settled or released — acting on it again would double-count.
    NoReservation(ProposalId),
}

/// One ledger per asset. Cross-asset arithmetic does not exist in this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub asset: AssetId,
    /// Everything pushed in, less everything settled. Minor units.
    pub balance: u64,
    pub commitments: Vec<Commitment>,
    pub reserved: Vec<Reservation>,
    pub settled: Vec<Settled>,
}

impl Ledger {
    pub fn new(asset: AssetId, balance: u64) -> Self {
        Self {
            asset,
            balance,
            commitments: Vec::new(),
            reserved: Vec::new(),
            settled: Vec::new(),
        }
    }

    /// Total held by in-flight payments.
    pub fn reserved_total(&self) -> u64 {
        self.reserved.iter().map(|r| r.minor).sum()
    }

    /// The balance less everything already on its way out.
    pub fn unreserved(&self) -> u64 {
        self.balance.saturating_sub(self.reserved_total())
    }

    /// Total earmarked by commitments that are open and not past their window.
    ///
    /// A commitment whose window has closed stops encumbering even if nothing has
    /// marked it `Expired` yet, so a forgotten sweep cannot strand funds forever.
    /// A commitment in `Drawing` does not encumber: its reservation holds the
    /// money instead, and counting both would hold it twice.
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
        self.unreserved().saturating_sub(self.encumbered(now))
    }

    pub fn commitment(&self, id: &CommitmentId) -> Option<&Commitment> {
        self.commitments.iter().find(|c| &c.id == id)
    }

    pub fn reservation(&self, id: &ProposalId) -> Option<&Reservation> {
        self.reserved.iter().find(|r| &r.proposal == id)
    }

    /// Has this proposal id already reached money — in flight or settled?
    pub fn has_seen(&self, id: &ProposalId) -> bool {
        self.reservation(id).is_some() || self.settled.iter().any(|s| &s.proposal == id)
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
            if c.is_drawable(now) && &c.recipient == recipient && minor <= c.earmarked {
                if found.is_some() {
                    return None;
                }
                found = Some(c);
            }
        }
        found
    }

    /// Money that has left, or is leaving, since `since`: settled payments plus
    /// reservations. Rate rules count both, or firing proposals faster than the
    /// rail settles them would walk straight past a velocity limit.
    pub fn outflow_since(&self, since: Timestamp) -> u64 {
        let settled: u64 = self
            .settled
            .iter()
            .filter(|s| s.at >= since)
            .map(|s| s.minor)
            .sum();
        let reserved: u64 = self
            .reserved
            .iter()
            .filter(|r| r.at >= since)
            .map(|r| r.minor)
            .sum();
        settled.saturating_add(reserved)
    }

    /// Count of payments made or in flight since `since`.
    pub fn outflow_count_since(&self, since: Timestamp) -> u32 {
        let s = self.settled.iter().filter(|s| s.at >= since).count();
        let r = self.reserved.iter().filter(|r| r.at >= since).count();
        (s + r) as u32
    }

    pub fn amount(&self, minor: u64) -> Amount {
        Amount::new(self.asset.clone(), minor)
    }

    // ── transitions ─────────────────────────────────────────────────────────
    //
    // Pure state changes, no I/O. The executor calls `settle` or `release` when
    // the rail reports an outcome; only `adjudicate` may call `reserve`.

    pub(crate) fn reserve(&mut self, r: Reservation) {
        if let Authorization::Commitment(id) = &r.via {
            if let Some(c) = self.commitments.iter_mut().find(|c| &c.id == id) {
                c.state = CommitmentState::Drawing;
            }
        }
        self.reserved.push(r);
    }

    /// The rail confirmed the money left. Debit the balance, record it, and
    /// consume the commitment it drew against.
    pub fn settle(&mut self, id: &ProposalId, at: Timestamp) -> Result<Settled, LedgerError> {
        let r = self.take_reservation(id)?;
        self.balance = self.balance.saturating_sub(r.minor);
        self.set_commitment(&r.via, CommitmentState::Consumed);
        let s = Settled {
            proposal: r.proposal,
            recipient: r.recipient,
            minor: r.minor,
            at,
        };
        self.settled.push(s.clone());
        Ok(s)
    }

    /// The payment did not happen — denied, failed, or refused by the executor.
    /// The money returns to the pool and a commitment it drew on reopens.
    pub fn release(&mut self, id: &ProposalId) -> Result<Reservation, LedgerError> {
        let r = self.take_reservation(id)?;
        self.set_commitment(&r.via, CommitmentState::Open);
        Ok(r)
    }

    /// Money pushed in: funding, or a settled payment that came back
    /// (unclaimed, returned, reversed). History is not rewritten; a returned
    /// payment still happened, and still counted against the rate limits.
    pub fn credit(&mut self, minor: u64) {
        self.balance = self.balance.saturating_add(minor);
    }

    fn take_reservation(&mut self, id: &ProposalId) -> Result<Reservation, LedgerError> {
        let i = self
            .reserved
            .iter()
            .position(|r| &r.proposal == id)
            .ok_or_else(|| LedgerError::NoReservation(id.clone()))?;
        Ok(self.reserved.remove(i))
    }

    fn set_commitment(&mut self, via: &Authorization, state: CommitmentState) {
        if let Authorization::Commitment(cid) = via {
            if let Some(c) = self.commitments.iter_mut().find(|c| &c.id == cid) {
                c.state = state;
            }
        }
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
    /// False once the running total passes the unreserved balance. This is the
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

    let pool = ledger.unreserved();
    let mut running = 0u64;
    open.into_iter()
        .map(|c| {
            running = running.saturating_add(c.earmarked);
            CommitmentForecast {
                id: c.id.clone(),
                recipient: c.recipient.clone(),
                earmarked: c.earmarked,
                due_by: c.window.until,
                funded: running <= pool,
            }
        })
        .collect()
}
