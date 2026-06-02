#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "integration tests fail fast on setup/assertion failures; long scenario splits are tracked in #109"
)]

mod helpers;

mod autonomy;
mod compaction;
mod concurrency;
mod discovery_ignore;
mod e2e;
mod guardrails;
mod heartbeat;
mod key_fallback;
mod ledger;
mod memory_integration;
mod message_integrity;
mod pipeline;
mod preferences_persist;
mod providers;
mod recovery;
mod spawn_bind_zero;
mod static_setup;
