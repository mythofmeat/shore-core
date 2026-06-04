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
    clippy::cast_possible_wrap,
    clippy::as_conversions,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    clippy::unwrap_in_result,
    clippy::panic_in_result_fn,
    clippy::let_underscore_must_use,
    clippy::clone_on_ref_ptr,
    clippy::dbg_macro,
    clippy::exit,
    clippy::mem_forget,
    clippy::match_wildcard_for_single_variants,
    clippy::wildcard_enum_match_arm,
    clippy::wildcard_imports,
    clippy::unseparated_literal_suffix,
    clippy::single_char_lifetime_names,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::float_arithmetic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::str_to_string,
    clippy::undocumented_unsafe_blocks,
    clippy::shadow_same,
    clippy::shadow_reuse,
    clippy::shadow_unrelated,
    unsafe_code,
    elided_lifetimes_in_paths,
    unused_qualifications
)]
#![deny(unreachable_pub)]

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
pub use mock_llm::AnthropicStreamBuilder;
#[cfg(unix)]
pub use mock_llm::MockLlmSidecar;
