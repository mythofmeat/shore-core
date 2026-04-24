# Tool Search And Deferred Loading

Status: idea, not current implementation.

Shore currently sends the normal visible tool set, filtered by private mode and config toggles. It does not use Anthropic `defer_loading` or a custom tool-search server tool.

Why revisit later:

- Shore's tool catalog is growing.
- Some tools are rare, while memory/workspace/web tools are core.
- A smaller visible tool set may improve tool choice and cache economics.

Constraints if implemented:

- Must not break Anthropic prompt-cache stability.
- Must degrade cleanly on non-Anthropic providers.
- Must keep memory/private-mode gates exact.
- Must preserve observability in ledger/diagnostics.

Likely shape:

- Add metadata to `ToolDef` for hot/default/deferred behavior.
- Keep memory and core workspace tools visible by default.
- Defer novelty or rare tools only after measuring behavior.
- Strip unsupported provider-specific fields outside Anthropic.

Do not implement this until tool-selection quality is measurably a problem.
