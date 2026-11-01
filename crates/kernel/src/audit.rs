//! The record of what happened, kept so that editing it is detectable.
//!
//! Every decision and every execution event is appended here. Each record's
//! hash covers its own fields and the previous record's hash, so changing,
//! removing or reordering anything breaks the chain at a specific, nameable
//! record.
//!
//! The design answers four specific failures found in an earlier audit chain
//! (a sibling project's, reviewed before this was written):
//!
//! 1. **Every field is hashed.** That chain stored an identity column the hash
//!    did not cover, so "who did this" was the one thing you could quietly
//!    rewrite. Here there is nothing stored outside the digest.
//! 2. **Fields are length-prefixed, not joined with a delimiter.** Joining
//!    with `|` means `("ab", "c")` and `("a", "bc")` can hash the same once a
//!    free-text field contains the delimiter. A length prefix makes the
//!    encoding unambiguous.
//! 3. **[`AuditChain::verify`] walks every record**, not the most recent N. An
//!    edit at record 5 of 10,000 followed by recomputing everything after it
//!    is caught by anyone who held an earlier head.
//! 4. **The head is meant to be read back.** That chain wrote daily anchors
//!    and nothing ever compared against them — a procedure that ran, with no
//!    one checking what it produced. [`AuditChain::head`] is shown on the
//!    console beside every refresh, so a human holding yesterday's head can
//!    ask whether today's chain still contains it ([`AuditChain::contains`]).
//!
//! The body is stored as the exact bytes that were hashed. Nothing is
//! re-serialized for verification, so there is no canonical-JSON question to
//! get wrong.
//!
//! Pure: no clock, no storage. The caller passes `at` and decides where the
//! records live.

use crate::types::{hex, Hash, Timestamp};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub at: Timestamp,
    /// What sort of record, e.g. `decision` or `execution`.
    pub kind: String,
    /// The record itself, exactly as hashed. Typically a JSON document.
    pub body: String,
    #[serde(serialize_with = "as_hex")]
    pub prev: Hash,
    #[serde(serialize_with = "as_hex")]
    pub hash: Hash,
}

fn as_hex<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex(h))
}

/// The one encoding. Domain-separated, every field length-prefixed.
pub fn digest(seq: u64, at: Timestamp, kind: &str, body: &str, prev: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(b"portcullis.audit.v1");
    for field in [
        &seq.to_be_bytes()[..],
        &at.to_be_bytes()[..],
        kind.as_bytes(),
        body.as_bytes(),
        &prev[..],
    ] {
        h.update((field.len() as u64).to_be_bytes());
        h.update(field);
    }
    h.finalize().into()
}

/// Where the chain stopped being trustworthy. Every variant names a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditError {
    /// Records are not numbered 0, 1, 2, … — one was removed or inserted.
    SequenceBroken { at: u64 },
    /// A record's `prev` is not the previous record's hash — history was
    /// reordered or replaced.
    ChainBroken { at: u64 },
    /// A record's contents do not match its own hash — it was edited.
    Tampered { at: u64 },
    /// Time ran backwards between two records.
    TimeWentBackwards { at: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditChain {
    records: Vec<AuditRecord>,
}

impl AuditChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from stored records. Run [`AuditChain::verify`] before trusting it.
    pub fn from_records(records: Vec<AuditRecord>) -> Self {
        Self { records }
    }

    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Hash of the newest record, or zeroes for an empty chain.
    pub fn head(&self) -> Hash {
        self.records.last().map(|r| r.hash).unwrap_or([0u8; 32])
    }

    /// Append, linking to the current head. A timestamp earlier than the last
    /// record's is clamped up to it rather than rejected: the caller's clock
    /// stepped back, and losing the record would be worse than a flat
    /// timestamp. `verify` holds stored chains to the same rule.
    pub fn append(&mut self, at: Timestamp, kind: &str, body: String) -> &AuditRecord {
        let seq = self.records.len() as u64;
        let at = self.records.last().map_or(at, |r| at.max(r.at));
        let prev = self.head();
        let hash = digest(seq, at, kind, &body, &prev);
        self.records.push(AuditRecord {
            seq,
            at,
            kind: kind.to_string(),
            body,
            prev,
            hash,
        });
        self.records.last().expect("just pushed")
    }

    /// Check every record, oldest first. Not the last N: an edit anywhere is
    /// an edit.
    pub fn verify(&self) -> Result<(), AuditError> {
        let mut prev: Hash = [0u8; 32];
        let mut last_at: Timestamp = 0;
        for (i, r) in self.records.iter().enumerate() {
            let i = i as u64;
            if r.seq != i {
                return Err(AuditError::SequenceBroken { at: i });
            }
            if r.prev != prev {
                return Err(AuditError::ChainBroken { at: i });
            }
            if r.at < last_at {
                return Err(AuditError::TimeWentBackwards { at: i });
            }
            if digest(r.seq, r.at, &r.kind, &r.body, &r.prev) != r.hash {
                return Err(AuditError::Tampered { at: i });
            }
            prev = r.hash;
            last_at = r.at;
        }
        Ok(())
    }

    /// Does this chain still contain a head someone recorded earlier? A
    /// verified chain that no longer contains yesterday's head was rewritten,
    /// even if every link in it is internally consistent.
    pub fn contains(&self, earlier_head: &Hash) -> bool {
        self.records.iter().any(|r| &r.hash == earlier_head)
    }
}
