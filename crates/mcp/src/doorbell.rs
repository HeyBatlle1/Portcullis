//! PayPal's webhook, as a doorbell.
//!
//! PayPal can tell us the moment a payout item changes
//! (`PAYMENT.PAYOUTS-ITEM.SUCCEEDED`, `.UNCLAIMED`, `.RETURNED` and the
//! rest). The usual way to take that call is to verify PayPal's signature on
//! it and then believe what it says. This does neither. A POST to
//! `/paypal/webhook` is never read: it rings a bell, and the settlement loop
//! answers by doing what it does every few seconds anyway, which is to ask
//! PayPal, over our own authenticated connection, what happened to each
//! payment in flight.
//!
//! So there is no signature to get wrong and nothing to forge. The most a
//! stranger can do with this route is make us check sooner, and the loop
//! keeps a minimum gap between passes however often the bell rings. Rule
//! one of the gatehouse is unchanged: only a definitive answer from the
//! rail, to a question we asked, settles or releases money.
//!
//! The timer stays. A webhook that never arrives costs a few seconds, not a
//! payment.

use axum::{extract::State, http::StatusCode, routing::post, Router};
use std::sync::Arc;
use tokio::sync::Notify;

/// The path to register in the PayPal developer dashboard.
pub const PATH: &str = "/paypal/webhook";

#[derive(Default)]
pub struct Doorbell(Notify);

impl Doorbell {
    pub fn new() -> Arc<Doorbell> {
        Arc::default()
    }

    /// Any number of rings before the loop next looks count as one.
    pub fn ring(&self) {
        self.0.notify_one();
    }

    /// Waits until the bell has rung since this was last awaited.
    pub async fn rung(&self) {
        self.0.notified().await;
    }
}

/// 200 whatever was sent, so PayPal does not retry. The body is not read.
async fn knock(State(bell): State<Arc<Doorbell>>) -> StatusCode {
    bell.ring();
    StatusCode::OK
}

pub fn routes(bell: Arc<Doorbell>) -> Router {
    Router::new().route(PATH, post(knock)).with_state(bell)
}
