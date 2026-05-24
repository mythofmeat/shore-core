# shore-daemon-ts Cutover Runbook

This runbook tracks the Phase 9 path from opt-in TypeScript daemon preview to
default daemon. It does not declare the cutover complete; it defines the
evidence required before `shore-daemon-ts` can replace the Rust daemon.

## Scope

- Preview binary: `/usr/bin/shore-daemon-ts`
- Preview service: `shore-daemon-ts.service`
- Existing Rust binary: `/usr/bin/shore-daemon`
- Existing Rust service: `shore-daemon.service`

Do not run the Rust and TypeScript daemon services against the same Shore
directories at the same time during the soak.

## Preflight

Run these checks on the commit that will be tagged:

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p shore-daemon -p shore-cli

cd backend/daemon-ts
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
bun run smoketest:compiled
```

Run live/provider checks when the release changed provider behavior, cache
placement, tool loops, or background LLM calls.

## Publish The Preview

Create a preview tag from the exact commit being soaked:

```sh
git tag -a shore-daemon-ts-v0.0.0_phase9_preview -m "shore-daemon-ts preview 0.0.0_phase9_preview"
git push origin shore-daemon-ts-v0.0.0_phase9_preview
```

Use underscores in the suffix; `makepkg` rejects hyphens in `pkgver`.

After pushing, verify:

- The `Build and publish Arch packages` workflow ran `publish-daemon-ts`.
- The `repo-arch` `latest` release contains a `shore-daemon-ts` package for
  the tag version.
- The caller tag release exists in `shore-core`.
- Normal CI is green for the same commit.

## Install And Start The Preview

Install the preview package from the configured Arch repo:

```sh
sudo pacman -Syu shore-daemon-ts
```

Switch the local daemon service to the preview:

```sh
systemctl --user stop shore-daemon.service
systemctl --user disable shore-daemon.service
systemctl --user enable --now shore-daemon-ts.service
```

Smoke the client path:

```sh
shore status
shore send "hello from the TS preview"
shore usage --by-kind --last today
journalctl --user -u shore-daemon-ts.service -n 100 --no-pager
```

## Soak Evidence

The one-release-cycle soak starts only after the preview package is published,
installed, and serving normal client traffic.

Record:

- Preview tag and commit SHA.
- Soak start date and release version of the Rust daemon shipped alongside it.
- Host/package version used for the preview.
- Daily client coverage: normal chat, regenerate, history/archive, memory
  compact, tool use, background compaction/dreaming if enabled, heartbeat if
  enabled, and usage/budget reporting.
- Any incidents, with date, symptom, logs retained locally, root cause, fix
  commit, and whether the soak clock restarted.

Exit requires one full release cycle with no live failures attributable to the
TypeScript daemon. A live failure that requires a code fix restarts the soak
clock from the fixed preview release.

## Default Switch

The default-switch PR must include a short decision note covering:

- Whether the `shore-daemon` package is replaced by the TypeScript binary or
  the Rust package is first renamed/split.
- The final installed binary names and service names.
- The migration path for users currently running `shore-daemon-ts.service`.
- The rollback path to the last Rust daemon build.
- The Rust-daemon retirement choice: move `backend/daemon` to `attic/` or
  delete it.

Do not mark Phase 9b complete until that PR lands and the Rust daemon is
retired by the chosen path.
