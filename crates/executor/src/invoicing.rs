//! PayPal Invoicing, sandbox only, from the vendor's side.
//!
//! This is not part of the gate. The gate pays bills; this is the supplier
//! sending one. It holds the *vendor's* credentials, a different PayPal account
//! from the one Portcullis pays from, so the party that issues an invoice and
//! the party that pays it are as separate here as they are in life. Nothing in
//! this module can move money: it creates an invoice, sends it, reads it, and
//! records against it a payment that has already happened elsewhere.
//!
//! Split like [`crate::paypal`]: the request bodies and the reading of
//! responses are pure functions, tested without a network; [`Invoicer`] is the
//! thin HTTP shell.

use crate::paypal::{Credentials, Endpoint, Session};
use serde::Serialize;
use serde_json::{json, Value};

/// One line of a bill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Line {
    pub name: String,
    pub quantity: u32,
    /// Decimal string in the bill's currency, e.g. `"58.40"`.
    pub unit: String,
}

/// Someone named on an invoice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Party {
    pub business: String,
    pub given: String,
    pub surname: String,
    /// The recipient's email gets the invoice. The invoicer's is left off: PayPal
    /// requires it to be on the account's profile, and the account's primary
    /// address is used anyway.
    pub email: Option<String>,
}

/// Everything needed to issue one invoice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bill {
    pub number: String,
    pub currency: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// One of PayPal's term types, e.g. `NET_15`.
    pub term: String,
    pub from: Party,
    pub to: Party,
    pub reference: String,
    pub note: String,
    pub lines: Vec<Line>,
}

fn party(p: &Party) -> Value {
    let mut v = json!({
        "business_name": p.business,
        "name": { "given_name": p.given, "surname": p.surname },
    });
    if let Some(e) = &p.email {
        v["email_address"] = json!(e);
    }
    v
}

/// Pure. The create-draft body for a bill.
pub fn draft(b: &Bill) -> Value {
    json!({
        "detail": {
            "invoice_number": b.number,
            "reference": b.reference,
            "currency_code": b.currency,
            "invoice_date": b.date,
            "note": b.note,
            "payment_term": { "term_type": b.term },
        },
        "invoicer": party(&Party { email: None, ..b.from.clone() }),
        "primary_recipients": [ { "billing_info": party(&b.to) } ],
        "items": b.lines.iter().map(|l| json!({
            "name": l.name,
            "quantity": l.quantity.to_string(),
            "unit_amount": { "currency_code": b.currency, "value": l.unit },
            "unit_of_measure": "QUANTITY",
        })).collect::<Vec<_>>(),
    })
}

/// Pure. The record-payment body. `OTHER`, because PayPal's `PAYPAL` method
/// wants the id of a payment made through the invoice itself, and this one was
/// made by Payouts; the note says which batch.
pub fn payment(amount: &str, currency: &str, date: &str, batch_id: &str) -> Value {
    json!({
        "method": "OTHER",
        "payment_date": date,
        "amount": { "currency_code": currency, "value": amount },
        "note": format!("Paid through Portcullis by PayPal Payouts, batch {batch_id}."),
    })
}

/// What PayPal said about a created invoice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub id: String,
    pub number: String,
}

/// Pure. Read a create-draft response. With `Prefer: return=representation`
/// the body is the invoice; without it, a link whose last segment is the id.
pub fn read_created(status: u16, body: &str) -> Result<Issued, String> {
    if status != 200 && status != 201 {
        return Err(error_text(status, body));
    }
    let v: Value = serde_json::from_str(body).map_err(|e| format!("unreadable invoice: {e}"))?;
    let id = v["id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            v["href"]
                .as_str()
                .and_then(|h| h.rsplit('/').next())
                .map(str::to_string)
        })
        .filter(|id| valid_id(id))
        .ok_or("no invoice id in the response")?;
    Ok(Issued {
        id,
        number: v["detail"]["invoice_number"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

/// Pure. The number PayPal says is next for this merchant.
pub fn read_next_number(status: u16, body: &str) -> Result<String, String> {
    if status != 200 {
        return Err(error_text(status, body));
    }
    let v: Value = serde_json::from_str(body).map_err(|e| format!("unreadable: {e}"))?;
    v["invoice_number"]
        .as_str()
        .filter(|n| !n.is_empty() && n.len() <= 25)
        .map(str::to_string)
        .ok_or_else(|| "no invoice_number".into())
}

/// Pure. Pull the payer's link out of a send response, if PayPal gave one.
pub fn read_sent(status: u16, body: &str) -> Result<Option<String>, String> {
    if !(200..300).contains(&status) {
        return Err(error_text(status, body));
    }
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    Ok(v["href"]
        .as_str()
        .filter(|h| h.starts_with("https://www.sandbox.paypal.com/"))
        .map(str::to_string))
}

/// Invoice ids are `INV2-` and groups of letters and digits. Anything else is
/// not interpolated into a URL.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 30 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn error_text(status: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let issue = v["details"][0]["issue"].as_str().unwrap_or_default();
    let msg = v["message"].as_str().unwrap_or_default();
    format!("HTTP {status} {issue} {msg}").trim().to_string()
}

/// The vendor's PayPal account, for invoicing only.
pub struct Invoicer {
    s: Session,
}

impl Invoicer {
    pub fn new(endpoint: Endpoint, creds: Credentials) -> Self {
        Invoicer {
            s: Session::new(endpoint, creds),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> Result<(u16, String), String> {
        for attempt in 0..2 {
            let bearer = self.s.bearer().await?;
            let mut req = self
                .s
                .http
                .request(
                    method.clone(),
                    format!("{}{path}", self.s.endpoint.as_str()),
                )
                .bearer_auth(bearer)
                .header("Prefer", "return=representation");
            if let Some(k) = key {
                req = req.header("PayPal-Request-Id", k);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let r = req.send().await.map_err(|e| format!("transport: {e}"))?;
            let status = r.status().as_u16();
            let text = r.text().await.unwrap_or_default();
            // A stale token gets one refresh; the retry carries the same
            // request id, so a create cannot happen twice.
            if status == 401 && attempt == 0 {
                self.s.forget_token().await;
                continue;
            }
            return Ok((status, text));
        }
        unreachable!("the loop returns on its second pass")
    }

    pub async fn next_number(&self) -> Result<String, String> {
        let (st, b) = self
            .call(
                reqwest::Method::POST,
                "/v2/invoicing/generate-next-invoice-number",
                None,
                None,
            )
            .await?;
        read_next_number(st, &b)
    }

    /// Create and send. `key` makes a retried create land on the same invoice.
    pub async fn issue(&self, bill: &Bill, key: &str) -> Result<(Issued, Option<String>), String> {
        let (st, b) = self
            .call(
                reqwest::Method::POST,
                "/v2/invoicing/invoices",
                Some(&draft(bill)),
                Some(key),
            )
            .await?;
        let issued = read_created(st, &b)?;
        let (st, b) = self
            .call(
                reqwest::Method::POST,
                &format!("/v2/invoicing/invoices/{}/send", issued.id),
                Some(&json!({ "send_to_recipient": true, "send_to_invoicer": false })),
                Some(&format!("{key}-send")),
            )
            .await?;
        let link = read_sent(st, &b)?;
        Ok((issued, link))
    }

    /// Mark an invoice paid by a payout that has already settled.
    pub async fn record_paid(
        &self,
        id: &str,
        amount: &str,
        currency: &str,
        date: &str,
        batch_id: &str,
    ) -> Result<(), String> {
        if !valid_id(id) {
            return Err("malformed invoice id".into());
        }
        let (st, b) = self
            .call(
                reqwest::Method::POST,
                &format!("/v2/invoicing/invoices/{id}/payments"),
                Some(&payment(amount, currency, date, batch_id)),
                Some(&format!("paid-{batch_id}")),
            )
            .await?;
        if (200..300).contains(&st) {
            Ok(())
        } else {
            Err(error_text(st, &b))
        }
    }

    /// The invoice's status as PayPal has it, e.g. `SENT` or `MARKED_AS_PAID`.
    pub async fn status(&self, id: &str) -> Result<String, String> {
        if !valid_id(id) {
            return Err("malformed invoice id".into());
        }
        let (st, b) = self
            .call(
                reqwest::Method::GET,
                &format!("/v2/invoicing/invoices/{id}"),
                None,
                None,
            )
            .await?;
        if st != 200 {
            return Err(error_text(st, &b));
        }
        let v: Value = serde_json::from_str(&b).map_err(|e| format!("unreadable: {e}"))?;
        Ok(v["status"].as_str().unwrap_or("UNKNOWN").to_string())
    }
}

/// Sum of a bill's lines, in minor units, if every price parses with exactly
/// two decimals. Used to check a bill's lines against the amount approved.
pub fn total_minor(lines: &[Line]) -> Option<u64> {
    lines.iter().try_fold(0u64, |acc, l| {
        let (whole, frac) = l.unit.split_once('.')?;
        if frac.len() != 2 || !whole.bytes().all(|b| b.is_ascii_digit()) || whole.is_empty() {
            return None;
        }
        let unit: u64 = whole.parse::<u64>().ok()? * 100 + frac.parse::<u64>().ok()?;
        acc.checked_add(unit.checked_mul(u64::from(l.quantity))?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill() -> Bill {
        Bill {
            number: "INV-30001".into(),
            currency: "USD".into(),
            date: "2026-11-04".into(),
            term: "NET_15".into(),
            from: Party {
                business: "Northline Glass Supply".into(),
                given: "Karen".into(),
                surname: "Lindqvist".into(),
                email: Some("never-sent@example.com".into()),
            },
            to: Party {
                business: "Lakeshore Glazing".into(),
                given: "Dana".into(),
                surname: "Okafor".into(),
                email: Some("payer@business.example.com".into()),
            },
            reference: "Meridian Tower storefront".into(),
            note: "Thank you".into(),
            lines: vec![
                Line {
                    name: "IGU low-E".into(),
                    quantity: 18,
                    unit: "58.40".into(),
                },
                Line {
                    name: "Freight".into(),
                    quantity: 1,
                    unit: "36.00".into(),
                },
            ],
        }
    }

    #[test]
    fn the_draft_names_the_payer_and_not_the_invoicers_email() {
        let d = draft(&bill());
        assert_eq!(d["detail"]["invoice_number"], "INV-30001");
        assert_eq!(
            d["primary_recipients"][0]["billing_info"]["email_address"],
            "payer@business.example.com"
        );
        assert!(d["invoicer"].get("email_address").is_none());
        assert_eq!(d["items"][0]["quantity"], "18");
        assert_eq!(d["items"][0]["unit_amount"]["value"], "58.40");
    }

    #[test]
    fn lines_total_exactly_or_not_at_all() {
        assert_eq!(total_minor(&bill().lines), Some(18 * 5840 + 3600));
        let bad = |u: &str| {
            vec![Line {
                name: "x".into(),
                quantity: 1,
                unit: u.into(),
            }]
        };
        assert_eq!(total_minor(&bad("58.4")), None);
        assert_eq!(total_minor(&bad("-1.00")), None);
        assert_eq!(total_minor(&bad("1.005")), None);
    }

    #[test]
    fn a_created_invoice_is_read_from_the_body_or_the_link() {
        let full = r#"{"id":"INV2-AB12-CD34-EF56-GH78","detail":{"invoice_number":"INV-30001"}}"#;
        assert_eq!(
            read_created(201, full).unwrap(),
            Issued {
                id: "INV2-AB12-CD34-EF56-GH78".into(),
                number: "INV-30001".into()
            }
        );
        let link = r#"{"rel":"self","href":"https://api-m.sandbox.paypal.com/v2/invoicing/invoices/INV2-ZZZZ-1111"}"#;
        assert_eq!(read_created(201, link).unwrap().id, "INV2-ZZZZ-1111");
        assert!(read_created(201, r#"{"id":"../../v1/payments"}"#).is_err());
        let dup = r#"{"name":"UNPROCESSABLE_ENTITY","details":[{"issue":"DUPLICATE_INVOICE_NUMBER"}],"message":"x"}"#;
        assert!(read_created(422, dup)
            .unwrap_err()
            .contains("DUPLICATE_INVOICE_NUMBER"));
    }

    #[test]
    fn only_a_sandbox_link_is_passed_on() {
        let ok = r#"{"href":"https://www.sandbox.paypal.com/invoice/p/#INV2-AB12"}"#;
        assert!(read_sent(200, ok).unwrap().is_some());
        let other = r#"{"href":"https://evil.example/invoice"}"#;
        assert_eq!(read_sent(200, other).unwrap(), None);
        assert!(read_sent(422, "{}").is_err());
    }

    #[test]
    fn a_recorded_payment_says_which_payout_paid_it() {
        let p = payment("1284.60", "USD", "2026-11-04", "5UXD2E8A7EBQJ");
        assert_eq!(p["method"], "OTHER");
        assert_eq!(p["amount"]["value"], "1284.60");
        assert!(p["note"].as_str().unwrap().contains("5UXD2E8A7EBQJ"));
    }
}
