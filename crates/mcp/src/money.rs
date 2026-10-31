//! Decimal strings to minor units, exactly. No float is ever formed.

/// Why an amount string was refused before it got anywhere near the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountError {
    /// Not `digits` or `digits.digits`. Signs, exponents, spaces, commas and
    /// currency symbols are all refused: every one of them is a way for "12"
    /// to mean something other than twelve.
    Malformed,
    /// More decimal places than the asset has. `"0.001"` USD is not rounded.
    TooPrecise,
    /// Does not fit in a u64 of minor units.
    TooLarge,
}

/// `"12.34"` with 2 decimals → `1234`. `"12"` → `1200`. `"12.3"` → `1230`.
pub fn parse_amount(s: &str, decimals: u8) -> Result<u64, AmountError> {
    if s.is_empty() || s.len() > 24 {
        return Err(AmountError::Malformed);
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || (s.contains('.') && frac.is_empty()) {
        return Err(AmountError::Malformed);
    }
    if frac.len() > decimals as usize {
        return Err(AmountError::TooPrecise);
    }
    let scale = 10u64
        .checked_pow(decimals as u32)
        .ok_or(AmountError::TooLarge)?;
    let w: u64 = whole.parse().map_err(|_| AmountError::TooLarge)?;
    let f: u64 = if frac.is_empty() {
        0
    } else {
        let padded = format!("{frac:0<width$}", width = decimals as usize);
        padded.parse().map_err(|_| AmountError::TooLarge)?
    };
    w.checked_mul(scale)
        .and_then(|m| m.checked_add(f))
        .ok_or(AmountError::TooLarge)
}
