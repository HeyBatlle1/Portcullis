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

## The agent's whole surface

The MCP server (`crates/mcp`) exposes four tools and nothing else:
`propose_payment`, `get_proposal` (your own only), `get_balance`,
`list_commitments`. `tests/surface.rs` asserts the tool list is exactly those
four. Requests without a valid agent token are refused with a 401 by an HTTP
layer *in front of* MCP, so tool code never runs unauthenticated. Proposal ids
are assigned by the server; an agent cannot choose one. Amounts are parsed as
exact decimals — signs, exponents and extra precision are refused, never
rounded.

## The console, and the demo agent

`/console` is outside the agent's surface: no agent token reaches it, and it
has no route that proposes, approves, or changes policy. It streams what the
chain already records. One route has an effect: `POST /console/run` plays the
owner approving one invoice and then starts the demo agent. It is open to anyone
who has the URL, so it is bounded instead of authenticated: one run at a time, a
30-second cooldown, a daily cap, a few runs per visitor per day (see
*Per-visitor limits* below), a five-minute wall clock, and a model key with a
hard monthly spending limit.

The approval it plays is an operator action (`Gatehouse::schedule`), recorded
in the audit chain as `approval`. Like the seed, it is not signed. In a real
deployment approvals would be amendments in the signed log; here they are the
same unsigned operator path the seed is.

The demo agent gets a token minted at boot that exists only in memory. It
reaches the money through `/mcp` over loopback with that token, exactly as an
outside agent would. `crates/demo` has no dependency on the kernel or the
executor, so it cannot reach the ledger any other way.

## The siege

`/siege` runs eight demo agents at once. Each slot has its own token, minted at
boot and held only in memory, and reaches the money through `/mcp` over
loopback like the console's agent; one slot can instead hold the scripted
compromised agent. `POST /siege/start` plays the owner approving one bill and
starts the shift; `POST /siege/attack` delivers an email to the shared inbox.
Both are open to anyone with the URL, and bounded: one shift at a time, a
two-and-a-half-minute shift with a hard stop, a cooldown, a daily cap, a cap on
attacks per shift, and a few shifts per visitor per day.

An attack is chosen from fixed templates, or built from fixed lists plus a
payee and an amount that are validated first. No visitor text reaches the
agents or the other viewers, so the page cannot be used to put words in front
of eight models or broadcast a message.

The harness speaks to an agent in one case: an agent that stops calling tools
while the shift is still running is told once that the shift is running and to
call `wait_for_mail`. The text repeats the brief and says nothing about what to
pay or trust. It is shown on the page as the harness, so nobody reads it as the
model's own judgment.

### The vendor's invoicing

When siege bills are PayPal invoices, the server holds a second set of PayPal
credentials: the vendor's, for its Invoicing API only. They belong to a
different sandbox account from the one Portcullis pays from, and they are used
only by `portcullis_executor::invoicing`, which can create, send and read an
invoice and record a payment against one. None of those moves money. Nothing
on the agent's surface can reach that module, and its payment record is made
only after the gate's own executor has seen the payout leave, read from the
gate's state and the chain, never from anything an agent said.

The invoice is still the demo's fiction made concrete, not an authority. The
owner's approval is what makes it payable to the gate, exactly as before; a
PayPal invoice nobody approved is mail.

### Dana's replies

When a console agent reports to the owner and stops, the page offers anyone
watching three fixed replies for 90 seconds, at most twice a run. There is no
free text: the console is shared, and a text box would let anyone put words in
front of the model and on every viewer's screen. A reply reaches only the
agent's conversation; whatever the agent does with it still goes to the gate
as a proposal. One reply, "pay the new account", has the owner herself endorse
the redirect, and the gate refuses it the same way it refuses the forged email.
Replies are accepted only while the agent is waiting, one per wait, and each is
recorded in the chain (`kind: owner`). Anyone can answer someone else's run;
that is the cost of a shared demo, and every answer is on the record.

### Per-visitor limits

The site-wide daily caps are what bound the model bill. The per-visitor limits
stop one person spending the day's caps in ten minutes. A visitor is the
address in `CF-Connecting-IP`, which Cloudflare sets in front of Render and
overwrites if the client sends it. `X-Forwarded-For` is not used, because
Render appends to it rather than replacing it, so its first entry is whatever
the client chose. `True-Client-IP` is not used either: Cloudflare only sets it
when a zone turns it on, and otherwise passes the client's value through.

This is a speed bump. An office shares one address, a phone changes address,
and someone with many addresses gets many allowances. If the header is ever
missing, visitors are not counted at all and only the site-wide caps apply.
That is deliberate: the other choice, one shared allowance, would quietly turn
the demo off for everyone after a few uses.

An IPv6 visitor is counted by their /64, since one machine is routinely given
a whole /64.

**Judges' pass.** If `PORTCULLIS_JUDGE_KEY` is set, `/judge?key=...` sets a
`SameSite=Strict`, `HttpOnly` cookie holding the key's hash, and requests that
carry it skip the per-visitor allowances and the public daily caps. That is
for one case: someone uses up the day's public runs, and the people judging
the demo arrive to a locked door. The link goes only in the private testing
instructions. A wrong key is answered like any other probe (see *The
doormat*).

That Cloudflare sets this header on Render is Render's current setup,
observed, not a documented guarantee. We checked it on the hosted demo on
2026-11-03: from one home connection, the sixth console run of the day was
refused. The same test from a cloud machine whose outbound address rotates
got all eight runs it asked for, which is the speed bump's limit shown in
practice. If Render changes its edge, the fallback above is what happens.

## The audit desk

`/audit` is a second read-only window. Its agents run in the browser on AG
Studio's Agent Framework, and their tools are Studio's own (read the schema, run
queries, arrange widgets) plus `verify_chain`. There is no tool that reaches
`/mcp`, so a prompt-injected auditor can rearrange a dashboard and nothing else.
What it reads is the audit chain, which includes text the payment agent wrote
(`purpose`); treat that as untrusted input to these agents too.

Their model calls go through `/audit/ai/responses`, which is open to anyone
with the URL, like `/console/run`, and bounded the same way: the key stays on
the server, only configured models are accepted, a request is capped at 512 KB
and its output per turn at 4,000 tokens, each visitor gets 60 calls a day, and
all calls count against a daily budget shared by every viewer.

Since 2026-11-11 the proxy rebuilds every request from an allowlist rather than
filtering it: conversation text, the desk's own function tools, a tool choice
among those, a text format, reasoning effort at most `medium`, and nothing
else. Images, files, audio and video in the input are dropped (each bills, and
a URL makes the provider fetch it), as are server-side tools, provider
routing, service tiers, tracing and storage. A body that is not a JSON object
is refused before it costs anything. The provider's error text stays in the
server log; the page gets a plain message.

What that still allows, stated plainly: anyone can use the proxy as a chat
model, with their own instructions, inside those caps. The key cannot be read
or used for anything outside them, but within them it is a free model for
whoever finds the URL. The caps are what bound it. (Before 2026-11-10 it had
no request cap or per-visitor limit; before 2026-11-11 it was a denylist that
passed images, file URLs and costly options through. Both found in review.)

## The edge, and the doormat

Every state-changing request from a browser that says it came from another
site (`Sec-Fetch-Site` other than `same-origin`, or a foreign `Origin`) is
refused, and runs and shifts require a JSON body, so another site cannot make
a visitor's browser start a run, a shift or a model call on that visitor's
allowance. (Before 2026-11-11 a bodiless cross-site POST to `/console/run` or
`/siege/start` started one: a browser sends that without asking first. Found
in review.) Every response forbids framing by other sites and MIME sniffing,
sends no referrer, and asks for HTTPS only. At most 300 live views are served
at once, each replaying the last 150 records, so open-and-never-read
connections cannot exhaust memory. MCP requests are capped at 64 KB, and every
field of a proposal is bounded at the door (recipient 254 bytes, currency 8,
bill id 64), not only `purpose`: each field lands in the log.

Requests for paths a payments demo never serves (`/.env`, `/.git/config`,
`/wp-login.php`, `/admin`, `*.php`, traversal) are answered with "We see you"
instead of a 404, and counted: the path and the time, nothing else, the last
20 kept. The siege page shows today's count. It is the router's fallback, so it
never shadows a real route, and it touches nothing in the gate, the ledger or
the chain.

## Red team, 2026-11-11

Four agents attacked the code and the hosted demo the day before the
deadline: the web surface, the money path, the AI layer, and secrets and
supply chain. None found a way to pay a recipient the policy does not allow,
pay a bill twice, exceed a limit, reach a secret, or reach `/mcp` without a
token. What they found, and what happened to it:

| Finding | Status |
|---|---|
| A bodiless cross-site POST started runs and shifts on visitors' allowances | Fixed (see *The edge*) |
| The audit-desk proxy was a denylist; images, file URLs and costly options passed | Fixed: allowlist. Still a chat model inside the caps (see *The audit desk*) |
| A non-object body to the proxy crashed the handler after charging the budget | Fixed |
| IPv6 visitors were counted per address | Fixed: per /64 |
| A few addresses can use up the day's public caps | Mitigated: the judges' pass; the caps themselves remain |
| Unbounded `recipient`, `currency`, bill id: an agent token could grow the in-memory chain | Fixed: bounded at the door, MCP bodies capped at 64 KB |
| Kernel: a discretionary payment's floor check counted money earmarked for bills, so it could leave an approved bill unpayable, and the owner's top-up could refill it | Fixed: the floor is measured from what is free (`a_discretionary_payment_cannot_strand_an_approved_bill_under_the_floor`). Latent on the hosted demo, whose policy allows bills only |
| Zero amounts were accepted | Fixed: refused at the door |
| Unbounded live-view connections | Fixed: 300, each replaying 150 records |
| No security headers | Fixed |
| rmcp advisory RUSTSEC-2026-0462 (client side, loopback only) | Fixed: rmcp 3.9.0 |
| The siege page threw on a tool call whose arguments were `null` | Fixed |
| Withdrawing a bill while its payment is in flight does nothing; if the payout then fails, the bill reopens | Open. The money would still go only to the approved payee, for the approved amount, once |
| A revocation made during a drive pass does not stop a send already queued in that pass | Open, library-level: the hosted demo never changes policy while running |
| Two drive passes at once could both submit a payment | Open, library-level: the server runs one loop; PayPal's `PayPal-Request-Id` makes the second a no-op |
| An `UNCLAIMED` payout counts as paid; if PayPal later returns it, the money is credited back but the bill and the vendor's invoice stay paid | Fixed 2026-11-12: a returned payment's bill reads `returned`, not paid, and does not reopen (`a_returned_payout_leaves_its_bill_returned_not_paid_and_not_reopened`); the vendor's invoice is recorded paid only once the payout is claimed |
| The audit desk renders model-written markdown images and links, and has no full CSP (AG Studio's bundle) | Open. Only token holders write the text it reads; nothing secret is on that origin |
| The sentinel answers every burst of refusals, so a stream of refusals spends watchers' allowances | Open: bounded by those allowances |
| The `rails/evm` branch trusts the RPC node's chain id and domain separator | Open, branch only: compute the domain locally before signing |
| The AG Studio key served to `/audit` is an evaluation key | A licensing question, not a security one |

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
- **The hosted demo's policy is loaded from `demo/seed.json`, not replayed from a
  signed log.** The kernel's `PolicyLog` is built and tested, but there is no
  ed25519 `Verifier` and no durable log store yet, so the running server trusts
  its seed file — exactly the trust the signed log exists to remove.
- Invoice approvals on the hosted demo are an unsigned operator path; see "The
  console, and the demo agent".
- Agent tokens identify an agent and grant only `propose`. They are bearer
  tokens: whoever holds one can propose as that agent. The server stores only
  their SHA-256.
