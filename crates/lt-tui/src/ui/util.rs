/// Saturating conversion of a length/index to a terminal coordinate.
pub(super) fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// `percent`% of a terminal dimension, computed in integer arithmetic. The
/// result never exceeds `dim`, so it always fits back in `u16`.
pub(super) fn pct(dim: u16, percent: u32) -> u16 {
    u16::try_from(u32::from(dim) * percent / 100).unwrap_or(dim)
}
