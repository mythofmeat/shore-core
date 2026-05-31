// Panic-hygiene lock (see [workspace.lints] in root Cargo.toml). The harness is
// still being cleaned, but the lock makes every remaining violation explicit.
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

pub mod chaos;
pub mod collected;
pub mod config;
pub mod harness;
pub mod mock_llm;

// Convenience re-exports for test files.
pub use chaos::CrashedHarness;
pub use collected::CollectedResponse;
pub use config::TestConfigBuilder;
pub use harness::TestHarness;
pub use mock_llm::{AnthropicStreamBuilder, MockLlmServer};
