//! Make (or reuse) the testnet wallets for a live run of the stablecoin rail,
//! and print their addresses. Never prints a key. `float` signs and holds the
//! token, `relayer` pays gas, `vendor` is the payee (a key we hold, so test
//! tokens paid to it are not stranded).
//!
//! ```text
//! cargo run -p portcullis-executor --example evm_keys -- <dir>
//! ```
//!
//! Writes `<dir>/{float,relayer,vendor}.key` (hex, mode 0600, in a 0700
//! directory) if they do not already exist. These are for testnets only: a
//! key generated on a build machine is not a key that should ever hold value.

use alloy_signer_local::PrivateKeySigner;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

fn load_or_make(path: &Path) -> PrivateKeySigner {
    if let Ok(hex) = fs::read_to_string(path) {
        return hex.trim().parse().expect("a readable key");
    }
    let key = PrivateKeySigner::random();
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("a new key file");
    f.write_all(alloy_primitives::hex::encode(key.to_bytes()).as_bytes())
        .expect("written");
    key
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: evm_keys <dir>");
    let dir = Path::new(&dir);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .expect("a private directory");
    let float = load_or_make(&dir.join("float.key"));
    let relayer = load_or_make(&dir.join("relayer.key"));
    let vendor = load_or_make(&dir.join("vendor.key"));
    println!("float   {}", float.address());
    println!("relayer {}", relayer.address());
    println!("vendor  {}", vendor.address());
}
