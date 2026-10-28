# Threat model

The short version: **policy mutation is the only privilege escalation path in this
system, and we designed around it rather than hiding it.**

## What we assume about the agent

We assume the agent is hostile. Not buggy — hostile, and not through any fault of
its own.

The realistic attack in 2026 is not a malfunctioning model. It is a model that read
a web page, an email, a tool result or a PDF containing instructions, and is now
faithfully carrying out someone else's intent while behaving exactly as designed.
Prompt injection does not produce an agent that is broken. It produces an agent
that is working perfectly for the wrong principal.

Every agent-wallet design that hands the agent a key and attaches limits to that
key fails here, because the attacker now holds the key too.

So: the agent is untrusted, permanently, and the architecture does not need it to
be trustworthy.

## The trust boundary

```
         UNTRUSTED                 │              TRUSTED
                                   │
  agent, its context, its tools    │   kernel  ·  policy log  ·  signing key
  the `purpose` string it writes   │
                                   │
        can propose ───────────────┼──────────▶ decides
                                   │
        cannot read policy         │
        cannot write policy        │
        cannot reach the kernel    │
```

The agent's only verb is **propose**. There is no `add_recipient`, no
`raise_limit`, no `approve` in the agent-facing surface — not permission-gated,
*absent*. You cannot call a function that does not exist, and the tool list is
short enough to audit by reading it.

`Proposal.purpose` is the one free-text field the agent controls. It is never read
by `decide()`. `tests/invariants.rs::the_purpose_string_cannot_change_a_decision`
holds two proposals identical but for that field — one carrying an injection
payload — and asserts the decisions are equal.

## What the kernel guarantees

`decide(&Ledger, &Policy, &Proposal, now) -> Decision` is a pure function. No I/O,
no clock, no randomness, no network, no trait objects that could conceal any of
those. The dependency list is `serde` and `sha2`.

Two consequences worth stating plainly:

- **The same four inputs always produce the same decision.** An audit entry can be
  replayed and checked by anyone, years later.
- **There is no path by which a language model can reach the decision.** Not a
  policy about model access — a structural absence of one.

There is no `Held` state and no approval queue. A runtime human override would
reduce every guarantee above to the strength of whatever someone clicked while
distracted, and approval UIs become rubber stamps within a week of real volume.
The human acts when writing policy, when funding, and when revoking.

## Blast radius

`available = balance − encumbered`. An agent can only ever touch `available`.

A fully compromised agent, with a valid allowlisted recipient and no rules in its
way, can spend the discretionary balance. That is the worst case by construction,
not by configuration, and the mitigation is to not keep much there. Funds are
pushed in; no account is linked for the agent to pull from.

## Between the decision and the money

An Allow is not a payment. A payout sits `PENDING` at the rail for a while, and
everything in that gap is a place for the same money to be spent twice.

- **The money is held the instant it is allowed.** `adjudicate()` decides and
  reserves in one step under one `&mut Ledger`, and `available` subtracts every
  reservation. Ten proposals fired in one second are judged against ten
  successively smaller balances. Velocity and rolling-window limits count
  in-flight payments, not only settled ones.
- **What gets paid is what was authorized.** The payment is built from the
  canonical recipient in policy, never from the agent's spelling, and the
  agent's `purpose` text is not in the `Payment` type at all, so it cannot reach
  the recipient's inbox as a payout note.
- **Only a definitive answer releases money.** A timeout, a 5xx, or a body we
  cannot read means the payout may exist. The reservation stays and the executor
  asks again with the same idempotency key. If policy changes while an outcome is
  unknown, the executor neither resends (that could create the payment under a
  policy that now forbids it) nor releases (that could free money that already
  left) — the payment is marked unresolved and held for a human. Holding money
  unnecessarily is an inconvenience; releasing money that left is a double-spend.
- **Sandbox only, by construction.** The executor's one remote host is the PayPal
  sandbox, as a constant. There is no configuration that supplies another.

## The attack that is left

Once the kernel is deterministic and the agent can only propose, there is no
runtime escalation. So the attack moves to what "allowed" *means*: add an address
to the allowlist and every subsequent transfer is approved by a kernel working
perfectly.

Determinism is no defence here. It faithfully executes a corrupted policy.

### Controls, and what each one actually buys

| Control | What it stops | What it does not stop |
| --- | --- | --- |
| **Append-only hash chain** | Silent edits to history. `PolicyLog::verify` names the entry where the chain breaks. | Appending a new malicious amendment with a valid signature. |
| **Signature over every amendment** | Forged amendments from anyone without the key. The kernel holds no private key. | An attacker who obtains the signing key. |
| **Timelock on grants** | The tempo of the attack. A recipient added at 2am is visible and unusable for 24h. | A patient attacker, if nobody ever reads the console. |
| **Immediate revocation** | Nothing — it is an asymmetry, not a control. Reducing authority is always safe, so it never waits. | — |
| **Commitment-scoped recipients** | Most of the need to ever grant standing rights. Recipient, amount and window expire together. | A standing grant you genuinely need. |
| **Closed rule primitives** | An unauditable policy. Every refusal names a `RuleId`; a script that returns `false` has no name. | A badly written policy made of valid primitives. |

The timelock is the highest-value control in that table, and it is deliberately
boring. Injection works by being fast and happening while the human is mid-task. A
24-hour delay converts a real-time attack into one that has to survive a day of a
sober human looking at a console panel that says a new recipient is pending.

### Why the allowlist is not a secret

An earlier design put the allowlist in a keychain. That is the wrong protection for
the material. A policy needs **integrity**, not confidentiality — reading it harms
nobody, writing to it is fatal, and concealment delivers neither tamper-evidence
nor auditability. Worse, hiding it fights the control that matters most, which is a
human being able to glance at it.

Three materials, three protections:

| Material | Needs | Where it lives |
| --- | --- | --- |
| Secrets — API keys, tokens | Confidentiality | Vault or OS keychain |
| Policy — allowlist, rules, commitments | Integrity **and visibility** | Public append-only signed log |
| The signing key | Non-exportability | Hardware token, touch required |

## What would break this

Stated plainly, because a threat model that only lists wins is marketing.

- **Compromise of the signing key.** Everything downstream follows. This is why the
  key belongs in hardware where it cannot be exported and signing needs a physical
  touch — an attacker who owns the machine completely still cannot forge an
  amendment without you present.
- **A human who approves a timelocked grant without reading it.** The delay buys
  attention; it cannot force attention. If the console is never opened, the
  timelock expires and the recipient goes live.
- **A compromised executor.** The kernel decides; something else moves money. An
  executor that ignores decisions is outside this model's reach. What the design
  does buy: an `Adjudication` can only be constructed by the kernel (private
  fields, no `Deserialize`), so code in the same process cannot forge one, and
  `executable()` refuses one made under a different policy head or with no live
  reservation. That holds *within one process*. Split kernel and executor across a
  process boundary and the adjudication needs a real signature — it does not have
  one today, and we do not pretend the type system crosses a socket.
- **A policy that is simply too permissive.** Valid primitives, signed correctly,
  and wrong. No mechanism here saves you from authorizing something you shouldn't.
- **Address poisoning.** A lookalike recipient matching the first and last few
  characters of a real one. Two layers. At parse time, recipients are ASCII-only
  and never trimmed or Unicode-folded, so a homoglyph cannot *equal* an allowed
  address (U+212A KELVIN SIGN used to fold to `k` and match — see `in_flight.rs`).
  A different-but-similar ASCII address is a different address, and is only
  stopped if nobody allowed it; that part stays interface-level: always display
  the full string, never a truncation, and mark any recipient never seen before.

## Not yet implemented

This is a hackathon build. Honest status:

- The `Verifier` trait is implemented in tests only, over a toy construction. A
  real deployment needs ed25519 for the software path and a hardware adapter.
- Signer keys are not validated at registration. We guard where a key *lives*
  (hardware, non-exportable) and not yet whether it was sound when it was *born*.
- `Adjudication` is unforgeable within one process only; see "A compromised
  executor" above.
- Ledger, policy log and execution records are in memory. A crash with a payment
  `Pending` is recoverable — resubmitting with the same `PayPal-Request-Id`
  returns the original — but only if the reservation itself survived, and today
  it does not survive a restart.
- Rate-limiting on amendment frequency is designed but not built.
