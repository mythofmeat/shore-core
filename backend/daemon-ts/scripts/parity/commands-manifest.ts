/**
 * Manifest types + loader for command-dispatcher parity cases.
 * One entry per command case (one command can have multiple cases —
 * e.g. ok + error variants — by setting distinct `id`s).
 */

import { readFileSync } from "node:fs";

import type { FuzzyDiffs } from "./_lib.ts";

export interface CommandCase {
  /** Stable identifier; defaults to `name` if omitted. Lets one command
   *  carry multiple cases (`switch_character` vs `switch_character-missing`). */
  id?: string;
  /** SWP command name sent in the request frame. */
  name: string;
  /** Dispatcher args; undefined → {}. */
  args?: Record<string, unknown>;
  /** Request id to send. Defaults to "r1"; null omits rid. */
  rid?: string | null;
  /** Named fixture directory under `parity-traces/fixtures/`. */
  fixture: string;
  /** Character selected in the client hello frame. */
  character: string;
  /** Baseline JSONL filename, resolved relative to `parity-traces/commands/`. */
  baseline: string;
  /** s2c frames the daemon emits AFTER the command frame (excludes the
   *  hello + initial history). Mutators that also broadcast history have
   *  2 here. Explicit, not auto-detected. */
  expected_frames: number;
  /** Per-frame-type fuzzy paths merged on top of GLOBAL_FUZZY. */
  fuzzy?: FuzzyDiffs;
  /** "ok" — expect command_output. "error" — expect error frame.
   *  Soft-asserted by the runner so a regression that turns an error into
   *  a silent ok doesn't slip through fuzzy matching. */
  outcome: "ok" | "error";
}

export interface Manifest {
  cases: CommandCase[];
}

export function loadManifest(path: string): Manifest {
  const raw = readFileSync(path, "utf8");
  const parsed = JSON.parse(raw) as Manifest;
  if (!Array.isArray(parsed.cases)) {
    throw new Error(`manifest at ${path}: missing "cases" array`);
  }
  for (const c of parsed.cases) {
    if (typeof c.name !== "string" || c.name.length === 0) {
      throw new Error(`manifest: case missing name (id=${c.id ?? "<none>"})`);
    }
    if (typeof c.fixture !== "string") {
      throw new Error(`manifest: case "${caseId(c)}" missing fixture`);
    }
    if (typeof c.character !== "string") {
      throw new Error(`manifest: case "${caseId(c)}" missing character`);
    }
    if (typeof c.baseline !== "string") {
      throw new Error(`manifest: case "${caseId(c)}" missing baseline`);
    }
    if (c.rid !== undefined && c.rid !== null && typeof c.rid !== "string") {
      throw new Error(`manifest: case "${caseId(c)}" rid must be a string or null`);
    }
    if (!Number.isInteger(c.expected_frames) || c.expected_frames < 0) {
      throw new Error(`manifest: case "${caseId(c)}" expected_frames must be a non-negative integer`);
    }
    if (c.outcome !== "ok" && c.outcome !== "error") {
      throw new Error(`manifest: case "${caseId(c)}" outcome must be "ok" or "error"`);
    }
  }
  return parsed;
}

export function caseId(c: CommandCase): string {
  return c.id ?? c.name;
}

/**
 * Merge per-case fuzzy paths on top of GLOBAL_FUZZY. Both are
 * `Record<frameType, string[]>`. Per-case entries extend (not replace)
 * the global list for a given frame type.
 */
export function mergeFuzzy(global: FuzzyDiffs, perCase: FuzzyDiffs | undefined): FuzzyDiffs {
  if (perCase === undefined) return global;
  const out: FuzzyDiffs = { ...global };
  for (const [frameType, paths] of Object.entries(perCase)) {
    out[frameType] = [...(out[frameType] ?? []), ...paths];
  }
  return out;
}
