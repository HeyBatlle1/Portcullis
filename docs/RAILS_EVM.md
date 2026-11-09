# Stablecoin rail: design (branch `rails/evm`)

Status: **design, phase 0.** Nothing here is on `main` or in the hosted demo.
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

| | Arbitrum Sepolia |
|---|---|
| chain id | `421614` (`0x66eee`), from `sepolia-rollup.arbitrum.io/rpc` |
| PYUSD (Paxos testnet) | `0x637A1259C6afd7E3AdF63993cA7E58BB438aB1B1` |
| `symbol()` | `PYUSD` |
| `decimals()` | `6` |
| `authorizationState(address,bytes32)` | answers (EIP-3009 present) |

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
| `authorizationState == false` and the chain's time is past `validBefore` | definitely never paid | release |
| A reverted receipt and `authorizationState == false` | definitely not paid | release |
| RPC error, timeout, or none of the above | unknown | change nothing, ask again |

The deadline is what turns "unknown" into "no". PayPal can leave a payout
pending indefinitely; a signed authorization dies on schedule.

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
1. `EvmRail` in `crates/executor`, against a fake JSON-RPC in tests:
   authorization building and EIP-712 signing, nonce derivation, the
   observation table above. Mutation-checked.
2. One live testnet payment with throwaway keys, read back from the chain.
3. The MCP router: two ledgers, two policies, the console showing both.
4. Safe as the treasury, with top-ups.
5. x402: Portcullis as the spending gate in front of HTTP 402 payments.
6. Solana.
