//! A payment rail, as the executor needs to see one: submit a payment, ask what
//! happened to it. PayPal Payouts is the implementation; tests use a scripted
//! fake. Nothing above this trait knows which.

use portcullis_kernel::Payment;
use std::future::Future;

/// What a submission attempt established. The three-way split is the whole
/// point: it separates "nothing was created" from "we don't know".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// The rail holds a payout for this payment. Either we just created it, or
    /// an earlier attempt did and the rail said so.
    Accepted { batch_id: String },
    /// The rail definitively did not create anything. Safe to release.
    Rejected { reason: String },
    /// We cannot tell whether anything was created — timeout, 5xx, a response
    /// we could not parse. **Never** safe to release: money may have moved.
    /// The only safe move is to ask again with the same idempotency key.
    Unknown { reason: String },
}

/// The state of a single payout item, as the rail reports it.
///
/// Mirrors PayPal's `transaction_status`. A batch-level DENIED or CANCELED with
/// no item is reported as [`ItemStatus::Failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Pending,
    OnHold,
    /// Delivered and claimed.
    Success,
    /// Debited from our balance; the receiver has not claimed it. Becomes
    /// `Returned` if it goes unclaimed long enough.
    Unclaimed,
    Failed,
    Blocked,
    Returned,
    Refunded,
    Reversed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observed {
    Status(ItemStatus),
    /// Could not learn anything this time. Changes nothing.
    Unknown {
        reason: String,
    },
}

/// What the ledger should do about an observed status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The money left. Debit it.
    Settle { final_: bool },
    /// The money never left. Return it to the pool.
    Release,
    /// Money that left has come back. Credit it.
    CreditBack,
    /// Nothing to do yet.
    Wait,
    /// Settled and claimed, or credited back: nothing more will happen.
    Done,
    /// A transition that should not happen, like FAILED after SUCCESS. Touch
    /// nothing; a human looks at it.
    Anomaly,
}

/// Pure: given what the rail says and whether we have already settled, what
/// does the ledger do? Every case is enumerated, so a new status from the rail
/// is a compile error here rather than a silent default.
pub fn step(status: ItemStatus, already_settled: bool) -> Step {
    use ItemStatus::*;
    match (status, already_settled) {
        (Pending | OnHold, false) => Step::Wait,
        (Success, false) => Step::Settle { final_: true },
        // Gone from our balance even though nobody has claimed it yet.
        (Unclaimed, false) => Step::Settle { final_: false },
        // Never left, or left and came back before we saw it go: net zero.
        (Failed | Blocked | Returned | Refunded | Reversed, false) => Step::Release,

        (Success, true) => Step::Done,
        (Unclaimed, true) => Step::Wait,
        (Returned | Refunded | Reversed, true) => Step::CreditBack,
        (Pending | OnHold | Failed | Blocked, true) => Step::Anomaly,
    }
}

pub trait Rail: Send + Sync {
    /// Submit, using `payment.proposal` as the idempotency key. Submitting the
    /// same payment twice must not pay twice.
    fn submit(&self, payment: &Payment) -> impl Future<Output = Submitted> + Send;

    fn observe(&self, batch_id: &str) -> impl Future<Output = Observed> + Send;
}
