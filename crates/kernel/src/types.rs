//! Primitive types. No floats, no clock, no conversion between assets.

use serde::{Deserialize, Serialize};

/// Seconds since the Unix epoch. The kernel never reads a clock; callers pass
/// `now` in, which is what makes every decision reproducible from its inputs.
pub type Timestamp = u64;

/// A span of seconds.
pub type Duration = u64;

/// 32 raw bytes of SHA-256. Hex on display.
pub type Hash = [u8; 32];

pub fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type! {
    /// Who proposed. Identity only — it grants nothing on its own.
    AgentId
}

id_type! {
    /// A single proposal's identity. Assigned by the server that accepts the
    /// proposal, never by the agent: the agent choosing its own id would let it
    /// collide with, or replay, one it has seen.
    ProposalId
}

id_type! {
    /// An earmark's identity.
    CommitmentId
}

// ── recipients ──────────────────────────────────────────────────────────────

/// What kind of address a [`RecipientId`] is. Determined by syntax alone: the
/// four grammars below are disjoint, so a string is at most one kind and the
/// executor never has to guess what it is paying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecipientKind {
    /// `local@domain.tld`, ASCII, lowercased.
    Email,
    /// E.164: `+` then 7–15 digits, first digit non-zero.
    Phone,
    /// A PayPal payer id: exactly 13 ASCII alphanumerics, uppercased.
    PayPalId,
    /// `0x` then 40 hex digits, lowercased. For the PYUSD adapter.
    Evm,
}

/// Why a string is not a payable address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientError {
    /// Contains a byte outside printable ASCII. This is the homoglyph gate:
    /// `"\u{212A}ate@example.com"` (KELVIN SIGN) lowercases to `kate@…` under
    /// Unicode rules, so any comparison that admits non-ASCII can be made to
    /// match an address the human never approved.
    NotAscii,
    /// Empty, or contains whitespace or a control character. Nothing is
    /// trimmed: trimming is a fuzzy match, and there is no fuzzy matching here.
    Whitespace,
    /// Printable ASCII, but no grammar accepts it.
    Unrecognized,
}

/// A payable address, in canonical form. **Always valid**: the only ways to
/// build one are [`RecipientId::parse`] and deserialization, which parses.
///
/// Two recipients are the same exactly when their bytes are equal. Because the
/// canonical form is the only form that exists, the bytes a human authorized in
/// policy are the bytes the executor sends — the agent's spelling never reaches
/// a payment.
///
/// Display the full string to humans, never a truncation: lookalike addresses
/// are generated to match the first and last few characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RecipientId(String);

impl RecipientId {
    pub fn parse(raw: &str) -> Result<RecipientId, RecipientError> {
        if raw.is_empty()
            || raw
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(RecipientError::Whitespace);
        }
        if !raw.is_ascii() {
            return Err(RecipientError::NotAscii);
        }
        let kind = Self::classify(raw).ok_or(RecipientError::Unrecognized)?;
        let canonical = match kind {
            RecipientKind::PayPalId => raw.to_ascii_uppercase(),
            _ => raw.to_ascii_lowercase(),
        };
        Ok(RecipientId(canonical))
    }

    pub fn kind(&self) -> RecipientKind {
        // Infallible: a RecipientId only exists if `classify` accepted it.
        Self::classify(&self.0).expect("RecipientId holds a classified address")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn classify(s: &str) -> Option<RecipientKind> {
        if is_email(s) {
            Some(RecipientKind::Email)
        } else if is_phone(s) {
            Some(RecipientKind::Phone)
        } else if is_evm(s) {
            Some(RecipientKind::Evm)
        } else if is_paypal_id(s) {
            Some(RecipientKind::PayPalId)
        } else {
            None
        }
    }
}

impl TryFrom<String> for RecipientId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        RecipientId::parse(&s).map_err(|e| format!("not a payable address ({e:?}): {s:?}"))
    }
}

impl From<RecipientId> for String {
    fn from(r: RecipientId) -> String {
        r.0
    }
}

impl std::fmt::Display for RecipientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// Grammars. Deliberately narrower than the RFCs: an address PayPal would accept
// but this refuses costs a policy amendment; the reverse costs money.

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~.-".contains(&b));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    local_ok && domain_ok && s.len() <= 254
}

fn is_phone(s: &str) -> bool {
    let Some(digits) = s.strip_prefix('+') else {
        return false;
    };
    (7..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

fn is_evm(s: &str) -> bool {
    let Some(h) = s.strip_prefix("0x") else {
        return false;
    };
    h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_paypal_id(s: &str) -> bool {
    s.len() == 13 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

// ── assets and amounts ──────────────────────────────────────────────────────

/// A named asset and its precision. The kernel never converts between assets and
/// holds no exchange rate: an FX rate is an external, time-varying input, and
/// admitting one would make decisions irreproducible.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId {
    pub symbol: String,
    /// Minor units per major unit, as a power of ten. USD is 2, PYUSD is 6.
    pub decimals: u8,
}

impl AssetId {
    pub fn new(symbol: impl Into<String>, decimals: u8) -> Self {
        Self {
            symbol: symbol.into(),
            decimals,
        }
    }

    pub fn usd() -> Self {
        Self::new("USD", 2)
    }

    pub fn pyusd() -> Self {
        Self::new("PYUSD", 6)
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.symbol)
    }
}

/// An amount in an asset's minor units.
///
/// Integers only. Floating point is not used anywhere in this crate, for money or
/// for anything else: `0.1 + 0.2 != 0.3` is not a property you want in a ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    pub asset: AssetId,
    pub minor: u64,
}

impl Amount {
    pub fn new(asset: AssetId, minor: u64) -> Self {
        Self { asset, minor }
    }

    pub fn same_asset(&self, other: &AssetId) -> bool {
        &self.asset == other
    }

    /// `"12.34"` — the decimal string a payment API expects, without the symbol.
    /// Exact: integer division and remainder, no float ever formed.
    pub fn decimal_string(&self) -> String {
        let d = self.asset.decimals as u32;
        if d == 0 {
            return self.minor.to_string();
        }
        let scale = 10u64.pow(d);
        format!(
            "{}.{:0width$}",
            self.minor / scale,
            self.minor % scale,
            width = d as usize
        )
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.decimal_string(), self.asset.symbol)
    }
}

/// A half-open interval `[from, until)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub from: Timestamp,
    pub until: Timestamp,
}

impl TimeWindow {
    pub fn new(from: Timestamp, until: Timestamp) -> Self {
        Self { from, until }
    }

    pub fn contains(&self, t: Timestamp) -> bool {
        t >= self.from && t < self.until
    }

    pub fn has_closed(&self, now: Timestamp) -> bool {
        now >= self.until
    }
}
