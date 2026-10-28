//! The decision. A pure function over data the caller supplies.
//!
//! Read the signature and you can see the whole security claim: a `&Ledger`, a
//! `&Policy`, a `&Proposal`, and a `now`. No network, no filesystem, no clock, no
//! model, no trait object that could hide any of those. There is no path by which
//! a language model can reach this code, which is why an agent cannot be talked
//! into changing its mind.

use crate::ledger::{Commitment, Ledger, Reservation};
use crate::policy::{Policy, Rule, RuleId};
use crate::proposal::{Adjudication, Authorization, Decision, Payment, Proposal};
use crate::types::{RecipientId, Timestamp};

const MINUTES_PER_DAY: u64 = 1440;

/// Decide a single proposal. Reads; never writes.
///
/// Two paths, because committed and discretionary spending are different things:
///
/// 1. **Against a commitment.** The human already authorized this payment by
///    scheduling it, so the discretionary rules do not apply — they would refuse a
///    payment that was explicitly approved. Only the balance floor still binds,
///    because that is a property of the account rather than of the authorization.
/// 2. **Discretionary.** The recipient must be standing and past its timelock, the
///    amount must fit inside `available`, and every rule must pass.
pub fn decide(ledger: &Ledger, policy: &Policy, proposal: &Proposal, now: Timestamp) -> Decision {
    match judge(ledger, policy, proposal, now) {
        Ok((_, via)) => Decision::Allow { via },
        Err(rule) => Decision::Refuse { rule },
    }
}

/// Decide, and if the answer is Allow, reserve the money in the same step.
///
/// This is the only way money gets reserved, and the only way an
/// [`Adjudication`] gets made. Taking `&mut Ledger` is the concurrency control:
/// the borrow checker will not let two of these run against one ledger at once,
/// so there is no window between "allowed" and "reserved" for a second proposal
/// to slip through. Put the ledger behind one lock and that holds across threads.
pub fn adjudicate(
    ledger: &mut Ledger,
    policy: &Policy,
    proposal: &Proposal,
    now: Timestamp,
) -> Adjudication {
    match judge(ledger, policy, proposal, now) {
        Err(rule) => Adjudication::new(proposal, Decision::Refuse { rule }, policy, now, None),
        Ok((to, via)) => {
            ledger.reserve(Reservation {
                proposal: proposal.id.clone(),
                recipient: to.clone(),
                minor: proposal.amount.minor,
                at: now,
                via: via.clone(),
            });
            let payment = Payment {
                proposal: proposal.id.clone(),
                to,
                amount: proposal.amount.clone(),
            };
            Adjudication::new(
                proposal,
                Decision::Allow { via },
                policy,
                now,
                Some(payment),
            )
        }
    }
}

/// The whole decision. On Allow, also returns the canonical recipient — the
/// authorized bytes, which are what gets paid.
fn judge(
    ledger: &Ledger,
    policy: &Policy,
    proposal: &Proposal,
    now: Timestamp,
) -> Result<(RecipientId, Authorization), RuleId> {
    // The agent's string is untrusted text until it parses. A homoglyph, a
    // stray space, a lookalike from another script: all stop here.
    let to = RecipientId::parse(&proposal.recipient).map_err(|_| RuleId::RecipientMalformed)?;

    if ledger.has_seen(&proposal.id) {
        return Err(RuleId::DuplicateProposal);
    }

    let minor = proposal.amount.minor;

    // The kernel holds no exchange rate and will not compare across assets.
    if !proposal.amount.same_asset(&ledger.asset) {
        return Err(RuleId::AssetMismatch);
    }
    asset_allowed(policy, proposal)?;

    // ── Path 1: the agent named a commitment. Naming one is an assertion. ──
    if let Some(id) = &proposal.against {
        let c = ledger.commitment(id).ok_or(RuleId::CommitmentNotFound)?;
        check_commitment(c, &to, minor, now)?;
        return settle_against_commitment(ledger, policy, minor, c).map(|via| (to, via));
    }

    // ── Path 1b: exactly one open commitment fits. Ambiguity is never guessed. ──
    if let Some(c) = ledger.auto_match(&to, minor, now) {
        return settle_against_commitment(ledger, policy, minor, c).map(|via| (to, via));
    }

    // ── Path 2: discretionary. ──
    match policy.standing_for(&to) {
        None => return Err(RuleId::RecipientNotAllowed),
        Some(s) if !s.is_active(now) => return Err(RuleId::RecipientNotYetActive),
        Some(_) => {}
    }

    if ledger.unreserved() < minor {
        return Err(RuleId::InsufficientFunds);
    }
    // Enough money is present, but some of it is spoken for.
    if ledger.available(now) < minor {
        return Err(RuleId::FundsEncumbered);
    }

    for rule in &policy.rules {
        apply(rule, ledger, minor, now)?;
    }

    Ok((to, Authorization::Discretionary))
}

fn asset_allowed(policy: &Policy, proposal: &Proposal) -> Result<(), RuleId> {
    for rule in &policy.rules {
        if let Rule::AssetAllowlist { assets } = rule {
            if !assets.contains(&proposal.amount.asset) {
                return Err(RuleId::AssetNotAllowed);
            }
        }
    }
    Ok(())
}

/// Every way a named commitment can fail to back this proposal.
fn check_commitment(
    c: &Commitment,
    to: &RecipientId,
    minor: u64,
    now: Timestamp,
) -> Result<(), RuleId> {
    if !c.is_open() {
        return Err(RuleId::CommitmentNotOpen);
    }
    if !c.window.contains(now) {
        return Err(RuleId::CommitmentWindowClosed);
    }
    if &c.recipient != to {
        return Err(RuleId::CommitmentRecipientMismatch);
    }
    if minor > c.earmarked {
        return Err(RuleId::CommitmentAmountExceeded);
    }
    Ok(())
}

/// A commitment draw checks the balance and the floor, and nothing else.
fn settle_against_commitment(
    ledger: &Ledger,
    policy: &Policy,
    minor: u64,
    c: &Commitment,
) -> Result<Authorization, RuleId> {
    let pool = ledger.unreserved();
    if pool < minor {
        return Err(RuleId::InsufficientFunds);
    }
    for rule in &policy.rules {
        if let Rule::BalanceFloor { minor: floor } = rule {
            if pool.saturating_sub(minor) < *floor {
                return Err(RuleId::BreachesBalanceFloor);
            }
        }
    }
    Ok(Authorization::Commitment(c.id.clone()))
}

/// One rule against one proposal.
fn apply(rule: &Rule, ledger: &Ledger, minor: u64, now: Timestamp) -> Result<(), RuleId> {
    let fail = |broken: bool, id: RuleId| if broken { Err(id) } else { Ok(()) };
    match rule {
        Rule::PerTransactionMax { minor: max } => fail(minor > *max, RuleId::ExceedsPerTransaction),
        Rule::RollingWindowMax { minor: max, window } => {
            let spent = ledger.outflow_since(now.saturating_sub(*window));
            fail(
                spent.saturating_add(minor) > *max,
                RuleId::ExceedsRollingWindow,
            )
        }
        Rule::VelocityMax { count, window } => {
            let n = ledger.outflow_count_since(now.saturating_sub(*window));
            fail(n.saturating_add(1) > *count, RuleId::ExceedsVelocity)
        }
        Rule::BalanceFloor { minor: floor } => fail(
            ledger.unreserved().saturating_sub(minor) < *floor,
            RuleId::BreachesBalanceFloor,
        ),
        Rule::AssetAllowlist { .. } => Ok(()), // checked before either path
        Rule::TimeOfDay {
            from_minute,
            until_minute,
        } => fail(
            !in_window(now, *from_minute, *until_minute),
            RuleId::OutsideTimeWindow,
        ),
    }
}

/// Minutes-from-UTC-midnight containment, half-open, wrapping across midnight.
/// `from == until` means the rule never excludes anything.
fn in_window(now: Timestamp, from: u32, until: u32) -> bool {
    if from == until {
        return true;
    }
    let m = ((now / 60) % MINUTES_PER_DAY) as u32;
    if from < until {
        m >= from && m < until
    } else {
        m >= from || m < until
    }
}
