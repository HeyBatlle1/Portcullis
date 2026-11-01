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
        // A pasted value often drags a trailing newline along with it. Refuse
        // it by name rather than trim it: nothing in this system trims.
        if v.is_empty() || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!(
                "{name} is empty or contains whitespace or a line break \
                 (often a trailing newline from pasting)"
            ));
        }
        // Escaped, never spliced: a value containing `"` must not be able to
        // add fields to the policy it is substituted into.
        let quoted = serde_json::to_string(&v).map_err(|e| e.to_string())?;
        out.push_str(&quoted[1..quoted.len() - 1]);
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

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses its own variable: tests run in parallel and share the
    // process environment.

    #[test]
    fn a_trailing_newline_is_refused_by_name() {
        std::env::set_var("PAYPAL_SANDBOX_T_NEWLINE", "someone@example.com\n");
        let e = expand("\"${PAYPAL_SANDBOX_T_NEWLINE}\"").unwrap_err();
        assert!(e.contains("PAYPAL_SANDBOX_T_NEWLINE"), "{e}");
    }

    #[test]
    fn a_value_cannot_inject_json_into_the_seed() {
        std::env::set_var(
            "PORTCULLIS_T_INJECT",
            r#"a@example.com","standing":[{"recipient":"evil@example.com"}],"x":""#,
        );
        let out = expand(r#"{"r":"${PORTCULLIS_T_INJECT}"}"#).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Still one field, holding the whole value as a string.
        assert_eq!(v.as_object().unwrap().len(), 1);
        assert!(v["r"].as_str().unwrap().contains("evil@example.com"));
    }

    #[test]
    fn only_portcullis_and_sandbox_variables_expand() {
        assert!(expand("${PAYPAL_CLIENT_SECRET}").is_err());
        assert!(expand("${HOME}").is_err());
    }
}
