//! The record kept in `demo/evidence/` still verifies, with the kernel's own
//! hash chain: every record's hash recomputed from its fields, every `prev`
//! the hash before it. Editing one character of that file fails this test.

use portcullis_kernel::{AuditChain, AuditRecord, Hash};
use serde_json::Value;

fn unhex(s: &str) -> Hash {
    assert_eq!(s.len(), 64, "a hash is 64 hex characters");
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    h
}

#[test]
fn the_kept_hosted_record_verifies() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../demo/evidence/2026-11-10-hosted.audit.jsonl"
    );
    let text = std::fs::read_to_string(path).expect("the evidence file");
    let records: Vec<AuditRecord> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("a JSON line");
            AuditRecord {
                seq: v["seq"].as_u64().unwrap(),
                at: v["at"].as_u64().unwrap(),
                kind: v["kind"].as_str().unwrap().into(),
                body: v["body"].as_str().unwrap().into(),
                prev: unhex(v["prev"].as_str().unwrap()),
                hash: unhex(v["hash"].as_str().unwrap()),
            }
        })
        .collect();
    assert_eq!(records.len(), 32);
    AuditChain::from_records(records).verify().unwrap();
}
