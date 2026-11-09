//! A stablecoin rail: ERC-20 transfers by EIP-3009 authorization, on an EVM
//! testnet. Design and the reasoning behind it: `docs/RAILS_EVM.md`.
//!
//! The two properties PayPal gives the executor, this rail gets from the token
//! contract itself:
//!
//! * **Exactly once.** Each payment is a `transferWithAuthorization` whose
//!   nonce is [`auth_nonce`] of the proposal id. The token refuses a nonce it
//!   has seen, so submitting the same payment again cannot pay again.
//! * **A definitive answer.** An authorization is valid only before its
//!   deadline. Unused at a finalized block whose time is past the deadline, it
//!   can never execute: that is a definite "not paid".
//!
//! Every answer about the past is read at the chain's `finalized` block, so a
//! reorganisation cannot turn "paid" into "not paid" after we have acted.
//!
//! Split like [`crate::paypal`]: the decisions are pure functions, tested
//! without a network; [`EvmRail`] is the HTTP shell around them.
//!
//! Testnet only, by construction: [`ALLOWED_CHAINS`] is a constant, checked
//! against the node before anything is signed.

use crate::rail::{ItemStatus, Observed, Rail, Submitted};
use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::Encodable2718;
use alloy_network::TxSignerSync;
use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall, SolEvent, SolValue};
use portcullis_kernel::{AssetId, Payment, ProposalId, RecipientKind};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Arbitrum Sepolia and Ethereum Sepolia. There is no configuration that
/// adds a chain; adding one is a code change, reviewed.
pub const ALLOWED_CHAINS: [u64; 2] = [421_614, 11_155_111];

/// How long a signed authorization stays valid. Long enough to be mined on a
/// slow day; short enough that an unknown outcome resolves the same hour.
pub const AUTH_WINDOW: u64 = 30 * 60;

sol! {
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
    event Transfer(address indexed from, address indexed to, uint256 value);
}

// ── pure ────────────────────────────────────────────────────────────────────

/// EIP-3009's type string. Checked against the token's own constant before the
/// first signature, so a token with a different struct is refused, not signed for.
pub fn typehash() -> B256 {
    keccak256(
        "TransferWithAuthorization(address from,address to,uint256 value,\
         uint256 validAfter,uint256 validBefore,bytes32 nonce)",
    )
}

/// One proposal, one nonce. Domain-separated so a Portcullis nonce cannot
/// collide with one some other system derived from the same string.
pub fn auth_nonce(proposal: &ProposalId) -> B256 {
    let mut b = b"portcullis:".to_vec();
    b.extend_from_slice(proposal.as_str().as_bytes());
    keccak256(b)
}

/// What the float wallet signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authorization {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub valid_after: U256,
    pub valid_before: U256,
    pub nonce: B256,
}

impl Authorization {
    /// The EIP-712 digest under the token's domain separator.
    pub fn digest(&self, domain: B256) -> B256 {
        let s = keccak256(
            (
                typehash(),
                self.from,
                self.to,
                self.value,
                self.valid_after,
                self.valid_before,
                self.nonce,
            )
                .abi_encode(),
        );
        let mut m = Vec::with_capacity(66);
        m.extend_from_slice(&[0x19, 0x01]);
        m.extend_from_slice(domain.as_slice());
        m.extend_from_slice(s.as_slice());
        keccak256(m)
    }

    /// Sign and encode the call. Signing is deterministic (RFC 6979), so the
    /// same authorization always produces the same calldata.
    pub fn calldata(&self, domain: B256, key: &PrivateKeySigner) -> Result<Bytes, String> {
        let sig = key
            .sign_hash_sync(&self.digest(domain))
            .map_err(|e| format!("signing: {e}"))?;
        let call = transferWithAuthorizationCall {
            from: self.from,
            to: self.to,
            value: self.value,
            validAfter: self.valid_after,
            validBefore: self.valid_before,
            nonce: self.nonce,
            v: 27 + u8::from(sig.v()),
            r: sig.r().into(),
            s: sig.s().into(),
        };
        Ok(call.abi_encode().into())
    }
}

/// Pure: what the finalized chain says about an authorization.
///
/// `used` is `authorizationState` at the finalized block; `transferred` is
/// whether a matching `Transfer(from, to, value)` log exists up to it.
pub fn classify(used: bool, transferred: bool, finalized_time: u64, valid_before: u64) -> Observed {
    match (used, transferred) {
        (true, true) => Observed::Status(ItemStatus::Success),
        // Used, but not for this transfer: cancelled, or something we do not
        // understand. Hold the money; a human looks.
        (true, false) => Observed::Unknown {
            reason: "anomaly: authorization used with no matching Transfer".into(),
        },
        // EIP-3009 requires block.timestamp < validBefore. Every block after a
        // finalized one is at least as late, so this can never execute now.
        (false, _) if finalized_time >= valid_before => Observed::Status(ItemStatus::Failed),
        (false, _) => Observed::Status(ItemStatus::Pending),
    }
}

/// Pure: a payment's amount as token base units, if the asset is this rail's.
pub fn token_value(p: &Payment, asset: &AssetId) -> Result<U256, String> {
    if p.amount.asset != *asset {
        return Err(format!(
            "this rail pays {} ({} decimals), not {} ({})",
            asset.symbol, asset.decimals, p.amount.asset.symbol, p.amount.asset.decimals
        ));
    }
    Ok(U256::from(p.amount.minor))
}

/// Pure: the payee as an address, if it is one.
pub fn payee(p: &Payment) -> Result<Address, String> {
    if p.to.kind() != RecipientKind::Evm {
        return Err(format!("{} is not an EVM address", p.to.as_str()));
    }
    p.to.as_str()
        .parse::<Address>()
        .map_err(|e| format!("unparseable address: {e}"))
}

// ── JSON-RPC ────────────────────────────────────────────────────────────────

/// Why a JSON-RPC call failed, split the way the executor needs it.
#[derive(Debug)]
pub enum RpcError {
    /// No answer, or one we could not read. Nothing can be concluded.
    Transport(String),
    /// The node answered with an error (a revert, a rejection).
    Node(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(e) => write!(f, "transport: {e}"),
            RpcError::Node(e) => write!(f, "node: {e}"),
        }
    }
}

struct Rpc {
    http: reqwest::Client,
    url: String,
}

impl Rpc {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let r = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        if !r.status().is_success() {
            return Err(RpcError::Transport(format!("HTTP {}", r.status())));
        }
        let v: Value = r
            .json()
            .await
            .map_err(|e| RpcError::Transport(format!("unreadable: {e}")))?;
        if let Some(e) = v.get("error") {
            let msg = e["message"].as_str().unwrap_or("error").to_string();
            let data = e["data"].as_str().unwrap_or_default();
            return Err(RpcError::Node(format!("{msg} {data}").trim().to_string()));
        }
        v.get("result")
            .cloned()
            .ok_or_else(|| RpcError::Transport("no result".into()))
    }

    async fn view<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: &str,
    ) -> Result<C::Return, RpcError> {
        let data = Bytes::from(call.abi_encode());
        let out = self
            .call("eth_call", json!([{ "to": to, "data": data }, block]))
            .await?;
        let hex = out.as_str().ok_or(RpcError::Transport("eth_call".into()))?;
        let bytes: Bytes = hex
            .parse()
            .map_err(|_| RpcError::Transport("eth_call: not hex".into()))?;
        C::abi_decode_returns(&bytes).map_err(|e| RpcError::Transport(format!("decode: {e}")))
    }

    async fn quantity(&self, method: &str, params: Value) -> Result<u64, RpcError> {
        let v = self.call(method, params).await?;
        hex_u64(&v).ok_or_else(|| RpcError::Transport(format!("{method}: not a quantity")))
    }

    /// Number and timestamp of a block by tag.
    async fn block(&self, tag: &str) -> Result<(u64, u64), RpcError> {
        let b = self
            .call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        match (hex_u64(&b["number"]), hex_u64(&b["timestamp"])) {
            (Some(n), Some(t)) => Ok((n, t)),
            _ => Err(RpcError::Transport(format!("block {tag}: unreadable"))),
        }
    }
}

fn hex_u64(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok()
}

// ── the rail ────────────────────────────────────────────────────────────────

pub struct EvmConfig {
    pub rpc_url: String,
    pub chain_id: u64,
    /// The token contract.
    pub token: Address,
    /// What the ledger calls the token. Its decimals must be the token's.
    pub asset: AssetId,
    /// Holds the float and signs authorizations. Never pays gas.
    pub float: PrivateKeySigner,
    /// Pays gas and broadcasts. Holds no token.
    pub relayer: PrivateKeySigner,
}

/// What we remember about a signed authorization, keyed by its nonce.
#[derive(Clone, Debug)]
struct Sent {
    to: Address,
    value: U256,
    valid_before: u64,
    /// Where to start looking for its Transfer log.
    from_block: u64,
    /// The signed call. Kept so a lost transaction can be sent again; it is
    /// the same bytes every time.
    data: Bytes,
    /// The relayer transaction that last carried it, if any was accepted.
    relay_tx: Option<B256>,
}

#[derive(Default)]
struct Memory {
    domain: Option<B256>,
    /// The deadline is fixed at the first attempt, so every retry signs the
    /// identical authorization.
    deadlines: HashMap<ProposalId, u64>,
    sent: HashMap<B256, Sent>,
}

pub struct EvmRail {
    cfg: EvmConfig,
    rpc: Rpc,
    memory: Mutex<Memory>,
}

impl EvmRail {
    pub fn new(cfg: EvmConfig) -> Result<Self, String> {
        if !ALLOWED_CHAINS.contains(&cfg.chain_id) {
            return Err(format!("chain {} is not an allowed testnet", cfg.chain_id));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(EvmRail {
            rpc: Rpc {
                http,
                url: cfg.rpc_url.clone(),
            },
            cfg,
            memory: Mutex::new(Memory::default()),
        })
    }

    pub fn float_address(&self) -> Address {
        self.cfg.float.address()
    }

    pub fn relayer_address(&self) -> Address {
        self.cfg.relayer.address()
    }

    fn mem(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The node is the chain we were told, and that chain is a testnet.
    async fn check_chain(&self) -> Result<(), Submitted> {
        match self.rpc.quantity("eth_chainId", json!([])).await {
            Ok(id) if id == self.cfg.chain_id && ALLOWED_CHAINS.contains(&id) => Ok(()),
            Ok(id) => Err(Submitted::Rejected {
                reason: format!("node is chain {id}, expected testnet {}", self.cfg.chain_id),
            }),
            Err(e) => Err(Submitted::Unknown {
                reason: e.to_string(),
            }),
        }
    }

    /// The token's EIP-712 domain, read once, after checking it signs the
    /// struct we think it does.
    async fn domain(&self) -> Result<B256, Submitted> {
        if let Some(d) = self.mem().domain {
            return Ok(d);
        }
        let unknown = |e: RpcError| Submitted::Unknown {
            reason: e.to_string(),
        };
        let theirs = self
            .rpc
            .view(
                self.cfg.token,
                TRANSFER_WITH_AUTHORIZATION_TYPEHASHCall {},
                "latest",
            )
            .await
            .map_err(unknown)?;
        if theirs != typehash() {
            return Err(Submitted::Rejected {
                reason: "token's authorization typehash is not EIP-3009's".into(),
            });
        }
        let d = self
            .rpc
            .view(self.cfg.token, DOMAIN_SEPARATORCall {}, "latest")
            .await
            .map_err(unknown)?;
        self.mem().domain = Some(d);
        Ok(d)
    }

    async fn used(&self, nonce: B256, block: &str) -> Result<bool, RpcError> {
        self.rpc
            .view(
                self.cfg.token,
                authorizationStateCall {
                    authorizer: self.float_address(),
                    nonce,
                },
                block,
            )
            .await
    }

    /// Things that make a payment definitely impossible, checked before
    /// signing, each with its own plain reason.
    async fn preflight(&self, to: Address, value: U256) -> Result<(), Submitted> {
        let unknown = |e: RpcError| Submitted::Unknown {
            reason: e.to_string(),
        };
        let t = self.cfg.token;
        if self
            .rpc
            .view(t, pausedCall {}, "latest")
            .await
            .map_err(unknown)?
        {
            return Err(Submitted::Rejected {
                reason: "the token is paused by its issuer".into(),
            });
        }
        for (who, addr) in [
            ("the float wallet", self.float_address()),
            ("the payee", to),
        ] {
            let frozen = self
                .rpc
                .view(t, isFrozenCall { account: addr }, "latest")
                .await
                .map_err(unknown)?;
            if frozen {
                return Err(Submitted::Rejected {
                    reason: format!("{who} is frozen by the token's issuer"),
                });
            }
        }
        let held = self
            .rpc
            .view(
                t,
                balanceOfCall {
                    account: self.float_address(),
                },
                "latest",
            )
            .await
            .map_err(unknown)?;
        if held < value {
            return Err(Submitted::Rejected {
                reason: format!("the float wallet holds {held}, the payment needs {value}"),
            });
        }
        Ok(())
    }

    /// Wrap the signed authorization in a transaction from the relayer and
    /// send it. Best effort: whatever happens here, the authorization's fate
    /// is settled by the chain, not by this call (see [`EvmRail::submit`]).
    async fn broadcast(&self, data: &Bytes) -> Result<B256, String> {
        let relayer = self.relayer_address();
        let unknown = |e: RpcError| e.to_string();
        let gas = self
            .rpc
            .quantity(
                "eth_estimateGas",
                json!([{ "from": relayer, "to": self.cfg.token, "data": data }]),
            )
            .await
            .map_err(|e| format!("not sent, the transfer would not go through now: {e}"))?;
        let fees = async {
            let tx_nonce = self
                .rpc
                .quantity("eth_getTransactionCount", json!([relayer, "pending"]))
                .await?;
            let tip = self
                .rpc
                .quantity("eth_maxPriorityFeePerGas", json!([]))
                .await?;
            let latest = self
                .rpc
                .call("eth_getBlockByNumber", json!(["latest", false]))
                .await?;
            let base = hex_u64(&latest["baseFeePerGas"]).unwrap_or(0);
            Ok::<_, RpcError>((tx_nonce, tip, base))
        };
        let (tx_nonce, tip, base) = fees.await.map_err(unknown)?;
        let mut tx = TxEip1559 {
            chain_id: self.cfg.chain_id,
            nonce: tx_nonce,
            gas_limit: gas.saturating_mul(12) / 10,
            max_fee_per_gas: u128::from(base) * 2 + u128::from(tip),
            max_priority_fee_per_gas: u128::from(tip),
            to: TxKind::Call(self.cfg.token),
            value: U256::ZERO,
            access_list: Default::default(),
            input: data.clone(),
        };
        let sig = self
            .cfg
            .relayer
            .sign_transaction_sync(&mut tx)
            .map_err(|e| format!("signing the relayer transaction: {e}"))?;
        let signed = tx.into_signed(sig);
        let hash = *signed.hash();
        let raw = TxEnvelope::from(signed).encoded_2718();
        self.rpc
            .call("eth_sendRawTransaction", json!([Bytes::from(raw)]))
            .await
            .map_err(unknown)?;
        Ok(hash)
    }

    /// Send again if nothing carrying the authorization is known to the node.
    /// The token's nonce makes a duplicate harmless; this only avoids paying
    /// gas for one.
    async fn resend_if_lost(&self, nonce: B256, sent: &Sent) -> Result<(), String> {
        if let Some(h) = sent.relay_tx {
            match self.rpc.call("eth_getTransactionByHash", json!([h])).await {
                Ok(Value::Null) => {}
                Ok(_) => return Ok(()),
                Err(e) => return Err(e.to_string()),
            }
        }
        let h = self.broadcast(&sent.data).await?;
        if let Some(s) = self.mem().sent.get_mut(&nonce) {
            s.relay_tx = Some(h);
        }
        Ok(())
    }
}

impl Rail for EvmRail {
    async fn submit(&self, payment: &Payment) -> Submitted {
        let (to, value) = match (payee(payment), token_value(payment, &self.cfg.asset)) {
            (Ok(to), Ok(v)) => (to, v),
            (Err(reason), _) | (_, Err(reason)) => return Submitted::Rejected { reason },
        };
        if let Err(s) = self.check_chain().await {
            return s;
        }
        let domain = match self.domain().await {
            Ok(d) => d,
            Err(s) => return s,
        };
        let nonce = auth_nonce(&payment.proposal);

        // An earlier attempt may have landed. If so, there is nothing to send.
        match self.used(nonce, "latest").await {
            Ok(true) => {
                return Submitted::Accepted {
                    batch_id: nonce.to_string(),
                }
            }
            Ok(false) => {}
            Err(e) => {
                return Submitted::Unknown {
                    reason: e.to_string(),
                }
            }
        }

        // Already signed by an earlier attempt: never refuse it now, never sign
        // anything different. Make sure it is on its way, and say so.
        let earlier = self.mem().sent.get(&nonce).cloned();
        if let Some(sent) = earlier {
            let _ = self.resend_if_lost(nonce, &sent).await;
            return Submitted::Accepted {
                batch_id: nonce.to_string(),
            };
        }

        let (head, now) = match self.rpc.block("latest").await {
            Ok(b) => b,
            Err(e) => {
                return Submitted::Unknown {
                    reason: e.to_string(),
                }
            }
        };
        let valid_before = *self
            .mem()
            .deadlines
            .entry(payment.proposal.clone())
            .or_insert(now + AUTH_WINDOW);

        // The only place this rail refuses: before anything is signed. Once a
        // signed authorization exists it has left our hands (a node simulating
        // it has seen it), so "no" can only come from the chain, at its
        // deadline. Refusing later would release money that could still move.
        if let Err(s) = self.preflight(to, value).await {
            return s;
        }

        let auth = Authorization {
            from: self.float_address(),
            to,
            value,
            valid_after: U256::ZERO,
            valid_before: U256::from(valid_before),
            nonce,
        };
        let data = match auth.calldata(domain, &self.cfg.float) {
            Ok(d) => d,
            // Signing itself failed: nothing exists outside this process.
            Err(reason) => return Submitted::Rejected { reason },
        };
        let sent = Sent {
            to,
            value,
            valid_before,
            from_block: head,
            data,
            relay_tx: None,
        };
        self.mem().sent.insert(nonce, sent.clone());
        // A failure to send is not a failure to pay or a refusal: observe()
        // sends again until the deadline, and the deadline decides.
        let _ = self.resend_if_lost(nonce, &sent).await;
        Submitted::Accepted {
            batch_id: nonce.to_string(),
        }
    }

    async fn observe(&self, batch_id: &str) -> Observed {
        let Ok(nonce) = batch_id.parse::<B256>() else {
            return Observed::Unknown {
                reason: format!("not an authorization nonce: {batch_id}"),
            };
        };
        let Some(sent) = self.mem().sent.get(&nonce).cloned() else {
            return Observed::Unknown {
                reason: "no record of this authorization (restarted?)".into(),
            };
        };
        let (fin_number, fin_time) = match self.rpc.block("finalized").await {
            Ok(b) => b,
            Err(e) => {
                return Observed::Unknown {
                    reason: e.to_string(),
                }
            }
        };
        let fin_tag = format!("0x{fin_number:x}");
        let used = match self.used(nonce, &fin_tag).await {
            Ok(u) => u,
            Err(e) => {
                return Observed::Unknown {
                    reason: e.to_string(),
                }
            }
        };
        let transferred = if used {
            let topic = |a: Address| B256::left_padding_from(a.as_slice());
            let logs = self
                .rpc
                .call(
                    "eth_getLogs",
                    json!([{
                        "address": self.cfg.token,
                        "fromBlock": format!("0x{:x}", sent.from_block),
                        "toBlock": fin_tag,
                        "topics": [Transfer::SIGNATURE_HASH, topic(self.float_address()), topic(sent.to)],
                    }]),
                )
                .await;
            match logs {
                Ok(Value::Array(ls)) => ls.iter().any(|l| {
                    l["data"]
                        .as_str()
                        .and_then(|d| d.parse::<U256>().ok())
                        .is_some_and(|v| v == sent.value)
                }),
                Ok(_) => false,
                Err(e) => {
                    return Observed::Unknown {
                        reason: e.to_string(),
                    }
                }
            }
        } else {
            false
        };
        let verdict = classify(used, transferred, fin_time, sent.valid_before);
        // Still open: if the chain has not seen it at all, it may have been
        // lost on the way. Send it again; the deadline still decides.
        if verdict == Observed::Status(ItemStatus::Pending) {
            match self.used(nonce, "latest").await {
                Ok(true) => {}
                Ok(false) => {
                    if let Err(reason) = self.resend_if_lost(nonce, &sent).await {
                        return Observed::Unknown {
                            reason: format!("open until its deadline; {reason}"),
                        };
                    }
                }
                Err(e) => {
                    return Observed::Unknown {
                        reason: e.to_string(),
                    }
                }
            }
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Signature;
    use portcullis_kernel::{Amount, RecipientId};

    fn pyusd() -> AssetId {
        AssetId::new("PYUSD", 6)
    }

    fn payment(to: &str, minor: u64, asset: AssetId) -> Payment {
        Payment {
            proposal: ProposalId::new("p-1"),
            to: RecipientId::parse(to).unwrap(),
            amount: Amount::new(asset, minor),
        }
    }

    #[test]
    fn the_typehash_is_eip_3009s() {
        // keccak256 of the EIP-3009 type string, as published in the EIP and
        // returned by USDC's and PYUSD's TRANSFER_WITH_AUTHORIZATION_TYPEHASH.
        assert_eq!(
            typehash().to_string(),
            "0x7c7c6cdb67a18743f49ec6fa9b35f50d52ed05cbed4cc592e13b44501c1a2267"
        );
    }

    #[test]
    fn one_proposal_one_nonce() {
        let a = auth_nonce(&ProposalId::new("p-1"));
        assert_eq!(a, auth_nonce(&ProposalId::new("p-1")));
        assert_ne!(a, auth_nonce(&ProposalId::new("p-2")));
        assert_ne!(a, keccak256("p-1"), "domain-separated from a bare hash");
    }

    #[test]
    fn the_signature_recovers_to_the_float_and_is_deterministic() {
        let key = PrivateKeySigner::random();
        let auth = Authorization {
            from: key.address(),
            to: Address::repeat_byte(0x11),
            value: U256::from(1_284_600_000u64),
            valid_after: U256::ZERO,
            valid_before: U256::from(2_000_000_000u64),
            nonce: auth_nonce(&ProposalId::new("p-1")),
        };
        let domain = B256::repeat_byte(0xd0);
        let data = auth.calldata(domain, &key).unwrap();
        assert_eq!(
            data,
            auth.calldata(domain, &key).unwrap(),
            "same bytes on retry"
        );
        let call = transferWithAuthorizationCall::abi_decode(&data).unwrap();
        let sig = Signature::new(call.r.into(), call.s.into(), call.v == 28);
        let who = sig
            .recover_address_from_prehash(&auth.digest(domain))
            .unwrap();
        assert_eq!(who, key.address());
        assert_eq!(call.to, auth.to);
        assert_eq!(call.value, auth.value);
        // A different domain (another token, another chain) is a different digest.
        assert_ne!(auth.digest(domain), auth.digest(B256::repeat_byte(0xd1)));
    }

    #[test]
    fn classify_names_each_case() {
        use ItemStatus::*;
        let st = |o: Observed| match o {
            Observed::Status(s) => Some(s),
            Observed::Unknown { .. } => None,
        };
        assert_eq!(st(classify(true, true, 100, 200)), Some(Success));
        assert_eq!(st(classify(true, false, 100, 200)), None, "anomaly holds");
        assert_eq!(st(classify(false, false, 199, 200)), Some(Pending));
        // block.timestamp < validBefore is required: equal is already too late.
        assert_eq!(st(classify(false, false, 200, 200)), Some(Failed));
        assert_eq!(st(classify(false, false, 500, 200)), Some(Failed));
    }

    #[test]
    fn only_this_rails_asset_and_only_evm_payees() {
        let to = "0x1111111111111111111111111111111111111111";
        assert!(token_value(&payment(to, 5, pyusd()), &pyusd()).is_ok());
        // Same symbol, different decimals, is a different asset.
        assert!(token_value(&payment(to, 5, AssetId::new("PYUSD", 2)), &pyusd()).is_err());
        assert!(token_value(&payment(to, 5, AssetId::usd()), &pyusd()).is_err());
        assert!(payee(&payment("vendor@example.com", 5, pyusd())).is_err());
        assert_eq!(
            payee(&payment(to, 5, pyusd())).unwrap(),
            Address::repeat_byte(0x11)
        );
    }

    #[test]
    fn mainnet_is_refused_at_construction() {
        let cfg = |chain_id| EvmConfig {
            rpc_url: "http://127.0.0.1:1".into(),
            chain_id,
            token: Address::ZERO,
            asset: pyusd(),
            float: PrivateKeySigner::random(),
            relayer: PrivateKeySigner::random(),
        };
        assert!(EvmRail::new(cfg(1)).is_err(), "Ethereum mainnet");
        assert!(EvmRail::new(cfg(42_161)).is_err(), "Arbitrum One");
        assert!(EvmRail::new(cfg(421_614)).is_ok());
    }
}
