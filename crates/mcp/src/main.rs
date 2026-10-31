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

use portcullis_executor::{paypal::unix_now, Credentials, Endpoint, Gatehouse, PayPal};
use portcullis_mcp::{app, seed::Seed, token_hash, AgentTokens, Portcullis};
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
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("randomness: {e}"))?;
    let token: String = std::iter::once("pcx_".to_string())
        .chain(bytes.iter().map(|b| format!("{b:02x}")))
        .collect();
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

fn serve() -> Result<(), String> {
    let creds = Credentials::from_env()?;
    let tokens = AgentTokens::parse(&std::env::var("PORTCULLIS_AGENT_TOKENS").unwrap_or_default())?;
    let seed_path = env_or("PORTCULLIS_SEED", "demo/seed.json");
    let seed = Seed::parse(
        &std::fs::read_to_string(&seed_path).map_err(|e| format!("{seed_path}: {e}"))?,
    )?;
    let boot = unix_now();
    let (ledger, policy) = seed.build(boot)?;
    let hosts: Vec<String> = env_or("PORTCULLIS_ALLOWED_HOSTS", "localhost,127.0.0.1")
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

    let gate = Arc::new(Gatehouse::new(
        ledger,
        policy,
        PayPal::new(Endpoint::sandbox(), creds),
    ));
    let audit: portcullis_mcp::AuditSink = Arc::new(|v| println!("{v}"));
    let server = Portcullis::new(gate.clone(), Arc::new(unix_now), audit.clone());
    let router = app(server, tokens, hosts.clone());

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
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
        eprintln!("portcullis: serving /mcp on :{port} for hosts {hosts:?} (sandbox)");
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| e.to_string())
    })
}
