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
    clippy::cast_possible_wrap
)]

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
