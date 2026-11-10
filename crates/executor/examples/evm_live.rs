//! One real payment on a testnet, through the kernel and the stablecoin rail,
//! read back from the chain. Then the same payment is tried again, and must
//! not move anything.
//!
//! ```text
//! cargo run -p portcullis-executor --example evm_live -- <key dir> <rpc url> <chain id> <token> <amount>
//! ```
//!
//! Keys come from `evm_keys`. Nothing here prints a key.

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use portcullis_executor::evm::{EvmConfig, EvmRail};
use portcullis_executor::{ItemStatus, Observed, Rail, Submitted};
use portcullis_kernel::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn key(dir: &str, name: &str) -> PrivateKeySigner {
    std::fs::read_to_string(format!("{dir}/{name}.key"))
        .expect("run evm_keys first")
        .trim()
        .parse()
        .expect("a key")
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dir, rpc, chain, token, amount) = (&a[1], &a[2], a[3].parse().unwrap(), &a[4], &a[5]);
    let token: Address = token.parse().unwrap();
    let pyusd = AssetId::new("PYUSD", 6);
    let minor: u64 = {
        let (w, f) = amount.split_once('.').unwrap_or((amount, ""));
        w.parse::<u64>().unwrap() * 1_000_000 + format!("{f:0<6}")[..6].parse::<u64>().unwrap()
    };
    let float = key(dir, "float");
    let relayer = key(dir, "relayer");
    let vendor = key(dir, "vendor").address();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // The gate decides first: a PYUSD ledger, a policy that knows the vendor.
    let mut ledger = Ledger::new(pyusd.clone(), 100_000_000);
    let policy = Policy {
        standing: vec![StandingRecipient {
            recipient: RecipientId::parse(&vendor.to_string()).unwrap(),
            added_at: 0,
            active_from: 0,
        }],
        rules: vec![Rule::PerTransactionMax { minor: 50_000_000 }],
        version: 1,
        head: [0; 32],
    };
    let proposal = Proposal {
        id: ProposalId::new(format!("live-{now}")),
        agent: AgentId::new("live-run"),
        recipient: vendor.to_string(),
        amount: Amount::new(pyusd.clone(), minor),
        proposed_at: now,
        against: None,
        purpose: "Phase 2: one real payment on a testnet".into(),
    };
    let adj = adjudicate(&mut ledger, &policy, &proposal, now);
    println!("kernel: {:?}", adj.decision());
    let payment = adj.payment().cloned().expect("the gate allowed it");

    let rail = EvmRail::new(EvmConfig {
        rpc_url: rpc.clone(),
        chain_id: chain,
        token,
        asset: pyusd,
        float,
        relayer,
    })
    .expect("a testnet");

    let batch = match rail.submit(&payment).await {
        Submitted::Accepted { batch_id } => batch_id,
        other => {
            println!("submit: {other:?}");
            return;
        }
    };
    println!("submitted: authorization nonce {batch}");
    if let Some(tx) = rail.relay_tx(&batch) {
        println!("relay tx:  https://sepolia.etherscan.io/tx/{tx}");
    }

    let started = std::time::Instant::now();
    loop {
        let o = rail.observe(&batch).await;
        println!("[{:>4}s] {o:?}", started.elapsed().as_secs());
        match o {
            Observed::Status(ItemStatus::Success) => break,
            Observed::Status(ItemStatus::Failed) => {
                println!("not paid: the deadline passed unused");
                return;
            }
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(45 * 60) {
            println!("gave up waiting; nothing is lost, the deadline still decides");
            return;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }

    // The same payment again: same authorization, nothing more moves.
    match rail.submit(&payment).await {
        Submitted::Accepted { batch_id } => {
            assert_eq!(batch_id, batch, "same authorization");
            println!("resubmitted: same batch {batch_id}, nothing new signed")
        }
        other => println!("resubmit: {other:?}"),
    }
}
