// Panic-hygiene lock (see [workspace.lints] in root Cargo.toml): this crate is
// cleaned, so these can never regress. Tests are exempt via clippy.toml.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::float_arithmetic,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    clippy::unwrap_in_result,
    clippy::panic_in_result_fn,
    clippy::let_underscore_must_use,
    clippy::clone_on_ref_ptr,
    clippy::dbg_macro,
    clippy::exit,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::str_to_string,
    clippy::mem_forget,
    clippy::match_wildcard_for_single_variants,
    clippy::wildcard_enum_match_arm,
    clippy::wildcard_imports,
    clippy::unseparated_literal_suffix,
    clippy::single_char_lifetime_names,
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block,
    clippy::missing_assert_message,
    clippy::shadow_same,
    clippy::shadow_reuse,
    clippy::shadow_unrelated,
    clippy::else_if_without_else,
    clippy::impl_trait_in_params,
    unsafe_code,
    elided_lifetimes_in_paths,
    unused_qualifications
)]
#![deny(clippy::print_stdout, clippy::print_stderr, unreachable_pub)]

pub mod budget;
pub mod cache_tracker;
pub mod client;
mod convert;
pub mod ledger;
pub mod pricing;
pub mod query;
pub mod stream;
mod sync;

pub use client::{CallType, CredentialFallbackEvent, LedgerClient};
pub use ledger::Ledger;
pub use stream::LedgerStream;

/// Whether a provider is billed by a flat subscription rather than metered
/// per-token usage.
///
/// Subscription calls are still recorded in the ledger (tokens, timing,
/// transcripts) for observability, but they contribute `$0` to usage budgets
/// and spend reports — per-token pricing is meaningless under a flat plan, and
/// budgets must never throttle calls that don't accrue marginal cost. Such rows
/// carry `cost_source = "subscription"` and `total_cost = 0`.
///
/// `opencode-go` is OpenCode's flat-rate ($10/mo) gateway over open models.
#[must_use]
pub fn is_subscription_provider(provider: &str) -> bool {
    matches!(provider, "opencode-go")
}
