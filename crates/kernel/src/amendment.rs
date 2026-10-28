//! How policy changes, and why that is the only thing worth attacking here.
//!
//! Once the kernel is deterministic and an agent can only propose, there is no
//! runtime escalation left. So the attack moves: change what "allowed" means and
//! the kernel will approve a theft while working perfectly.
//!
//! Three mechanical controls, no judgement anywhere:
//!
//! - **Append-only and hash-chained.** Policy is not a table that gets edited. It
//!   is a log that gets replayed. A rewritten or missing entry breaks the chain
//!   and [`PolicyLog::verify`] says so.
//! - **Signed.** Every amendment carries a signature over its own hash. The kernel
//!   verifies through the [`Verifier`] trait and holds no key itself, so the
//!   signer can live in a hardware token that an attacker on this machine cannot
//!   reach.
//! - **Timelocked, asymmetrically.** Adding authority waits. Removing it is
//!   immediate — reducing what an agent can do is always safe, so it never waits.

use crate::policy::{Policy, Rule, StandingRecipient};
pub use crate::types::{hex, Hash};
use crate::types::{Duration, RecipientId, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Verifies a detached signature. The kernel never signs and never holds a
/// private key; it only ever asks "was this signed by that key?".
///
/// Implement over ed25519 for the software path, or over a hardware token where
/// the private key is non-exportable and signing needs a physical touch.
pub trait Verifier {
    fn verify(&self, message: &Hash, signature: &[u8], signer: &[u8]) -> bool;
}

/// A change to policy. Each variant is a named, bounded operation; there is no
/// "set arbitrary state" amendment, by design.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Amendment {
    /// Grant a recipient standing spend rights after `activation_delay`.
    ///
    /// This is the highest-privilege operation in the system. Prefer scheduling a
    /// commitment, which scopes the recipient to one payment and expires with it.
    AddStandingRecipient {
        recipient: RecipientId,
        activation_delay: Duration,
    },
    /// Revoke standing rights. Takes effect at once.
    RemoveStandingRecipient { recipient: RecipientId },
    /// Add a constraint. Constraints only ever reduce what is possible, so this
    /// takes effect at once.
    AddRule { rule: Rule },
    /// Drop a constraint. This *widens* authority, so it waits out `delay`.
    RemoveRule { rule: Rule, delay: Duration },
}

impl Amendment {
    /// Does applying this widen what the agent may do?
    pub fn widens_authority(&self) -> bool {
        matches!(
            self,
            Amendment::AddStandingRecipient { .. } | Amendment::RemoveRule { .. }
        )
    }
}

/// One entry in the chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAmendment {
    pub seq: u64,
    pub prev: Hash,
    pub body: Amendment,
    pub signed_at: Timestamp,
    /// The signing key's public half, as bytes.
    pub signer: Vec<u8>,
    pub signature: Vec<u8>,
}

impl SignedAmendment {
    /// What gets signed: this entry's content bound to everything before it.
    /// Changing any earlier entry changes this hash, so a forged history cannot
    /// keep its signatures.
    pub fn digest(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(self.seq.to_be_bytes());
        h.update(self.prev);
        h.update(self.signed_at.to_be_bytes());
        h.update(&self.signer);
        h.update(canonical_body(&self.body));
        h.finalize().into()
    }
}

/// A stable byte encoding of an amendment, so the digest does not depend on
/// serializer settings or field ordering.
fn canonical_body(a: &Amendment) -> Vec<u8> {
    let mut v = Vec::new();
    match a {
        Amendment::AddStandingRecipient {
            recipient,
            activation_delay,
        } => {
            v.push(1);
            v.extend_from_slice(recipient.as_str().as_bytes());
            v.push(0);
            v.extend_from_slice(&activation_delay.to_be_bytes());
        }
        Amendment::RemoveStandingRecipient { recipient } => {
            v.push(2);
            v.extend_from_slice(recipient.as_str().as_bytes());
        }
        Amendment::AddRule { rule } => {
            v.push(3);
            v.extend_from_slice(&rule_bytes(rule));
        }
        Amendment::RemoveRule { rule, delay } => {
            v.push(4);
            v.extend_from_slice(&rule_bytes(rule));
            v.push(0);
            v.extend_from_slice(&delay.to_be_bytes());
        }
    }
    v
}

fn rule_bytes(r: &Rule) -> Vec<u8> {
    let mut v = Vec::new();
    match r {
        Rule::PerTransactionMax { minor } => {
            v.push(1);
            v.extend_from_slice(&minor.to_be_bytes());
        }
        Rule::RollingWindowMax { minor, window } => {
            v.push(2);
            v.extend_from_slice(&minor.to_be_bytes());
            v.extend_from_slice(&window.to_be_bytes());
        }
        Rule::VelocityMax { count, window } => {
            v.push(3);
            v.extend_from_slice(&count.to_be_bytes());
            v.extend_from_slice(&window.to_be_bytes());
        }
        Rule::BalanceFloor { minor } => {
            v.push(4);
            v.extend_from_slice(&minor.to_be_bytes());
        }
        Rule::AssetAllowlist { assets } => {
            v.push(5);
            for a in assets {
                v.extend_from_slice(a.symbol.as_bytes());
                v.push(a.decimals);
                v.push(0);
            }
        }
        Rule::TimeOfDay {
            from_minute,
            until_minute,
        } => {
            v.push(6);
            v.extend_from_slice(&from_minute.to_be_bytes());
            v.extend_from_slice(&until_minute.to_be_bytes());
        }
    }
    v
}

/// Why a log failed to verify. Each one is a specific, nameable break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogError {
    /// Entries are not numbered 0, 1, 2, …
    SequenceBroken { at: u64 },
    /// An entry's `prev` is not the previous entry's digest. Someone edited history.
    ChainBroken { at: u64 },
    /// The signature does not check out against the named key.
    BadSignature { at: u64 },
    /// Time moved backwards between entries.
    TimeWentBackwards { at: u64 },
}

/// The append-only record from which policy is rebuilt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyLog {
    pub entries: Vec<SignedAmendment>,
}

impl PolicyLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Digest of the last entry, or zeroes for an empty log.
    pub fn head(&self) -> Hash {
        self.entries.last().map(|e| e.digest()).unwrap_or([0u8; 32])
    }

    /// Append without verifying. Callers that accept amendments from outside
    /// should run [`PolicyLog::verify`] afterwards and reject the whole log on
    /// error rather than trusting any part of it.
    pub fn append(&mut self, entry: SignedAmendment) {
        self.entries.push(entry);
    }

    /// Check numbering, the hash chain, monotonic time, and every signature.
    pub fn verify<V: Verifier>(&self, v: &V) -> Result<(), LogError> {
        let mut prev: Hash = [0u8; 32];
        let mut last_time: Timestamp = 0;
        for (i, e) in self.entries.iter().enumerate() {
            let i = i as u64;
            if e.seq != i {
                return Err(LogError::SequenceBroken { at: i });
            }
            if e.prev != prev {
                return Err(LogError::ChainBroken { at: i });
            }
            if e.signed_at < last_time {
                return Err(LogError::TimeWentBackwards { at: i });
            }
            let d = e.digest();
            if !v.verify(&d, &e.signature, &e.signer) {
                return Err(LogError::BadSignature { at: i });
            }
            prev = d;
            last_time = e.signed_at;
        }
        Ok(())
    }

    /// Rebuild the policy by replaying the log.
    ///
    /// Timelocks are applied here: an amendment that widens authority only takes
    /// effect once its delay has elapsed, which is what gives a human a day to
    /// notice a recipient they did not mean to add. Narrowing amendments apply the
    /// moment they are signed.
    ///
    /// Verify the log first. This function assumes it has already passed.
    pub fn replay(&self, now: Timestamp) -> Policy {
        let mut policy = Policy::default();
        for e in &self.entries {
            match &e.body {
                Amendment::AddStandingRecipient {
                    recipient,
                    activation_delay,
                } => {
                    let active_from = e.signed_at.saturating_add(*activation_delay);
                    policy.standing.retain(|s| &s.recipient != recipient);
                    policy.standing.push(StandingRecipient {
                        recipient: recipient.clone(),
                        added_at: e.signed_at,
                        active_from,
                    });
                }
                Amendment::RemoveStandingRecipient { recipient } => {
                    policy.standing.retain(|s| &s.recipient != recipient);
                }
                Amendment::AddRule { rule } => {
                    if !policy.rules.contains(rule) {
                        policy.rules.push(rule.clone());
                    }
                }
                Amendment::RemoveRule { rule, delay } => {
                    if now >= e.signed_at.saturating_add(*delay) {
                        policy.rules.retain(|r| r != rule);
                    }
                }
            }
            policy.version = e.seq + 1;
            policy.head = e.digest();
        }
        policy
    }

    /// Entries that have been signed but are not yet in force. This is what the
    /// console shows in its loudest panel, and what a human is meant to catch.
    pub fn pending(&self, now: Timestamp) -> Vec<&SignedAmendment> {
        self.entries
            .iter()
            .filter(|e| match &e.body {
                Amendment::AddStandingRecipient {
                    activation_delay, ..
                } => now < e.signed_at.saturating_add(*activation_delay),
                Amendment::RemoveRule { delay, .. } => now < e.signed_at.saturating_add(*delay),
                _ => false,
            })
            .collect()
    }
}
