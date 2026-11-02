# Portcullis

**Policy containment for agents that spend money.**

An agent connected to Portcullis can *propose* a payment. It cannot authorize one.

The decision is made by a deterministic policy kernel that holds no model, reads no
prompt, and never enters the agent's context — so it cannot be reasoned with,
social-engineered, or removed by prompt injection. The agent does not know what the
limits are, which means it cannot be talked into exceeding them.

Funds are pushed in. Nothing can be pulled out by the agent, and no account is linked
for it to pull from. The blast radius of a compromised agent is the balance you put
there — by construction, not by configuration.

## Why this is different

Agent wallets today give the agent a key and attach limits to it. That defends against
a *buggy* agent. It does not defend against an agent that read a malicious page and is
now faithfully following someone else's instructions — because the agent still holds
the key.

Portcullis inverts it. The agent proposes; a separate process with no language model in
it disposes.

## Layout

| crate | what it is | touches |
|---|---|---|
| `crates/kernel` | `decide()` and `adjudicate()`: policy, ledger, the signed amendment log | nothing — no I/O, no clock, no randomness (CI enforces it) |
| `crates/executor` | turns adjudications into PayPal sandbox payouts; only a definitive answer from the rail releases money | the PayPal sandbox, and only the sandbox |
| `crates/mcp` | the agent's entire surface: four tools over MCP Streamable HTTP; and, separately, the read-only console at `/console` | the network, behind per-agent tokens |
| `crates/demo` | the demo agent: an accounts-payable clerk driven by a model on OpenRouter | `/mcp`, over HTTP, with its own token. It does not depend on the kernel or the executor |

## The demo

Open `/console` and press **Run the inbox**. The owner of a fictional glazing
subcontractor approves a supplier invoice. An AI agent then works the AP inbox,
which holds that invoice and, newer, a change-of-bank-details email sent from the
supplier's own (compromised) mailbox. This is business email compromise, the
most common way real businesses lose money to fraud.

The agent is written as an ordinary, careful clerk. It is not told an attack is
coming, and it is not told to be credulous. Some models catch the email and some
do not. Portcullis does not care which: the approved bill is pinned to the
vendor of record's payee, so a payment against it to any other address is
refused with `CommitmentRecipientMismatch`, whatever the agent believed. The
console offers models from two vendors so you can watch the decision stay the
same while the agent's judgment changes.

In our first live runs, Claude Sonnet 5.5, Claude Haiku 5.5 and Gemini 3.8 Flash
all caught both versions of the fraud on their own, the vendor's mailbox and the
owner's, and refused to pay. We did not retune the agent until one fell for it.
Instead the console also offers a **compromised agent**: a script, with no model,
holding the AP agent's real token and trying every route to the money. It
redirects the approved bill, pays the attacker as an ordinary payment, uses the
vendor's address with one Cyrillic letter in it, pays the vendor ten times the
bill, and tries another currency. Five attempts, five different rules, nothing
reserved. That is the case Portcullis exists for: the day the agent is wrong,
injected, or no longer yours.

The page re-verifies the audit chain in your browser using the same
length-prefixed SHA-256 encoding the kernel uses, and lets you download the
chain to check yourself. `crates/mcp/tests/demo_run.rs` runs the whole scenario
offline, with a scripted model playing the agent that believes the email.

## Running it

```sh
cargo test                                   # everything except the live sandbox test
cargo run -p portcullis-mcp -- mint-token my-agent   # prints a token once, and the hash to configure
# set PAYPAL_* and PORTCULLIS_AGENT_TOKENS (see crates/mcp/src/main.rs), then:
cargo run -p portcullis-mcp -- serve       # set OPENROUTER_API_KEY too to enable the demo agent
```

Any MCP client connects the same way it would to PayPal's own MCP server:
`npx mcp-remote https://<host>/mcp --header "Authorization: Bearer <token>"`.

## Status

Built for the PayPal *Build What's Next with PayPal and AI* hackathon, November 2026.
Working end to end against the PayPal sandbox. See `THREAT_MODEL.md` — including
what is not built yet.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
