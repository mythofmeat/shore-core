//! Smart image resize with format awareness, dimension floors, and disk caching.
//!
//! Replaces the MVP single-pass resizer with:
//! - Alpha detection (transparent PNGs stay PNG, opaque images convert to JPEG)
//! - Quality-first strategy for images under 2048px
//! - Dimension estimation for larger images
//! - XDG disk cache to avoid re-encoding on every turn
//! - Async pre-warming via spawn_blocking

use std::path::Path;
