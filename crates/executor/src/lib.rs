//! # Portcullis executor
//!
//! The kernel decides. This acts — and only on an [`Adjudication`] the kernel
//! made, against the policy and ledger held right now.
//!
//! Kept apart from the kernel on purpose: everything here touches a network, a
//! clock, or async, and none of it is allowed in the decision path. Decision
//! records come from the kernel; execution records come from here; the two
//! reconcile against each other.
//!
//! [`Adjudication`]: portcullis_kernel::Adjudication

#![forbid(unsafe_code)]

pub mod gatehouse;
pub mod paypal;
pub mod rail;

pub use gatehouse::{Event, Execution, Gatehouse, State, MAX_ATTEMPTS};
pub use paypal::{Credentials, Endpoint, PayPal, SANDBOX};
pub use rail::{step, ItemStatus, Observed, Rail, Step, Submitted};
