//! The decision. A pure function over data the caller supplies.
//!
//! Read the signature and you can see the whole security claim: a `&Ledger`, a
//! `&Policy`, a `&Proposal`, and a `now`. No network, no filesystem, no clock, no
//! model, no trait object that could hide any of those. There is no path by which
//! a language model can reach this code, which is why an agent cannot be talked
//! into changing its mind.

use crate::ledger::{Commitment, Ledger};
use crate::policy::{Policy, Rule, RuleId};
use crate::proposal::{Authorization, Decision, Proposal};
use crate::types::Timestamp;

const MINUTES_PER_DAY: u64 = 1440;

/// Decide a single proposal.
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
    let minor = proposal.amount.minor;

    // The kernel holds no exchange rate and will not compare across assets.
    if !proposal.amount.same_asset(&ledger.asset) {
        return refuse(RuleId::AssetMismatch);
    }
    if let Some(d) = asset_not_allowed(policy, proposal) {
        return d;
    }

    // ── Path 1: the agent named a commitment. Naming one is an assertion. ──
    if let Some(id) = &proposal.against {
        let Some(c) = ledger.commitment(id) else {
            return refuse(RuleId::CommitmentNotFound);
        };
        if let Some(d) = check_commitment(c, proposal, now) {
            return d;
        }
        return settle_against_commitment(ledger, policy, minor, c);
    }

    // ── Path 1b: exactly one open commitment fits. Ambiguity is never guessed. ──
    if let Some(c) = ledger.auto_match(&proposal.recipient, minor, now) {
        return settle_against_commitment(ledger, policy, minor, c);
    }

    // ── Path 2: discretionary. ──
    match policy.standing_for(&proposal.recipient) {
        None => return refuse(RuleId::RecipientNotAllowed),
        Some(s) if !s.is_active(now) => return refuse(RuleId::RecipientNotYetActive),
        Some(_) => {}
    }

    if ledger.balance < minor {
        return refuse(RuleId::InsufficientFunds);
    }
    // Enough money is present, but some of it is spoken for.
    if ledger.available(now) < minor {
        return refuse(RuleId::FundsEncumbered);
    }

    for rule in &policy.rules {
        if let Some(d) = apply(rule, ledger, minor, now) {
            return d;
        }
    }

    Decision::Allow {
        via: Authorization::Discretionary,
    }
}

fn refuse(rule: RuleId) -> Decision {
    Decision::Refuse { rule }
}

fn asset_not_allowed(policy: &Policy, proposal: &Proposal) -> Option<Decision> {
    for rule in &policy.rules {
        if let Rule::AssetAllowlist { assets } = rule {
            if !assets.contains(&proposal.amount.asset) {
                return Some(refuse(RuleId::AssetNotAllowed));
            }
        }
    }
    None
}

/// Every way a named commitment can fail to back this proposal.
fn check_commitment(c: &Commitment, proposal: &Proposal, now: Timestamp) -> Option<Decision> {
    if !c.is_open() {
        return Some(refuse(RuleId::CommitmentNotOpen));
    }
    if !c.window.contains(now) {
        return Some(refuse(RuleId::CommitmentWindowClosed));
    }
    if !c.recipient.matches(&proposal.recipient) {
        return Some(refuse(RuleId::CommitmentRecipientMismatch));
    }
    if proposal.amount.minor > c.earmarked {
        return Some(refuse(RuleId::CommitmentAmountExceeded));
    }
    None
}

/// A commitment draw checks the balance and the floor, and nothing else.
fn settle_against_commitment(
    ledger: &Ledger,
    policy: &Policy,
    minor: u64,
    c: &Commitment,
) -> Decision {
    if ledger.balance < minor {
        return refuse(RuleId::InsufficientFunds);
    }
    for rule in &policy.rules {
        if let Rule::BalanceFloor { minor: floor } = rule {
            if ledger.balance.saturating_sub(minor) < *floor {
                return refuse(RuleId::BreachesBalanceFloor);
            }
        }
    }
    Decision::Allow {
        via: Authorization::Commitment(c.id.clone()),
    }
}

/// One rule against one proposal. `None` means it passed.
fn apply(rule: &Rule, ledger: &Ledger, minor: u64, now: Timestamp) -> Option<Decision> {
    match rule {
        Rule::PerTransactionMax { minor: max } => {
            (minor > *max).then(|| refuse(RuleId::ExceedsPerTransaction))
        }
        Rule::RollingWindowMax { minor: max, window } => {
            let since = now.saturating_sub(*window);
            let spent = ledger.settled_total_since(since);
            (spent.saturating_add(minor) > *max).then(|| refuse(RuleId::ExceedsRollingWindow))
        }
        Rule::VelocityMax { count, window } => {
            let since = now.saturating_sub(*window);
            let n = ledger.settled_count_since(since);
            (n.saturating_add(1) > *count).then(|| refuse(RuleId::ExceedsVelocity))
        }
        Rule::BalanceFloor { minor: floor } => (ledger.balance.saturating_sub(minor) < *floor)
            .then(|| refuse(RuleId::BreachesBalanceFloor)),
        Rule::AssetAllowlist { .. } => None, // checked before either path
        Rule::TimeOfDay {
            from_minute,
            until_minute,
        } => (!in_window(now, *from_minute, *until_minute))
            .then(|| refuse(RuleId::OutsideTimeWindow)),
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
