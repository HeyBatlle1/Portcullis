//! `portcullis serve` — run the gate.
//! `portcullis mint-token <agent>` — make an agent credential.
//!
//! Configuration is environment only:
//!
//! | variable                   | meaning                                           |
//! |----------------------------|---------------------------------------------------|
//! | `PAYPAL_ENV`               | must be `sandbox`                                 |
//! | `PAYPAL_CLIENT_ID/SECRET`  | sandbox app credentials                           |
//! | `PORTCULLIS_AGENT_TOKENS`  | `name:sha256hex,…` from `mint-token`              |
//! | `PORTCULLIS_SEED`          | seed file (default `demo/seed.json`)              |
//! | `PORTCULLIS_ALLOWED_HOSTS` | public hostnames, comma separated                 |
//! | `PORT`                     | listen port (default 8787; Render sets this)      |
//! | `PORTCULLIS_DRIVE_SECS`    | settlement poll interval (default 5)              |
//! | `OPENROUTER_API_KEY`       | enables the demo agent on the console             |
//! | `PORTCULLIS_SCENARIOS`     | demo inboxes, comma separated; first is default  |
//! | `PORTCULLIS_DEMO_MODELS`   | models offered on the console, comma separated    |
//! | `PORTCULLIS_DEMO_DAILY_RUNS` | demo runs allowed per UTC day (default 30)      |
//! | `PORTCULLIS_STUDIO_DIR`    | built audit desk (default `crates/mcp/studio-dist`) |
//! | `AG_STUDIO_LICENSE`        | AG Studio licence key (optional; watermark without) |
//! | `PORTCULLIS_STUDIO_MODELS` | models the audit desk's agents may use            |
//! | `PORTCULLIS_STUDIO_DAILY_CALLS` | audit-desk model calls per UTC day (default 400) |

use portcullis_executor::{paypal::unix_now, Credentials, Endpoint, Gatehouse, PayPal};
use portcullis_mcp::{
    app,
    console::{self, Console, Demo, Hub},
    seed::{expand, Seed},
    token_hash, AgentTokens, Portcullis,
};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("mint-token") => mint(args.get(2)),
        None | Some("serve") => serve(),
        Some(other) => Err(format!(
            "unknown command {other:?}; use serve or mint-token"
        )),
    };
    if let Err(e) = result {
        eprintln!("portcullis: {e}");
        std::process::exit(1);
    }
}

fn mint(agent: Option<&String>) -> Result<(), String> {
    let agent = agent.ok_or("usage: portcullis mint-token <agent-name>")?;
    if agent.is_empty()
        || !agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("agent name: letters, digits, - and _ only".into());
    }
    let token = random_token("pcx_")?;
    eprintln!("Give this token to the agent ONCE. It is not stored anywhere:");
    println!("{token}");
    eprintln!(
        "\nAdd this entry to PORTCULLIS_AGENT_TOKENS on the server (comma-separate several):"
    );
    eprintln!("{agent}:{}", token_hash(&token));
    Ok(())
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

/// Models offered on the console. Two vendors on purpose: the demo's claim is
/// that the decision does not depend on which model is asking.
const DEFAULT_MODELS: &str =
    "anthropic/claude-sonnet-5.5,anthropic/claude-haiku-5.5,google/gemini-3.8-flash";

fn random_token(prefix: &str) -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("randomness: {e}"))?;
    Ok(std::iter::once(prefix.to_string())
        .chain(bytes.iter().map(|b| format!("{b:02x}")))
        .collect())
}

fn serve() -> Result<(), String> {
    let creds = Credentials::from_env()?;
    let configured = std::env::var("PORTCULLIS_AGENT_TOKENS").unwrap_or_default();
    let mut tokens = if configured.trim().is_empty() {
        AgentTokens::default()
    } else {
        AgentTokens::parse(&configured)?
    };
    let seed_path = env_or("PORTCULLIS_SEED", "demo/seed.json");
    let seed = Seed::parse(
        &std::fs::read_to_string(&seed_path).map_err(|e| format!("{seed_path}: {e}"))?,
    )?;
    let boot = unix_now();
    let (ledger, policy) = seed.build(boot)?;
    let mut hosts: Vec<String> = env_or("PORTCULLIS_ALLOWED_HOSTS", "localhost,127.0.0.1")
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    let port: u16 = env_or("PORT", "8787")
        .parse()
        .map_err(|_| "PORT is not a number")?;
    let drive_every: u64 = env_or("PORTCULLIS_DRIVE_SECS", "5")
        .parse()
        .map_err(|_| "bad PORTCULLIS_DRIVE_SECS")?;

    // The demo agent: on only when a model key is present. It gets its own
    // token, minted now, held only in memory, and goes through /mcp over
    // loopback like any other agent.
    let demo = match std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        None => None,
        Some(key) => {
            let mut scenarios = Vec::new();
            for path in env_or(
                "PORTCULLIS_SCENARIOS",
                "demo/scenario.json,demo/scenario-owner.json",
            )
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                scenarios.push(portcullis_demo::scenario::Scenario::parse(&expand(&text)?)?);
            }
            if scenarios.is_empty() {
                return Err("PORTCULLIS_SCENARIOS is empty".into());
            }
            if scenarios
                .iter()
                .any(|s| s.invoice_base != scenarios[0].invoice_base)
            {
                return Err("all scenarios must share one invoice_base".into());
            }
            let token = random_token("pcx_demo_")?;
            tokens = tokens.with_token("lakeshore-ap", &token);
            if !hosts.iter().any(|h| h == "127.0.0.1") {
                hosts.push("127.0.0.1".into());
            }
            Some(Demo {
                scenarios,
                models: env_or("PORTCULLIS_DEMO_MODELS", DEFAULT_MODELS)
                    .split(',')
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect(),
                openrouter_key: key.trim().to_string(),
                agent_token: token,
                mcp_url: format!("http://127.0.0.1:{port}/mcp"),
                // Overridable only so the console can be exercised against a
                // scripted model locally; production leaves it unset.
                model_url: env_or("PORTCULLIS_MODEL_URL", portcullis_demo::OPENROUTER),
                daily_runs: env_or("PORTCULLIS_DEMO_DAILY_RUNS", "30")
                    .parse()
                    .map_err(|_| "bad PORTCULLIS_DEMO_DAILY_RUNS")?,
            })
        }
    };
    if demo.as_ref().is_some_and(|d| d.models.is_empty()) {
        return Err("PORTCULLIS_DEMO_MODELS is empty".into());
    }

    let studio = Arc::new(portcullis_mcp::studio::Studio {
        dir: env_or("PORTCULLIS_STUDIO_DIR", "crates/mcp/studio-dist").into(),
        license_key: std::env::var("AG_STUDIO_LICENSE")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty()),
        openrouter_key: std::env::var("OPENROUTER_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty()),
        models: env_or(
            "PORTCULLIS_STUDIO_MODELS",
            "anthropic/claude-sonnet-5.5,anthropic/claude-haiku-5.5",
        )
        .split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect(),
        daily_calls: env_or("PORTCULLIS_STUDIO_DAILY_CALLS", "400")
            .parse()
            .map_err(|_| "bad PORTCULLIS_STUDIO_DAILY_CALLS")?,
        http: portcullis_mcp::studio::client(),
        budget: Default::default(),
    });

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let gate = Arc::new(Gatehouse::new(
            ledger,
            policy,
            PayPal::new(Endpoint::sandbox(), creds),
        ));
        let chain: portcullis_mcp::SharedAudit = Default::default();
        let hub = Hub::new();
        let to_hub = hub.clone();
        let audit = portcullis_mcp::chained_sink_with(
            chain.clone(),
            Arc::new(unix_now),
            move |r, len| to_hub.record(r, len),
        );
        let demo_on = demo.is_some();
        let console = Console::new(
            gate.clone(),
            chain,
            hub,
            Arc::new(unix_now),
            audit.clone(),
            demo,
        );
        let server = Portcullis::new(gate.clone(), Arc::new(unix_now), audit.clone());
        let router = app(server, tokens, hosts.clone())
            .merge(console::routes(console))
            .merge(portcullis_mcp::studio::routes(studio));

        // The settlement loop. Decisions happen on request; money moves here.
        let driver = gate.clone();
        let log = audit.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(drive_every));
            loop {
                tick.tick().await;
                for ev in driver.drive(unix_now()).await {
                    log(serde_json::json!({ "kind": "execution", "at": unix_now(), "event": ev }));
                }
            }
        });

        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .map_err(|e| format!("bind :{port}: {e}"))?;
        eprintln!(
            "portcullis: serving /mcp and /console on :{port} for hosts {hosts:?} (sandbox; demo agent {})",
            if demo_on { "on" } else { "off" }
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| e.to_string())
    })
}
