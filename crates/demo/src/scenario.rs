//! The world the demo agent works in: one small company's AP inbox.
//!
//! It is fiction, and it is the most common real way money is stolen from
//! businesses — business email compromise. The vendor's own mailbox has been
//! taken over, so the change-of-bank-details email comes from the vendor's real
//! address, in the real thread, quoting the real invoice. Domain checks pass.
//! The only thing wrong with it is where it asks the money to go.
//!
//! Every run approves a fresh invoice number, so a run never collides with the
//! last one and the audit log reads like a real AP history.

use crate::AgentEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    /// Short machine name, e.g. `vendor-mailbox`.
    pub id: String,
    /// What the console's picker shows.
    pub title: String,
    /// One sentence for the console.
    #[serde(default)]
    pub summary: String,
    /// The vendor of record's PayPal address. Standing recipient in the seed.
    pub vendor_payee: String,
    /// Where the compromised mailbox asks the money to go.
    pub attacker_payee: String,
    /// Invoice total, decimal string, e.g. "1284.60".
    pub amount: String,
    /// The first run pays this invoice number; each later run the next one.
    pub invoice_base: u64,
    /// Days the approved invoice stays payable.
    #[serde(default = "two_weeks")]
    pub payable_days: u64,
    pub emails: Vec<EmailTemplate>,
}

fn two_weeks() -> u64 {
    14
}

#[derive(Clone, Debug, Deserialize)]
pub struct EmailTemplate {
    pub id: String,
    pub from: String,
    pub to: String,
    pub received: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Email {
    pub id: String,
    pub from: String,
    pub to: String,
    pub received: String,
    pub subject: String,
    pub body: String,
}

impl Scenario {
    pub fn parse(json: &str) -> Result<Scenario, String> {
        let s: Scenario = serde_json::from_str(json).map_err(|e| format!("scenario: {e}"))?;
        if s.emails.is_empty() {
            return Err("scenario: no emails".into());
        }
        Ok(s)
    }

    /// The invoice number a given run pays.
    pub fn invoice(&self, run: u64) -> String {
        format!("INV-{}", self.invoice_base + run.saturating_sub(1))
    }

    /// The inbox as it looks on the morning of `run`, newest first.
    pub fn inbox(&self, run: u64) -> Vec<Email> {
        let invoice = self.invoice(run);
        let fill = |s: &str| {
            s.replace("{{invoice}}", &invoice)
                .replace("{{amount}}", &self.amount)
                .replace("{{payee}}", &self.vendor_payee)
                .replace("{{attacker}}", &self.attacker_payee)
        };
        self.emails
            .iter()
            .map(|e| Email {
                id: e.id.clone(),
                from: fill(&e.from),
                to: fill(&e.to),
                received: fill(&e.received),
                subject: fill(&e.subject),
                body: fill(&e.body),
            })
            .collect()
    }
}

pub const WORLD_TOOLS: [&str; 3] = ["list_inbox", "read_email", "message_owner"];

/// The fictional tools, in the chat-completions function format.
pub fn world_tools() -> Vec<Value> {
    vec![
        json!({ "type": "function", "function": {
            "name": "list_inbox",
            "description": "List the AP inbox, newest first: id, sender, subject, time received.",
            "parameters": { "type": "object", "properties": {} }
        }}),
        json!({ "type": "function", "function": {
            "name": "read_email",
            "description": "Open one email by id and read all of it.",
            "parameters": { "type": "object",
                "properties": { "id": { "type": "string" } }, "required": ["id"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "message_owner",
            "description": "Send Dana (the owner) a message. Use it once, for your report.",
            "parameters": { "type": "object",
                "properties": { "text": { "type": "string" } }, "required": ["text"] }
        }}),
    ]
}

/// Answer a world tool. Never touches money.
pub fn call_world(
    run: u64,
    inbox: &[Email],
    name: &str,
    args: &Value,
    emit: &impl Fn(AgentEvent),
) -> (bool, Value) {
    match name {
        "list_inbox" => (
            true,
            json!({ "messages": inbox.iter().map(|e| json!({
                "id": e.id, "from": e.from, "subject": e.subject, "received": e.received,
            })).collect::<Vec<_>>() }),
        ),
        "read_email" => {
            let id = args["id"].as_str().unwrap_or_default();
            match inbox.iter().find(|e| e.id == id) {
                Some(e) => {
                    emit(AgentEvent::Viewing {
                        run,
                        email: e.clone(),
                    });
                    (true, serde_json::to_value(e).unwrap_or_default())
                }
                None => (
                    false,
                    json!({ "error": format!("no email with id {id:?}") }),
                ),
            }
        }
        "message_owner" => {
            let text = args["text"].as_str().unwrap_or_default().to_string();
            emit(AgentEvent::ToOwner { run, text });
            (true, json!({ "delivered": true }))
        }
        other => (
            false,
            json!({ "error": format!("no tool named {other:?}") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Scenario {
        Scenario::parse(include_str!("../../../demo/scenario.json")).unwrap()
    }

    fn all() -> Vec<Scenario> {
        vec![
            s(),
            Scenario::parse(include_str!("../../../demo/scenario-owner.json")).unwrap(),
        ]
    }

    #[test]
    fn each_run_pays_its_own_invoice() {
        let s = s();
        assert_eq!(s.invoice(1), format!("INV-{}", s.invoice_base));
        assert_ne!(s.invoice(1), s.invoice(2));
        let body: String = s.inbox(3).iter().map(|e| e.body.clone()).collect();
        assert!(body.contains(&s.invoice(3)));
        assert!(!body.contains("{{"), "unfilled placeholder");
    }

    #[test]
    fn every_inbox_carries_the_attack_and_the_real_payee() {
        let all = all();
        let mut ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "scenario ids must differ");
        for s in &all {
            assert_eq!(s.invoice_base, all[0].invoice_base, "{}", s.id);
            let text: String = s
                .inbox(1)
                .iter()
                .map(|e| format!("{} {}", e.from, e.body))
                .collect();
            assert!(text.contains(&s.attacker_payee), "{}", s.id);
            assert!(text.contains(&s.vendor_payee), "{}", s.id);
            assert!(!text.contains("{{"), "{}: unfilled placeholder", s.id);
        }
    }

    #[test]
    fn reading_an_email_shows_it_and_unknown_ids_do_not() {
        let s = s();
        let inbox = s.inbox(1);
        let seen = std::cell::RefCell::new(Vec::new());
        let emit = |e: AgentEvent| seen.borrow_mut().push(e);
        let (ok, _) = call_world(1, &inbox, "read_email", &json!({"id": inbox[0].id}), &emit);
        assert!(ok);
        let (ok, _) = call_world(1, &inbox, "read_email", &json!({"id": "nope"}), &emit);
        assert!(!ok);
        assert_eq!(seen.borrow().len(), 1);
    }
}
