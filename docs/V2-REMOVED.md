# Shore V2 — Intentionally Removed / Replaced

V1 features that were consciously not ported to V2, either because they were
replaced by better alternatives or because they don't fit the V2 architecture.

Add items here as decisions are made.

## Replaced by Better V2 Alternatives

- **Flat models.toml with [[models]] array** — Replaced by nested
  [chat.provider.model] config structure with include/conf.d support.
  More expressive, matches V1's original design intent.

- **Separate models.toml file** — Merged into config.toml. Can still be
  split out via `include = ["models.toml"]` or `conf.d/models.toml` if desired.

- **provider_defaults section** — Replaced by hardcoded provider defaults
  (ported from V1's PROVIDER_DEFAULTS) plus inline provider-level scalars
  under [chat.provider]. More ergonomic — zero config for known providers.

- **Swipe CLI command** — Removed from CLI; still available daemon-side.
  Will be TUI-only (swipe gestures / keybindings make more sense in TUI context).

## Architecture Decisions

(Add items as they come up — e.g., features that turned out to be unused,
things that the new architecture handles differently, etc.)
