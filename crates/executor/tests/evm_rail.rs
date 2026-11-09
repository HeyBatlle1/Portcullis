//! The stablecoin rail against a fake chain that behaves like the token.
//!
//! The fake is written independently of the rail: it hashes the EIP-3009
//! struct with alloy's own EIP-712 implementation, recovers the signer, and
//! enforces what PYUSD enforces (signature, unused nonce, deadline, balance,
//! pause, freeze). So a test passing here means the rail produced something
//! the real token would accept, not that the rail agrees with itself.
//!
//! What it pins down:
//! * one payment, one transfer, however many times it is submitted;
//! * settled only once the transfer is final, never at "latest";
//! * once signed, never refused: a failed simulation or a lost transaction is
//!   not a "no", and the money stays held until the chain says;
//! * a lost transaction is sent again, and the same bytes go out;
//! * "not paid" only after the deadline has passed at a finalized block;
//! * refusals happen only before anything is signed, and leak nothing;
//! * an authorization used without its transfer holds the money for a human.

use alloy_consensus::TxEnvelope;
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{keccak256, Address, Signature, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall, SolStruct, SolValue};
use axum::{extract::State, routing::post, Json, Router};
use portcullis_executor::evm::{EvmConfig, EvmRail};
use portcullis_executor::{ItemStatus, Observed, Rail, Submitted};
use portcullis_kernel::{Amount, AssetId, Payment, ProposalId, RecipientId};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const CHAIN: u64 = 421_614;
const DOMAIN: B256 = B256::repeat_byte(0xd0);
const PAYEE: &str = "0x1111111111111111111111111111111111111111";
const VALUE: u64 = 1_284_600_000; // 1,284.60 PYUSD

sol! {
    struct TransferWithAuthorization {
        address from; address to; uint256 value;
        uint256 validAfter; uint256 validBefore; bytes32 nonce;
    }
    function transferWithAuthorization(
        address from, address to, uint256 value,
        uint256 validAfter, uint256 validBefore, bytes32 nonce,
        uint8 v, bytes32 r, bytes32 s
    );
    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
    function balanceOf(address account) external view returns (uint256);
    function isFrozen(address account) external view returns (bool);
    function paused() external view returns (bool);
    function DOMAIN_SEPARATOR() external view returns (bytes32);
    function TRANSFER_WITH_AUTHORIZATION_TYPEHASH() external view returns (bytes32);
}

#[derive(Clone, Copy, PartialEq)]
enum Send {
    /// Mined into the next block.
    Mine,
    /// Accepted by the node, then lost: never mined, never known.
    Drop,
}

struct Chain {
    chain_id: u64,
    token: Address,
    latest: (u64, u64),
    finalized: (u64, u64),
    balances: HashMap<Address, U256>,
    /// nonce -> block it was used in
    used: HashMap<(Address, B256), u64>,
    /// (block, from, to, value)
    transfers: Vec<(u64, Address, Address, U256)>,
    txs: HashMap<B256, u64>,
    send: Send,
    paused: bool,
    frozen: Vec<Address>,
    revert_estimates: bool,
    /// Every call that carried a signed authorization to the node.
    disclosed: Vec<Vec<u8>>,
}

type Fake = Arc<Mutex<Chain>>;

impl Chain {
    fn block_number(&self, tag: &Value) -> u64 {
        match tag.as_str() {
            Some("latest") | Some("pending") | None => self.latest.0,
            Some("finalized") => self.finalized.0,
            Some(h) => u64::from_str_radix(h.trim_start_matches("0x"), 16).unwrap(),
        }
    }

    fn mine(&mut self, secs: u64) {
        self.latest = (self.latest.0 + 1, self.latest.1 + secs);
    }

    /// The token's checks, in PYUSD's order of concern. Returns the revert reason.
    fn check(&self, c: &transferWithAuthorizationCall, at_time: u64) -> Result<(), &'static str> {
        if self.paused {
            return Err("ContractPaused");
        }
        if self.frozen.contains(&c.from) || self.frozen.contains(&c.to) {
            return Err("AddressFrozen");
        }
        if U256::from(at_time) >= c.validBefore || U256::from(at_time) <= c.validAfter {
            return Err("AuthorizationExpired");
        }
        if self.used.contains_key(&(c.from, c.nonce)) {
            return Err("AuthorizationAlreadyUsed");
        }
        let st = TransferWithAuthorization {
            from: c.from,
            to: c.to,
            value: c.value,
            validAfter: c.validAfter,
            validBefore: c.validBefore,
            nonce: c.nonce,
        };
        let mut m = vec![0x19, 0x01];
        m.extend_from_slice(DOMAIN.as_slice());
        m.extend_from_slice(st.eip712_hash_struct().as_slice());
        let sig = Signature::new(c.r.into(), c.s.into(), c.v == 28);
        match sig.recover_address_from_prehash(&keccak256(m)) {
            Ok(a) if a == c.from => {}
            _ => return Err("InvalidSignature"),
        }
        if self.balances.get(&c.from).copied().unwrap_or_default() < c.value {
            return Err("InsufficientFunds");
        }
        Ok(())
    }

    fn rpc(&mut self, method: &str, p: &Value) -> Result<Value, String> {
        let hex = |n: u64| json!(format!("0x{n:x}"));
        Ok(match method {
            "eth_chainId" => hex(self.chain_id),
            "eth_getBlockByNumber" => {
                let n = self.block_number(&p[0]);
                // Only two blocks are ever asked for by number: the head and the
                // finalized one.
                let t = if n >= self.latest.0 {
                    self.latest.1
                } else {
                    self.finalized.1
                };
                json!({ "number": hex(n), "timestamp": hex(t), "baseFeePerGas": "0x1" })
            }
            "eth_getTransactionCount" => hex(self.txs.len() as u64),
            "eth_maxPriorityFeePerGas" => hex(1),
            "eth_call" => {
                let data: alloy_primitives::Bytes = p[0]["data"].as_str().unwrap().parse().unwrap();
                let at = self.block_number(&p[1]);
                let sel: [u8; 4] = data[..4].try_into().unwrap();
                let out: Vec<u8> = if sel == TRANSFER_WITH_AUTHORIZATION_TYPEHASHCall::SELECTOR {
                    keccak256(
                        "TransferWithAuthorization(address from,address to,uint256 value,\
                         uint256 validAfter,uint256 validBefore,bytes32 nonce)",
                    )
                    .abi_encode()
                } else if sel == DOMAIN_SEPARATORCall::SELECTOR {
                    DOMAIN.abi_encode()
                } else if sel == pausedCall::SELECTOR {
                    self.paused.abi_encode()
                } else if sel == isFrozenCall::SELECTOR {
                    let c = isFrozenCall::abi_decode(&data).unwrap();
                    self.frozen.contains(&c.account).abi_encode()
                } else if sel == balanceOfCall::SELECTOR {
                    let c = balanceOfCall::abi_decode(&data).unwrap();
                    self.balances
                        .get(&c.account)
                        .copied()
                        .unwrap_or_default()
                        .abi_encode()
                } else if sel == authorizationStateCall::SELECTOR {
                    let c = authorizationStateCall::abi_decode(&data).unwrap();
                    self.used
                        .get(&(c.authorizer, c.nonce))
                        .is_some_and(|b| *b <= at)
                        .abi_encode()
                } else {
                    return Err("unknown selector".into());
                };
                json!(format!("0x{}", alloy_primitives::hex::encode(out)))
            }
            "eth_estimateGas" => {
                let data: alloy_primitives::Bytes = p[0]["data"].as_str().unwrap().parse().unwrap();
                self.disclosed.push(data.to_vec());
                let c = transferWithAuthorizationCall::abi_decode(&data).unwrap();
                if self.revert_estimates {
                    return Err("execution reverted: simulated".into());
                }
                self.check(&c, self.latest.1 + 1)
                    .map_err(|e| format!("execution reverted: {e}"))?;
                hex(90_000)
            }
            "eth_sendRawTransaction" => {
                let raw: alloy_primitives::Bytes = p[0].as_str().unwrap().parse().unwrap();
                let env = TxEnvelope::decode_2718(&mut raw.as_ref()).unwrap();
                let TxEnvelope::Eip1559(signed) = env else {
                    return Err("not 1559".into());
                };
                assert_eq!(signed.tx().chain_id, self.chain_id, "signed for this chain");
                assert_eq!(signed.tx().to.to().copied(), Some(self.token));
                let data = signed.tx().input.clone();
                self.disclosed.push(data.to_vec());
                let hash = *signed.hash();
                if self.send == Send::Drop {
                    return Ok(json!(hash));
                }
                let c = transferWithAuthorizationCall::abi_decode(&data).unwrap();
                self.mine(2);
                self.txs.insert(hash, self.latest.0);
                // Mined either way; a revert changes nothing on the token.
                if self.check(&c, self.latest.1).is_ok() {
                    *self.balances.entry(c.from).or_default() -= c.value;
                    *self.balances.entry(c.to).or_default() += c.value;
                    self.used.insert((c.from, c.nonce), self.latest.0);
                    self.transfers.push((self.latest.0, c.from, c.to, c.value));
                }
                json!(hash)
            }
            "eth_getTransactionByHash" => {
                let h: B256 = p[0].as_str().unwrap().parse().unwrap();
                if self.txs.contains_key(&h) {
                    json!({ "hash": h })
                } else {
                    Value::Null
                }
            }
            "eth_getLogs" => {
                let f = &p[0];
                let from = self.block_number(&f["fromBlock"]);
                let to = self.block_number(&f["toBlock"]);
                let topic = |i: usize| -> Option<Address> {
                    f["topics"][i]
                        .as_str()
                        .map(|t| Address::from_word(t.parse::<B256>().unwrap()))
                };
                let logs: Vec<Value> = self
                    .transfers
                    .iter()
                    .filter(|(b, f2, t2, _)| {
                        *b >= from
                            && *b <= to
                            && topic(1).is_none_or(|a| a == *f2)
                            && topic(2).is_none_or(|a| a == *t2)
                    })
                    .map(|(b, _, _, v)| {
                        json!({ "blockNumber": format!("0x{b:x}"),
                                "data": format!("0x{}", alloy_primitives::hex::encode(v.abi_encode())) })
                    })
                    .collect();
                json!(logs)
            }
            m => return Err(format!("unexpected method {m}")),
        })
    }
}

async fn serve(fake: Fake) -> String {
    async fn handle(State(f): State<Fake>, Json(req): Json<Value>) -> Json<Value> {
        let method = req["method"].as_str().unwrap_or_default().to_string();
        let r = f.lock().unwrap().rpc(&method, &req["params"]);
        Json(match r {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": 1, "result": v }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": 3, "message": e } }),
        })
    }
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    let app = Router::new().route("/", post(handle)).with_state(fake);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{a}/")
}

struct Rig {
    fake: Fake,
    rail: EvmRail,
    float: Address,
    payee: Address,
}

async fn rig() -> Rig {
    let float = PrivateKeySigner::random();
    let relayer = PrivateKeySigner::random();
    let token = Address::repeat_byte(0x70);
    let mut balances = HashMap::new();
    balances.insert(float.address(), U256::from(5_000_000_000u64)); // 5,000 PYUSD
    let fake = Arc::new(Mutex::new(Chain {
        chain_id: CHAIN,
        token,
        latest: (1_000, 1_800_000_000),
        finalized: (990, 1_799_999_980),
        balances,
        used: HashMap::new(),
        transfers: vec![],
        txs: HashMap::new(),
        send: Send::Mine,
        paused: false,
        frozen: vec![],
        revert_estimates: false,
        disclosed: vec![],
    }));
    let url = serve(fake.clone()).await;
    let float_addr = float.address();
    let rail = EvmRail::new(EvmConfig {
        rpc_url: url,
        chain_id: CHAIN,
        token,
        asset: AssetId::new("PYUSD", 6),
        float,
        relayer,
    })
    .unwrap();
    Rig {
        fake,
        rail,
        float: float_addr,
        payee: PAYEE.parse().unwrap(),
    }
}

fn payment(id: &str) -> Payment {
    Payment {
        proposal: ProposalId::new(id),
        to: RecipientId::parse(PAYEE).unwrap(),
        amount: Amount::new(AssetId::new("PYUSD", 6), VALUE),
    }
}

fn accepted(s: Submitted) -> String {
    match s {
        Submitted::Accepted { batch_id } => batch_id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

impl Rig {
    fn finalize(&self) {
        let mut c = self.fake.lock().unwrap();
        c.finalized = c.latest;
    }
    /// Let time pass on the chain: a block `secs` later.
    fn advance(&self, secs: u64) {
        self.fake.lock().unwrap().mine(secs);
    }
    fn transfers(&self) -> usize {
        self.fake.lock().unwrap().transfers.len()
    }
}

#[tokio::test]
async fn pays_once_and_settles_only_when_final() {
    let r = rig().await;
    let p = payment("p-1");
    let batch = accepted(r.rail.submit(&p).await);
    assert_eq!(r.transfers(), 1);

    // Mined, but not final: not settled.
    assert_eq!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Pending)
    );
    r.finalize();
    assert_eq!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Success)
    );

    // Submitted again (a retry, a restart of the drive loop): same batch,
    // still one transfer.
    assert_eq!(accepted(r.rail.submit(&p).await), batch);
    assert_eq!(r.transfers(), 1);
    let c = r.fake.lock().unwrap();
    assert_eq!(c.balances[&r.payee], U256::from(VALUE));
    assert_eq!(c.balances[&r.float], U256::from(5_000_000_000u64 - VALUE));
}

#[tokio::test]
async fn a_lost_transaction_is_sent_again_with_the_same_bytes() {
    let r = rig().await;
    r.fake.lock().unwrap().send = Send::Drop;
    let batch = accepted(r.rail.submit(&payment("p-2")).await);
    assert_eq!(r.transfers(), 0, "lost on the way");

    r.fake.lock().unwrap().send = Send::Mine;
    assert_eq!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Pending)
    );
    assert_eq!(r.transfers(), 1, "observe sent it again");
    r.finalize();
    assert_eq!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Success)
    );

    let c = r.fake.lock().unwrap();
    let sends: Vec<_> = c.disclosed.iter().collect();
    assert!(
        sends.windows(2).all(|w| w[0] == w[1]),
        "one authorization, same bytes every time"
    );
}

#[tokio::test]
async fn once_signed_a_failed_simulation_is_not_a_refusal() {
    // The node has seen the signed authorization when it simulated it. Calling
    // that a refusal would release money the authorization can still move.
    let r = rig().await;
    r.fake.lock().unwrap().revert_estimates = true;
    let batch = accepted(r.rail.submit(&payment("p-3")).await);
    assert!(
        !r.fake.lock().unwrap().disclosed.is_empty(),
        "the node saw it"
    );
    assert_eq!(r.transfers(), 0);

    // Still open before the deadline, whatever the simulation said.
    r.finalize();
    assert!(!matches!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Failed)
    ));
}

#[tokio::test]
async fn not_paid_only_after_the_deadline_is_final() {
    let r = rig().await;
    r.fake.lock().unwrap().send = Send::Drop;
    let batch = accepted(r.rail.submit(&payment("p-4")).await);

    // Past the deadline at "latest", but not yet at "finalized": still open.
    r.advance(31 * 60);
    assert!(!matches!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Failed)
    ));
    // Final and past the deadline: it can never execute. Release.
    r.finalize();
    assert_eq!(
        r.rail.observe(&batch).await,
        Observed::Status(ItemStatus::Failed)
    );

    // And the token agrees: the same authorization, sent now, moves nothing.
    let data = r.fake.lock().unwrap().disclosed[0].clone();
    let c = transferWithAuthorizationCall::abi_decode(&data).unwrap();
    let ch = r.fake.lock().unwrap();
    assert_eq!(ch.check(&c, ch.latest.1), Err("AuthorizationExpired"));
}

#[tokio::test]
async fn refusals_come_only_before_anything_is_signed() {
    let refused = |s: Submitted, why: &str| match s {
        Submitted::Rejected { reason } => assert!(reason.contains(why), "{reason}"),
        other => panic!("expected Rejected ({why}), got {other:?}"),
    };

    let r = rig().await;
    r.fake.lock().unwrap().paused = true;
    refused(r.rail.submit(&payment("p-5")).await, "paused");
    assert!(r.fake.lock().unwrap().disclosed.is_empty());

    let r = rig().await;
    let payee = r.payee;
    r.fake.lock().unwrap().frozen.push(payee);
    refused(r.rail.submit(&payment("p-6")).await, "frozen");
    assert!(r.fake.lock().unwrap().disclosed.is_empty());

    let r = rig().await;
    let float = r.float;
    r.fake
        .lock()
        .unwrap()
        .balances
        .insert(float, U256::from(1u64));
    refused(r.rail.submit(&payment("p-7")).await, "float wallet holds");
    assert!(r.fake.lock().unwrap().disclosed.is_empty());

    let r = rig().await;
    r.fake.lock().unwrap().chain_id = 1;
    refused(r.rail.submit(&payment("p-8")).await, "node is chain 1");
    assert!(
        r.fake.lock().unwrap().disclosed.is_empty(),
        "nothing signed for mainnet"
    );
}

#[tokio::test]
async fn used_without_its_transfer_holds_the_money_for_a_human() {
    let r = rig().await;
    r.fake.lock().unwrap().send = Send::Drop;
    let p = payment("p-9");
    let batch = accepted(r.rail.submit(&p).await);
    // Cancelled out from under us (cancelAuthorization), or something stranger.
    {
        let mut c = r.fake.lock().unwrap();
        c.mine(2);
        let at = c.latest.0;
        let nonce: B256 = batch.parse().unwrap();
        c.used.insert((r.float, nonce), at);
    }
    r.finalize();
    match r.rail.observe(&batch).await {
        Observed::Unknown { reason } => assert!(reason.contains("anomaly"), "{reason}"),
        other => panic!("expected a hold, got {other:?}"),
    }
}
