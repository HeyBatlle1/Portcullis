//! The chain, kept.
//!
//! By default the audit chain lives in memory and a restart begins a new one.
//! With `PORTCULLIS_CHAIN_FILE` set, every record is also appended to that
//! file as it is chained, one JSON line each (the same lines `/console/audit.jsonl`
//! serves), and on boot the file is read back, verified from record 0, and
//! appended to. The record then outlives the process, and
//! [`AuditChain::contains`] becomes something a visitor can test: write down
//! today's head, come back after a deploy, and ask whether the chain still
//! holds it.
//!
//! Only the record is kept. The ledger starts again from its seed, and the
//! chain says so with a `boot` record, because a record that hid its own
//! restarts would be claiming a continuity the money does not have.
//!
//! A kept chain that does not verify stops the server. Carrying on from a
//! record someone edited would put new, honest hashes on top of it.

use portcullis_kernel::{AuditChain, AuditRecord, Hash};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

fn unhex(s: &str) -> Option<Hash> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(h)
}

/// One exported line back into a record. Data, not authority: nothing read
/// here is trusted until [`AuditChain::verify`] has walked it.
pub fn parse_line(line: &str) -> Option<AuditRecord> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(AuditRecord {
        seq: v["seq"].as_u64()?,
        at: v["at"].as_u64()?,
        kind: v["kind"].as_str()?.to_string(),
        body: v["body"].as_str()?.to_string(),
        prev: unhex(v["prev"].as_str()?)?,
        hash: unhex(v["hash"].as_str()?)?,
    })
}

/// Read a kept chain and verify all of it. A missing file is an empty chain.
///
/// A crash can leave half a line at the very end. Half a line was never a
/// record (its hash was never handed to anyone), so it is cut off and the
/// chain resumes from the last whole one. Anything else that does not parse
/// or verify is an error naming the record.
pub fn load(path: &Path) -> Result<AuditChain, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AuditChain::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let whole = text.rfind('\n').map_or(0, |i| i + 1);
    if whole < text.len() {
        OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_len(whole as u64))
            .map_err(|e| format!("{}: cutting off a torn last line: {e}", path.display()))?;
    }
    let mut records = Vec::new();
    for (n, line) in text[..whole].lines().enumerate() {
        records.push(parse_line(line).ok_or_else(|| {
            format!(
                "{}: line {} is not an audit record; move the file aside to start a new chain",
                path.display(),
                n + 1
            )
        })?);
    }
    let chain = AuditChain::from_records(records);
    chain.verify().map_err(|e| {
        format!(
            "{}: the kept chain does not verify ({e:?}); move the file aside to start a new chain",
            path.display()
        )
    })?;
    Ok(chain)
}

/// Appends records to the file, in the order they are handed over. Call it
/// from the audit sink, which already holds the chain's lock, so the file
/// and the chain cannot disagree about order.
///
/// The file is only ever a prefix of the chain. If one write fails (a full
/// disk), nothing after it is written either: a file with a record missing
/// from the middle would fail to verify at the next boot, and a shorter
/// file that verifies is worth more than a longer one that does not.
pub struct Keeper(Mutex<Option<File>>);

impl Keeper {
    pub fn open(path: &Path) -> Result<Keeper, String> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|f| Keeper(Mutex::new(Some(f))))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// One line, written and flushed to the disk before this returns. A
    /// record the page has been shown is a record that survives.
    pub fn keep(&self, r: &AuditRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_string(r).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(f) = slot.as_mut() else {
            return Err(std::io::Error::other("an earlier record was not kept"));
        };
        let written = f.write_all(line.as_bytes()).and_then(|()| f.sync_data());
        if written.is_err() {
            *slot = None;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "portcullis-keep-{}-{name}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn kept(path: &Path, n: u64) -> AuditChain {
        let keeper = Keeper::open(path).unwrap();
        let mut chain = load(path).unwrap();
        for i in 0..n {
            let at = 100 + chain.len() as u64;
            let r = chain
                .append(at, "decision", format!("{{\"n\":{i}}}"))
                .clone();
            keeper.keep(&r).unwrap();
        }
        chain
    }

    #[test]
    fn a_kept_chain_resumes_from_its_own_head_across_a_restart() {
        let path = scratch("resume");
        let before = kept(&path, 3);
        let head = before.head();

        // A new process: same file, nothing in memory.
        let after = kept(&path, 2);
        assert_eq!(after.len(), 5);
        assert!(after.verify().is_ok());
        assert!(after.contains(&head), "yesterday's head is still in it");
        assert_eq!(
            after.records()[3].prev,
            head,
            "and the new records link to it"
        );
        assert_eq!(load(&path).unwrap(), after);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_edited_file_stops_the_server_instead_of_being_built_on() {
        let path = scratch("edited");
        kept(&path, 3);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen("\\\"n\\\":1", "\\\"n\\\":9", 1)).unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.contains("Tampered { at: 1 }"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_torn_last_line_is_cut_off_and_the_chain_carries_on() {
        let path = scratch("torn");
        kept(&path, 2);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":2,\"at\":10").unwrap();
        drop(f);

        let chain = kept(&path, 1);
        assert_eq!(chain.len(), 3);
        assert_eq!(load(&path).unwrap(), chain);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_is_an_empty_chain() {
        assert!(load(&scratch("missing")).unwrap().is_empty());
    }
}
