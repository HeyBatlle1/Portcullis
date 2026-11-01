//! The audit chain's claims. Each test corresponds to a way an earlier audit
//! chain could be edited without detection.

use portcullis_kernel::audit::digest;
use portcullis_kernel::*;

fn chain(n: u64) -> AuditChain {
    let mut c = AuditChain::new();
    for i in 0..n {
        c.append(
            1_000 + i,
            "decision",
            format!(r#"{{"proposal":"p{i}","agent":"alice"}}"#),
        );
    }
    c
}

#[test]
fn a_clean_chain_verifies() {
    assert_eq!(chain(50).verify(), Ok(()));
    assert_eq!(AuditChain::new().verify(), Ok(()));
}

#[test]
fn editing_any_field_is_caught() {
    // The identity lives in the body, and the body is hashed. Nothing is
    // stored outside the digest.
    let mut records = chain(10).records().to_vec();
    records[3].body = records[3].body.replace("alice", "mallory");
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::Tampered { at: 3 })
    );

    let mut records = chain(10).records().to_vec();
    records[4].kind = "execution".into();
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::Tampered { at: 4 })
    );
}

/// Joining fields with a delimiter makes ("ab","c") and ("a","bc") the same
/// bytes. Length prefixes do not.
#[test]
fn field_boundaries_cannot_be_shifted() {
    let z = [0u8; 32];
    assert_ne!(digest(0, 1, "ab", "c", &z), digest(0, 1, "a", "bc", &z));
    assert_ne!(digest(0, 1, "a|b", "", &z), digest(0, 1, "a", "b", &z));
}

/// An edit far back in history, with every later hash recomputed so the links
/// are all consistent, is still caught: the edited record no longer hashes to
/// what its successor committed to, unless everything after it was rewritten —
/// and then the head someone held earlier is gone.
#[test]
fn an_old_edit_is_caught_however_long_the_chain() {
    let c = chain(1_000);
    let mut records = c.records().to_vec();
    records[5].body = records[5].body.replace("p5", "p5-forged");
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::Tampered { at: 5 })
    );
}

#[test]
fn a_full_rewrite_is_consistent_but_loses_the_old_head() {
    let honest = chain(100);
    let held_by_a_human = chain(100).records()[49].hash;
    assert!(honest.contains(&held_by_a_human));

    // Rewrite record 10 and rebuild everything after it, properly linked.
    let mut forged = AuditChain::new();
    for (i, r) in honest.records().iter().enumerate() {
        let body = if i == 10 {
            r.body.replace("p10", "p10-forged")
        } else {
            r.body.clone()
        };
        forged.append(r.at, &r.kind, body);
    }
    assert_eq!(forged.verify(), Ok(()), "internally consistent");
    assert!(
        !forged.contains(&held_by_a_human),
        "but the held head is gone"
    );
}

#[test]
fn removing_or_reordering_is_caught() {
    let mut records = chain(10).records().to_vec();
    records.remove(4);
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::SequenceBroken { at: 4 })
    );

    let mut records = chain(10).records().to_vec();
    records.swap(2, 3);
    // Swapped records keep their own seq numbers, so the numbering breaks first.
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::SequenceBroken { at: 2 })
    );

    // Renumber the swap so the sequence looks right: now the links break.
    let mut records = chain(10).records().to_vec();
    records.swap(2, 3);
    records[2].seq = 2;
    records[3].seq = 3;
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::ChainBroken { at: 2 })
    );
}

#[test]
fn time_cannot_run_backwards_in_a_stored_chain() {
    let mut records = chain(5).records().to_vec();
    records[3].at = 1;
    // Re-hash it so only the time rule can object.
    records[3].hash = digest(3, 1, &records[3].kind, &records[3].body, &records[3].prev);
    assert_eq!(
        AuditChain::from_records(records).verify(),
        Err(AuditError::TimeWentBackwards { at: 3 })
    );
}

#[test]
fn a_clock_stepping_back_on_append_is_flattened_not_lost() {
    let mut c = AuditChain::new();
    c.append(500, "decision", "a".into());
    let r = c.append(400, "decision", "b".into());
    assert_eq!(r.at, 500);
    assert_eq!(c.verify(), Ok(()));
}
