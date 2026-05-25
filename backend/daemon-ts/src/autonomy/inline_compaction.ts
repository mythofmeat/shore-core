/**
 * Inline-compaction runner — the post-generation + idle-tick bridge that
 * actually calls `runCompaction` against a character's data root.
 *
 * Mirror of `backend/daemon/src/handler/task.rs::run_inline_compaction`
 * (and the `execute_idle_compaction` arm in
 * `backend/daemon/src/autonomy/manager.rs:1172` which uses the same call
 * sequence).
 *
 * Sequence:
 *   1. Emit `Phase{phase:"compacting"}` so the connected client can
 *      render a status hint.
 *   2. Pull `autonomy.cachedLastRequest(character)` — preserves the
 *      Anthropic prompt-cache prefix for the compaction call once the
 *      `RealCompactionLlm` cached-prefix path lands (today the value is
 *      passed through but `summarize` ignores it; tracked as audit
 *      blocker #12 in `REWRITE.md`).
 *   3. Run `runCompaction` against the character's data root.
 *   4. On success: `engine.reload()` so the next turn sees the post-
 *      compaction `active.jsonl` (otherwise the in-memory MessageStore
 *      would re-send the now-archived turns), then
 *      `autonomy.notifyCompactionComplete`.
 *   5. On failure: `autonomy.notifyCompactionFailed` so the trigger
 *      clears for retry.
 *
 * The runner is a closure factory so main.ts can build one with all the
 * long-lived deps (config, ledger, pricing, catalog, autonomy, broadcast,
 * engines) and pass the returned function both to the post-generation
 * hook AND to `AutonomyRegistry.onIdleCompaction`.
 */

import { resolveApiKey } from "../llm/generate.ts";
import { resolveModel } from "../llm/catalog.ts";
import { RealCompactionLlm } from "../memory/compaction/llm.ts";
import { runCompaction } from "../memory/compaction/background.ts";
import { resolveDisplayName } from "../config/loader.ts";

import type { LoadedConfig } from "../config/loader.ts";
import type { EngineRegistry } from "../engine/engine.ts";
import type { Ledger } from "../ledger/ledger.ts";
import type { CacheForensics } from "../ledger/cache_forensics.ts";
import type { NotificationService } from "../notifications/service.ts";
import type { ServerMessage } from "../swp/types.ts";
import type { ResolvedModel } from "../llm/catalog.ts";
import type { CompactionConfig } from "../memory/compaction/types.ts";

import type { AutonomyRegistry } from "./registry.ts";

export interface InlineCompactionDeps {
  engines: EngineRegistry;
  config: LoadedConfig;
  /** Resolved Shore data directory (matches Rust's `config.dirs.data`). */
  dataDir: string;
  /** Resolved Shore config directory (matches Rust's `config.dirs.config`). */
  configDir: string;
  compactionConfig: CompactionConfig;
  /** Catalog map from `loadCatalog` — short and qualified-name keyed. */
  catalog: Map<string, ResolvedModel>;
  ledger: Ledger;
  cacheForensics?: CacheForensics;
  autonomy: AutonomyRegistry;
  notifier: NotificationService;
  broadcast: (frame: ServerMessage) => void;
}

export type InlineCompactionRunner = (
  characterName: string,
  rid?: string,
) => Promise<void>;

export function buildInlineCompactionRunner(
  deps: InlineCompactionDeps,
): InlineCompactionRunner {
  return async (characterName, rid) => {
    deps.broadcast({
      type: "phase",
      phase: "compacting",
      ...(rid !== undefined ? { rid } : {}),
    });

    const modelName = deps.config.app.defaults.model;
    if (modelName === undefined || modelName.length === 0) {
      console.warn(
        `[shore-daemon-ts] inline compaction skipped for ${characterName}: no app.defaults.model set`,
      );
      deps.autonomy.notifyCompactionFailed(characterName);
      return;
    }

    let resolved: ResolvedModel;
    try {
      resolved = resolveModel(deps.catalog, modelName);
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] inline compaction skipped for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyCompactionFailed(characterName);
      return;
    }

    let apiKey: string;
    try {
      apiKey = resolveApiKey(resolved);
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] inline compaction skipped for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyCompactionFailed(characterName);
      return;
    }

    const cachedRequest = deps.autonomy.cachedLastRequest(characterName);
    const engine = deps.engines.get(characterName);

    const llm = new RealCompactionLlm({
      resolved,
      apiKey,
      ledger: deps.ledger,
      character: characterName,
      ...(deps.cacheForensics !== undefined
        ? { cacheForensics: deps.cacheForensics }
        : {}),
    });

    try {
      const result = await runCompaction({
        character: characterName,
        dataDir: deps.dataDir,
        configDir: deps.configDir,
        config: deps.compactionConfig,
        displayName: resolveDisplayName(deps.config),
        llm,
        ...(cachedRequest !== undefined ? { cachedRequest } : {}),
      });

      // Reload so the next chat turn / heartbeat tick reads the
      // post-compaction active.jsonl. Without this the in-memory
      // MessageStore would re-send the archived turns to the model.
      await engine.reload();

      deps.autonomy.notifyCompactionComplete(
        characterName,
        result.retainedTurns,
      );
      if (result.outcome?.kind === "compacted") {
        deps.notifier.notify(
          "compaction_complete",
          `Shore — ${characterName}`,
          `Compaction complete: ${result.outcome.result.memoryFilesWritten.length} entries from ${result.outcome.result.compactedTurns} turns`,
        );
      }
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] inline compaction failed for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyCompactionFailed(characterName);
      deps.notifier.notify(
        "error",
        `Shore — ${characterName}`,
        `Compaction failed: ${(e as Error).message}`,
      );
    }
  };
}
