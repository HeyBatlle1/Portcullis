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

## Status

Early. Built for the PayPal *Build What's Next with PayPal and AI* hackathon, November 2026.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
