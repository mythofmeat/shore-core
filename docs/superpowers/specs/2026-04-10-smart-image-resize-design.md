# Smart Image Resize Pipeline

**Date:** 2026-04-10
**Status:** Approved
**Replaces:** MVP single-pass resize in `shore-daemon/src/handler/images.rs`

## Problem

The current `maybe_resize()` implementation has several shortcomings:

1. **Single-pass guess:** Uses `sqrt(ratio) * 0.9` heuristic with no verification or retry. If the first encode is still over the limit, the image is sent as-is.
2. **Always converts to JPEG:** PNGs with transparency lose their alpha channel. Screenshots with sharp edges get JPEG compression artifacts.
3. **Fixed quality 85:** No quality stepping. Lowering quality before scaling dimensions would often preserve more visual information.
4. **No dimension awareness:** Never considers that very large dimensions (8K+) are wasteful for LLM vision, or that images below ~2048px shouldn't be shrunk further.
5. **No caching:** The same image is decoded, resized, and re-encoded on every API call — every turn of conversation.
6. **Blocks the event loop:** All resize work is synchronous CPU work on the tokio async runtime.

## Design

### Resize Algorithm

Format-aware decision tree:

```
Input image arrives
  ├─ size ≤ max_bytes → pass through unchanged
  ├─ GIF → pass through with warning (unchanged from MVP)
  └─ needs resize:
      ├─ has alpha transparency?
      │   YES → keep as PNG, resize dimensions + max compression (level 9)
      │   NO  → convert to JPEG
      │         ├─ dims ≤ 2048px longest side? → quality-only: encode at 90, if over encode at 75
      │         └─ dims > 2048px → estimate target dims via bpp, encode at quality 90
      └─ verify: if still over limit, one retry with 15% more aggressive settings
         └─ if STILL over: send anyway + warn log
```

**Estimation math:** Source bpp (bits per pixel) = `(file_bytes * 8) / (width * height)`. For JPEG at quality Q, approximate output bpp ≈ `source_bpp * (Q / 100)^1.5` (empirical, content-dependent but sufficient for a first estimate). Solve for target dimensions given target byte size and chosen quality level.

**Dimension floor:** 2048px on the longest side. Never resize below this unless the source is already smaller, or it's a transparent PNG that cannot fit within the byte limit at that resolution.

**Alpha detection:** Check if any pixel in the decoded image has alpha < 255. If all pixels are fully opaque, treat as opaque (eligible for JPEG conversion). This is a scan of the decoded pixel buffer — fast since we've already decoded the image.

### Cache Layer

**Location:** `$XDG_CACHE_HOME/shore/resized/` via a new `cache` field on `ShoreDirs`.

**Cache key:** `sha256(canonical_path + ":" + file_mtime_nanos + ":" + max_image_size)` → hex string used as filename (e.g., `a3f2b1…c4d5.jpg` or `.png`).

- `mtime` invalidates when the source file changes.
- `max_image_size` invalidates when the user changes their config.
- Deleting `$XDG_CACHE_HOME/shore/resized/` is always safe — triggers re-encode on next use.

**No eviction policy.** Resized images are ~1-2MB each. Even 1000 cached images would be ~2GB, acceptable for a cache directory.

### Async Architecture

**Pre-warm + cache-read pattern:**

```rust
// At call sites (handler/mod.rs, autonomy/manager.rs):
warm_image_cache(&prompt_result.messages, max_image_size, &cache_dir).await;
let (llm_messages, system) = build_llm_messages(
    &prompt_result, include_unsigned_thinking, max_image_size, &cache_dir,
);
```

`warm_image_cache` is an async function that:
1. Scans all messages for images that need resizing (size > max_bytes and not already cached).
2. For each cache miss, spawns a `tokio::task::spawn_blocking` task to decode, resize, and write the result to the cache directory.
3. Runs all spawned tasks concurrently via `join_all`.

`build_llm_messages` stays synchronous. `maybe_resize` checks the cache first — on a hit, it reads the cached file (~1ms disk read) instead of decoding and resizing. On a miss (defensive fallback, should not happen after warm-up), it performs the resize synchronously as today.

**Rationale for this pattern over alternatives:**
- Making `build_llm_messages` fully async would require rewriting its `.iter().map()` chain and propagating async through callers — high cost, same result.
- Passing pre-computed data via a HashMap adds a new data structure and fiddly mapping. The disk cache IS the communication channel — simpler and the cache persists across restarts.
- Graceful degradation: if pre-warming is skipped for any reason, the system still works (just blocks, as it does today).

### Dependencies

**Add:** `fast_image_resize` with `rayon` feature to `shore-daemon/Cargo.toml`. Provides SIMD-optimized (AVX2/SSE4.1) resize that is ~14x faster than the `image` crate's `resize()`. Pure Rust, no system library required.

**Keep:** `image` crate for decode and encode. `fast_image_resize` only handles the pixel-buffer resize step.

### Config

No config changes. The existing `max_image_size` field in `[advanced]` controls the byte limit (default 2MB, 0 = disable). The dimension floor (2048px) and quality parameters are hardcoded — they're implementation details, not user-facing knobs.

### ShoreDirs Change

Add a `cache: PathBuf` field to `ShoreDirs` in `shore-config/src/lib.rs`, resolved via the existing `resolve_xdg_dir` pattern:

```rust
cache: resolve_xdg_dir(
    "SHORE_CACHE_DIR",
    "XDG_CACHE_HOME",
    dirs::cache_dir,
    "~/.cache",
),
```

## Files Affected

- `shore-config/src/lib.rs` — add `cache` field to `ShoreDirs`
- `shore-daemon/Cargo.toml` — add `fast_image_resize` dependency
- `shore-daemon/src/handler/images.rs` — rewrite `maybe_resize()`, add cache logic, add `warm_image_cache()`, add alpha detection
- `shore-daemon/src/handler/mod.rs` — thread `cache_dir` through `build_llm_messages`, add `warm_image_cache` call
- `shore-daemon/src/autonomy/manager.rs` — add `warm_image_cache` call at autonomy call site

## Testing

- Unit tests for alpha detection (opaque RGBA, transparent RGBA, RGB-only)
- Unit tests for format decision logic (PNG with alpha stays PNG, opaque PNG → JPEG, JPEG stays JPEG)
- Unit tests for quality-first path (image ≤ 2048px should reduce quality, not dimensions)
- Unit tests for dimension estimation math
- Unit tests for cache key generation and cache hit/miss paths
- Integration test: oversized image → cached resize → second call uses cache
- Existing tests updated to pass `cache_dir` parameter
