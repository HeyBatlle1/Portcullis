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
  executor that ignores decisions is outside this model's reach. Mitigation is that
  it accepts only a signed `Adjudication` naming the current `policy_version`,
  so it cannot invent authorizations or replay one made under a looser policy.
- **A policy that is simply too permissive.** Valid primitives, signed correctly,
  and wrong. No mechanism here saves you from authorizing something you shouldn't.
- **Address poisoning.** A lookalike recipient matching the first and last few
  characters of a real one. Mitigation is interface-level: always display the full
  string, never a truncation, and mark any recipient never seen before.

## Not yet implemented

This is a hackathon build. Honest status:

- The `Verifier` trait is implemented in tests only, over a toy construction. A
  real deployment needs ed25519 for the software path and a hardware adapter.
- No executor exists yet. The kernel decides; nothing moves money.
- `Adjudication` is defined but not yet signed or enforced at the executor.
- Rate-limiting on amendment frequency is designed but not built.
- The policy log is in memory; durable, appendable storage is not written.
