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
| `crates/mcp` | the agent's entire surface: four tools over MCP Streamable HTTP | the network, behind per-agent tokens |

## Running it

```sh
cargo test                                   # everything except the live sandbox test
cargo run -p portcullis-mcp -- mint-token my-agent   # prints a token once, and the hash to configure
# set PAYPAL_* and PORTCULLIS_AGENT_TOKENS (see crates/mcp/src/main.rs), then:
cargo run -p portcullis-mcp -- serve
```

Any MCP client connects the same way it would to PayPal's own MCP server:
`npx mcp-remote https://<host>/mcp --header "Authorization: Bearer <token>"`.

## Status

Built for the PayPal *Build What's Next with PayPal and AI* hackathon, November 2026.
Working end to end against the PayPal sandbox. See `THREAT_MODEL.md` — including
what is not built yet.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
