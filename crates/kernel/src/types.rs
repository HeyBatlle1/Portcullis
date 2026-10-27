//! Primitive types. No floats, no clock, no conversion between assets.

use serde::{Deserialize, Serialize};

/// Seconds since the Unix epoch. The kernel never reads a clock; callers pass
/// `now` in, which is what makes every decision reproducible from its inputs.
pub type Timestamp = u64;

/// A span of seconds.
pub type Duration = u64;

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
    /// Where money would go: a PayPal payer id, an email, an `0x` address.
    ///
    /// Compared byte-for-byte after [`RecipientId::canonical`]. Display the full
    /// string to humans, never a truncation: lookalike addresses are generated to
    /// match the first and last few characters.
    RecipientId
}

id_type! {
    /// A single proposal's identity, assigned by the caller.
    ProposalId
}

id_type! {
    /// An earmark's identity.
    CommitmentId
}

impl RecipientId {
    /// Lowercase and trim. Two recipients are the same only if their canonical
    /// forms are equal — there is no fuzzy matching anywhere in this crate.
    pub fn canonical(&self) -> RecipientId {
        RecipientId(self.0.trim().to_lowercase())
    }

    pub fn matches(&self, other: &RecipientId) -> bool {
        self.canonical() == other.canonical()
    }
}

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
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scale = 10u64.pow(self.asset.decimals as u32);
        let major = self.minor / scale;
        let minor = self.minor % scale;
        write!(
            f,
            "{}.{:0width$} {}",
            major,
            minor,
            self.asset.symbol,
            width = self.asset.decimals as usize
        )
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
