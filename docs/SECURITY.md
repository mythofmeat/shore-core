# Security

Shore is a local-first daemon. Security guidance should be explicit about what
is and is not protected.

## Boundary Principles

- Validate external data at boundaries.
- Keep the daemon as the sole owner of character state.
- Do not imply authentication, authorization, or encryption where Shore does
  not provide it.
- Prefer narrow, mechanically tested boundaries over broad trust in callers or
  model behavior.

## Daemon Access

The daemon is localhost-only by default. Non-loopback binding requires:

```toml
[daemon]
unsafe_allow_remote_access = true
allowed_hosts = ["100.64.0.2"]
```

`allowed_hosts` is a source-IP allowlist only. It is not authentication or TLS.
Use a private overlay network such as Tailscale or WireGuard for remote access.

## Workspace Tools

Workspace tools operate inside a character workspace:

- paths must stay under `characters/<Character>/workspace/`;
- `memory/...` access must respect memory read/write gates;
- symlink and traversal escapes are bugs;
- protected prompt files must queue deferred activation.

`exec` is intentionally narrow:

- command strings are parsed to argv and executed directly;
- shell features are not supported;
- executable names are allowlisted;
- executable paths are rejected;
- path-like arguments must stay inside the character workspace.

## Secrets

Provider keys are read from environment variables or `.env` in the config
directory. Do not commit real keys, captured Authorization headers, or private
profile data.

## Matrix

Matrix is a client bridge, not a trusted state store. Embedded and external
homeserver modes must not bypass daemon-owned state or profile write gates.

## Agent Rule

Any change that widens daemon access, workspace access, tool dispatch, provider
request construction, or profile mutability must update this document, add or
adjust tests, and run `python3 scripts/harness-check.py`.
