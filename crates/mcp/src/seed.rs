//! The starting state a demo boots into: balance, scheduled payments, standing
//! recipients, rules. Times are offsets from boot, so a seed never goes stale.
//!
//! **Known gap, on purpose and in the open:** policy here is loaded directly,
//! not replayed from a signed amendment log. The kernel's `PolicyLog` exists
//! and is tested; what is missing is an ed25519 `Verifier` and durable log
//! storage. Until those land, the hosted demo's policy is whatever this file
//! says — which is exactly the trust a signed log exists to remove.

use portcullis_kernel::{
    AssetId, Commitment, CommitmentId, CommitmentState, Ledger, Policy, RecipientId, Rule,
    StandingRecipient, TimeWindow, Timestamp,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Seed {
    pub asset: AssetId,
    pub balance: u64,
    #[serde(default)]
    pub standing: Vec<SeedStanding>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub commitments: Vec<SeedCommitment>,
}

#[derive(Debug, Deserialize)]
pub struct SeedStanding {
    pub recipient: String,
    /// Seconds after boot the grant was signed. Negative means before boot.
    #[serde(default)]
    pub added: i64,
    /// Seconds after boot it becomes spendable. Positive means still timelocked.
    #[serde(default)]
    pub active: i64,
}

#[derive(Debug, Deserialize)]
pub struct SeedCommitment {
    pub id: String,
    pub recipient: String,
    pub earmarked: u64,
    pub opens: i64,
    pub closes: i64,
    #[serde(default)]
    pub memo: String,
}

fn at(boot: Timestamp, offset: i64) -> Timestamp {
    boot.saturating_add_signed(offset)
}

/// Replace `${NAME}` with the value of an environment variable. Only names that
/// start with `PORTCULLIS_` or `PAYPAL_SANDBOX_` are expanded, so a seed file
/// cannot be used to read credentials out of the environment.
pub fn expand(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or("unterminated ${")?;
        let name = &after[..end];
        if !(name.starts_with("PORTCULLIS_") || name.starts_with("PAYPAL_SANDBOX_")) {
            return Err(format!("seed may not reference ${{{name}}}"));
        }
        let v = std::env::var(name).map_err(|_| format!("{name} is not set"))?;
        out.push_str(&v);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Seed {
    pub fn parse(json: &str) -> Result<Seed, String> {
        serde_json::from_str(&expand(json)?).map_err(|e| format!("seed: {e}"))
    }

    pub fn build(&self, boot: Timestamp) -> Result<(Ledger, Policy), String> {
        let rid =
            |s: &str| RecipientId::parse(s).map_err(|e| format!("seed recipient {s:?}: {e:?}"));
        let mut ledger = Ledger::new(self.asset.clone(), self.balance);
        for c in &self.commitments {
            ledger.commitments.push(Commitment {
                id: CommitmentId::new(&c.id),
                recipient: rid(&c.recipient)?,
                earmarked: c.earmarked,
                window: TimeWindow::new(at(boot, c.opens), at(boot, c.closes)),
                state: CommitmentState::Open,
                memo: c.memo.clone(),
            });
        }
        let mut standing = Vec::new();
        for s in &self.standing {
            standing.push(StandingRecipient {
                recipient: rid(&s.recipient)?,
                added_at: at(boot, s.added),
                active_from: at(boot, s.active),
            });
        }
        let policy = Policy {
            standing,
            rules: self.rules.clone(),
            version: 1,
            head: [0u8; 32],
        };
        Ok((ledger, policy))
    }
}
