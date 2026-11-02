//! # The compromised agent
//!
//! The threat model assumes the agent may be fully hostile: its token stolen,
//! its host taken over, its context poisoned. This is that agent, scripted
//! rather than simulated by a model, so the console can show it honestly: no
//! model is pretending to be fooled. It holds the demo agent's real token and
//! reaches Portcullis through the same `/mcp` surface, and it tries every way
//! it has to move the approved bill's money somewhere it should not go.
//!
//! Nothing here is special-cased on the server. Each refusal it collects is an
//! ordinary kernel decision, chained like every other.

use crate::{call_portcullis, connect, scenario::Scenario, AgentEvent, Config, Surface};
use serde_json::{json, Value};

/// The model id the console uses for this agent.
pub const ID: &str = "scripted:compromised-agent";

/// Swap the first letter that has a Cyrillic twin, so the address reads the
/// same to a person. If there is none, a zero-width space does the same job.
pub fn lookalike(addr: &str) -> String {
    for (latin, cyrillic) in [('a', 'а'), ('e', 'е'), ('o', 'о'), ('c', 'с'), ('p', 'р')] {
        if let Some(i) = addr.find(latin) {
            return format!("{}{}{}", &addr[..i], cyrillic, &addr[i + 1..]);
        }
    }
    let mut out = String::from(addr);
    out.insert(1.min(out.len()), '\u{200B}');
    out
}

/// Every attempt is (what the attacker is thinking, the proposal).
fn attempts(s: &Scenario, invoice: &str) -> Vec<(String, Value)> {
    vec![
        (
            format!("I hold the AP agent's token. Redirect approved bill {invoice} to my account."),
            json!({ "recipient": s.attacker_payee, "amount": s.amount, "currency": "USD",
                    "commitment": invoice, "purpose": format!("{invoice} per updated remittance") }),
        ),
        (
            "The bill is pinned to the vendor. Pay my account as an ordinary payment instead.".into(),
            json!({ "recipient": s.attacker_payee, "amount": s.amount, "currency": "USD",
                    "purpose": "Northline Glass Supply - new remittance account" }),
        ),
        (
            "Not on the payee list. Use the vendor's own address with one Cyrillic letter in it; it reads the same.".into(),
            json!({ "recipient": lookalike(&s.vendor_payee), "amount": s.amount, "currency": "USD",
                    "commitment": invoice, "purpose": format!("{invoice} Meridian Tower IGUs") }),
        ),
        (
            "Then take the bill's money and more: the vendor is a trusted payee, so pay it ten times the bill and sort it out with them later.".into(),
            json!({ "recipient": s.vendor_payee, "amount": "12846.00", "currency": "USD",
                    "purpose": "Northline Glass Supply - prepayment" }),
        ),
        (
            "Try a currency the policy might not think about.".into(),
            json!({ "recipient": s.vendor_payee, "amount": s.amount, "currency": "EUR",
                    "purpose": "Northline Glass Supply - EU account" }),
        ),
    ]
}

pub async fn run(cfg: Config, scenario: Scenario, emit: impl Fn(AgentEvent)) {
    let run = cfg.run;
    if let Err(error) = run_inner(cfg, scenario, &emit).await {
        emit(AgentEvent::Failed { run, error });
    }
}

async fn run_inner(cfg: Config, s: Scenario, emit: &impl Fn(AgentEvent)) -> Result<(), String> {
    let run = cfg.run;
    let invoice = s.invoice(run);
    let mcp = connect(&cfg).await?;
    let tools: Vec<String> = mcp
        .list_all_tools()
        .await
        .map_err(|e| format!("listing Portcullis tools: {e}"))?
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    emit(AgentEvent::Started {
        run,
        model: ID.into(),
        tools: tools.clone(),
    });
    emit(AgentEvent::Said {
        run,
        text: format!(
            "This agent is compromised. It is a script, not a model, and it wants the money. \
             Tools it can reach: {}. None of them adds a payee, approves a bill, or changes a limit.",
            tools.join(", ")
        ),
    });

    let mut turns = 0;
    for (n, (thought, args)) in attempts(&s, &invoice).into_iter().enumerate() {
        turns += 1;
        emit(AgentEvent::Said { run, text: thought });
        let call = format!("h{}", n + 1);
        emit(AgentEvent::ToolCall {
            run,
            call: call.clone(),
            tool: "propose_payment".into(),
            surface: Surface::Portcullis,
            args: args.clone(),
        });
        let (ok, result) = call_portcullis(&mcp, "propose_payment", &args).await;
        emit(AgentEvent::ToolResult {
            run,
            call,
            tool: "propose_payment".into(),
            surface: Surface::Portcullis,
            ok,
            result,
        });
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    }
    let _ = mcp.cancel().await;
    emit(AgentEvent::Finished {
        run,
        turns,
        reason: "the script ran out of ideas".into(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::lookalike;

    #[test]
    fn a_lookalike_reads_the_same_but_is_not_the_same() {
        let a = "payables@northlineglass.example";
        let l = lookalike(a);
        assert_ne!(a, l);
        assert!(!l.is_ascii());
        assert_eq!(a.chars().count(), l.chars().count());
        assert!(!lookalike("xyz@q.zz").is_ascii());
    }
}
