//! Explicit, saturating numeric conversions used in place of lossy `as` casts.
//!
//! Centralizing these keeps the truncation policy (and its justification) in one
//! place instead of scattering `#[expect]`-worthy `as` casts across the provider
//! modules.

use std::time::Duration;

/// Whole milliseconds of an elapsed `Duration`, saturating at `u32::MAX`.
///
/// `Duration::as_millis` returns `u128`; a single request's latency never
/// approaches `u32::MAX` ms (~49 days), so saturating is a safe, explicit floor
/// that documents intent and avoids a silent truncating `as` cast.
pub(crate) fn elapsed_ms_u32(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// Whole milliseconds of an elapsed `Duration`, saturating at `u64::MAX`.
pub(crate) fn elapsed_ms_u64(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
