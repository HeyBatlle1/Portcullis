# Stablecoin rail: design (branch `rails/evm`)

Status: **phase 2 done: one live testnet payment, settled at finality.**
Nothing here is on `main` or in the hosted demo.
Testnet only, by construction, as the PayPal rail is sandbox only.

## Why

Portcullis's claim is that the gate does not care which model is asking. The
same should hold for what the money moves on. The kernel never sees a rail: it
judges an asset, an amount, a payee and a policy. PayPal Payouts is one
implementation of [`Rail`](../crates/executor/src/rail.rs), two functions,
`submit` and `observe`. This adds a second: a stablecoin transfer on an EVM
chain, starting with PayPal USD (PYUSD) on Arbitrum Sepolia.

## What does not change

- **The kernel.** It already parses `0x` addresses (`RecipientKind::Evm`,
  canonical lowercase) and assets of any decimals (`AssetId`, PYUSD is 6). Its
  dependencies stay `serde` + `sha2`.
- **One ledger, one asset.** `AssetMismatch` stays. USD and PYUSD are separate
  ledgers with separate policies. Nothing ever converts one into the other.
  Supporting both means one `Gatehouse` per (account, asset), and the MCP layer
  routing `propose_payment` by its `currency`.
- **Only a definitive answer releases money.** The rail must say paid, or
  definitely not paid. Anything else holds the reservation.

## Verified facts (2026-11-09, read from the chain)

| | Arbitrum Sepolia | Ethereum Sepolia |
|---|---|---|
| chain id | `421614` (`0x66eee`) | `11155111` (`0xaa36a7`) |
| RPC used | `sepolia-rollup.arbitrum.io/rpc` | `ethereum-sepolia-rpc.publicnode.com` |
| PYUSD (Paxos testnet) | `0x637A1259C6afd7E3AdF63993cA7E58BB438aB1B1` | `0xCaC524BcA292aaade2DF8A05cC58F0a65B1B3bB9` |
| `symbol()` | `PYUSD` | `PYUSD` |
| `decimals()` | `6` | `6` |
| `authorizationState(address,bytes32)` | answers (EIP-3009 present) | answers; used live |
| `finalized` behind `latest` | about 17 min | about 14 to 17 min |

The PYUSD contract (paxosglobal/pyusd-contract, `PYUSD.abi`) implements
EIP-3009: `transferWithAuthorization` (both the `v,r,s` and `bytes signature`
forms), `receiveWithAuthorization`, `cancelAuthorization`, and
`authorizationState`. It also implements issuer controls: `freeze`, `pause`,
`wipeFrozenAddress`. See *Honest limits*.

## The design

```
 agent ─ propose_payment(currency: PYUSD) ─► Portcullis MCP ─ routes by asset ─► PYUSD Gatehouse
                                                                                    │ Allow
 owner's Safe (hardware key) ── tops up ──► float wallet (executor's signing key) ◄─┘
                                                 │ signs an EIP-3009 authorization
                                                 ▼
                                   relayer (gas only) ── broadcasts ──► chain
```

### 1. Exactly-once, enforced by the token

Each payment is an EIP-3009 `transferWithAuthorization` signed by the float
wallet, with

- `nonce = keccak256("portcullis:" || proposal_id)`. One proposal, one nonce.
  The token refuses a nonce twice, so a resend after a timeout cannot pay twice.
  This is PayPal's `PayPal-Request-Id`, except the chain enforces it.
- `validAfter = 0`, `validBefore = decided_at + 30 min`.
- `to` and `value` from the kernel's `Payment`: the canonical recipient and the
  approved minor units, never the agent's spelling.

### 2. A definitive answer always arrives

| Observation | Meaning | Executor |
|---|---|---|
| A finalized receipt, status 1, with a `Transfer(float, to, value)` log matching the `Payment` | paid | settle |
| `authorizationState(float, nonce) == true` and a matching `Transfer` found | paid (we lost the receipt) | settle |
| `authorizationState == true` and no matching `Transfer` | the authorization was cancelled or used otherwise: **anomaly** | hold for a human |
| `authorizationState == false` at a finalized block whose time is past `validBefore` | definitely never paid | release |
| `authorizationState == false`, deadline not yet past at a finalized block | open | wait; send again if the node has lost it |
| RPC error, timeout, or none of the above | unknown | change nothing, ask again |

The deadline is what turns "unknown" into "no". PayPal can leave a payout
pending indefinitely; a signed authorization dies on schedule.

**Once signed, never refused.** A reverted transaction, a failed simulation, a
transaction the node lost: none of these is "not paid". The signed
authorization has left our hands (a node that simulated it has seen it), and
anyone holding it can submit it until its deadline. Releasing the money early
would let a payment the gate already gave back go through anyway. So the rail
refuses only *before* signing (a paused token, a frozen payee or float, a float
too small, a node on the wrong chain), and after signing the answer comes
from the chain alone. Lost transactions are sent again, the same bytes, until
the deadline.

On Arbitrum Sepolia the `finalized` block trailed the newest by about 17
minutes when this was written, so a settlement takes about that long.

"Finalized" means the chain's `finalized` block tag, not a confirmation count
picked by hand.

### 3. Two fences

The executor's key controls only a **float wallet** holding at most one day's
spending (the policy's rolling-window maximum). The treasury sits in the owner's
Safe, behind a hardware key; topping up the float is a human act, like funding
the PayPal account. A server compromised outright loses at most the float.

The float only signs. A separate **relayer** wallet holds only gas and submits
the signed authorization. Neither can move the treasury.

### 4. Testnet only, by construction

The executor holds a constant set of allowed chain ids: Arbitrum Sepolia
(`421614`) and Ethereum Sepolia (`11155111`). It checks `eth_chainId` at boot
and before every submit, and refuses to sign for anything else. The RPC URL is
configuration; the chain is not.

### 5. Addresses

Recipients are exact bytes, as everywhere in Portcullis. **Address poisoning**,
where a lookalike address matching the first and last characters of a real one
is planted in a victim's history, is the commonest everyday crypto theft, and
an exact-match allowlist with full-string display defeats it.

To add: verify EIP-55 checksums when input is mixed-case, and display the
EIP-55 form. That needs Keccak-256, which is not a kernel dependency. Do it at
the MCP boundary, or admit `sha3` to the kernel deliberately and update the CI
check. Undecided.

Solana, later, is base58 and **case-sensitive**: today's canonicalization
lowercases every non-PayPal kind and would corrupt one. It needs its own kind
that is not case-folded.

## Honest limits

- **Stablecoins are not outside the system.** PYUSD's issuer can freeze an
  address, pause the token, and wipe a frozen balance. A payment Portcullis
  allowed can still be stopped by Paxos, and a frozen float wallet stops all
  payments. That is the same trust a PayPal balance carries, and it is a reason
  the gate is rail-neutral rather than a bet on one rail.
- **The float key is hot.** It lives where the executor runs. The float cap is
  what bounds that, not a hardware module.
- **Gas.** The relayer needs ETH; if it runs dry, payments stall (hold, not
  lose).
- **Finality on an L2.** Arbitrum's `finalized` tag means finalized on
  Ethereum, which takes minutes. Acceptable for accounts payable.

## Phases

0. This document.
1. **Done.** `EvmRail` in `crates/executor/src/evm.rs`, tested against a fake
   chain (`crates/executor/tests/evm_rail.rs`) that hashes and recovers the
   EIP-712 signature independently and enforces what PYUSD enforces.
   Mutation-checked: refusing after signing, never re-sending, judging the
   deadline at `latest`, skipping preflight, counting an unexplained use as
   paid, a fresh nonce per attempt, and a wrong EIP-712 prefix each fail a test.
2. **Done, 2026-11-10.** One live testnet payment with throwaway keys, read
   back from the chain. See *Live result*.
3. The MCP router: two ledgers, two policies, the console showing both.
4. Safe as the treasury, with top-ups.
5. x402: Portcullis as the spending gate in front of HTTP 402 payments.
6. Solana.

## Live result (phase 2, 2026-11-10)

`examples/evm_live.rs` on Ethereum Sepolia (`11155111`), PYUSD
`0xCaC524BcA292aaade2DF8A05cC58F0a65B1B3bB9`, keys from `examples/evm_keys.rs`
(float `0x4287…Ca32`, relayer `0xd6aF…10A0`, vendor `0xb7e3…cc0D`). The float
was funded with 100 PYUSD from the Paxos faucet and never held ETH.

1. The kernel judged a 12.84 PYUSD proposal to the vendor against a PYUSD
   ledger and a 50 PYUSD per-transaction cap: `Allow { via: Discretionary }`
   (no commitment was involved, so not `via: Commitment`).
2. The float signed an EIP-3009 authorization, nonce
   `0xe55f7319…550d1a` = `keccak256("portcullis:" || proposal_id)`; the relayer
   broadcast it:
   [`0x6093fd4a…eb077`](https://sepolia.etherscan.io/tx/0x6093fd4a1e2fc2d80143e76b0b23753bc244c13168395f10a3dc706d525eb077).
3. Receipt: status 1, block 12,106,602, 275,290 gas, an `AuthorizationUsed`
   log and one `Transfer(float → vendor, 12_840_000)`.
4. `observe` answered `Pending` every 30 s while the block was mined but not
   finalized, and `Success` at 815 s, when the `finalized` tag passed it.
5. Resubmitting the same payment returned the same batch (the nonce) and
   signed nothing. The relayer's transaction count is 1: nothing was sent
   twice. Balances after: float 87.16, vendor 12.84.

Two things learned getting here:

- **Google's Ethereum Sepolia PYUSD faucet cannot fund a fresh address.** It
  sends with a 100,000 gas limit; a transfer to an address that has never held
  PYUSD needs about 158,000 (tx `0x1063f292…e69f` reverted at 98,840). The
  Paxos faucet works.
- Ethereum Sepolia's public node at `ethereum-sepolia-rpc.publicnode.com`
  refuses `eth_getLogs` without an address filter. The rail always filters by
  the token, so this did not bite, but an RPC swap should be checked for it.
