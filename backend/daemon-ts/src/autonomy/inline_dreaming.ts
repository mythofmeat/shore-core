/**
 * Inline-dreaming runner — the autonomy-tick bridge that actually calls
 * `runLibrarianSweep`.
 *
 * Mirror of `backend/daemon/src/autonomy/manager.rs::execute_scheduled_dream`
 * (line 1279). The autonomy tick decides whether the per-character
 * backoff window is past; this runner does the rest: build the provider
 * + resolved-model + tool context, invoke `runLibrarianSweep`, and
 * translate the result into the autonomy notify calls.
 *
 * Outcome handling matches Rust:
 *   - sweep returns DreamSweepResult → notifyDreamingSuccess (clear
 *     failure count + next attempt)
 *   - sweep returns undefined (cron gate said "not due" or config
 *     disabled) → also notifyDreamingSuccess (clear any stale backoff;
 *     no work happened, but it's not a failure either)
 *   - throws → notifyDreamingFailed (increment failure count + back
 *     off via background_retry_delay)
 *
 * Skip-when-deps-missing: matches the way `execute_scheduled_dream`
 * early-returns when `llm_client` / `loaded_config` are absent. In TS
 * land the equivalent is the chat model being unresolvable; we surface
 * that as a failure (with the backoff applied) so it shows up in logs
 * instead of silently going dark.
 */

import { resolveApiKey, buildProvider } from "../llm/generate.ts";
import { resolveModel } from "../llm/catalog.ts";
import { runLibrarianSweep } from "../memory/dreaming.ts";

import type { LoadedConfig } from "../config/loader.ts";
import type { EngineRegistry } from "../engine/engine.ts";
import type { Ledger } from "../ledger/ledger.ts";
import type { CacheForensics } from "../ledger/cache_forensics.ts";
import type { ResolvedModel } from "../llm/catalog.ts";
import type { Embedder } from "../llm/embed.ts";
import type { DreamingConfig } from "../memory/dreaming.ts";
import type {
  ImageGenConfig,
  RetrievalConfig,
  SearchConfig,
} from "../tools/registry.ts";
import { resolveDisplayName } from "../config/loader.ts";

import type { AutonomyRegistry } from "./registry.ts";

export interface InlineDreamingDeps {
  engines: EngineRegistry;
  config: LoadedConfig;
  /** Resolved Shore data directory. */
  dataDir: string;
  /** Resolved Shore config directory. */
  configDir: string;
  /** Resolved Shore cache directory (workspace index lives here). */
  cacheDir: string;
  dreamingConfig: DreamingConfig;
  catalog: Map<string, ResolvedModel>;
  ledger: Ledger;
  cacheForensics?: CacheForensics;
  /** Optional embedder for librarian's file_search hybrid mode. */
  embedder?: Embedder;
  searchConfig?: SearchConfig;
  retrievalConfig?: RetrievalConfig;
  imageGenConfig?: ImageGenConfig;
  autonomy: AutonomyRegistry;
}

export type InlineDreamingRunner = (characterName: string) => Promise<void>;

export function buildInlineDreamingRunner(
  deps: InlineDreamingDeps,
): InlineDreamingRunner {
  return async (characterName) => {
    const modelName = deps.config.app.defaults.model;
    if (modelName === undefined || modelName.length === 0) {
      console.warn(
        `[shore-daemon-ts] scheduled dream skipped for ${characterName}: no app.defaults.model set`,
      );
      deps.autonomy.notifyDreamingFailed(characterName);
      return;
    }

    let resolved: ResolvedModel;
    try {
      resolved = resolveModel(deps.catalog, modelName);
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] scheduled dream skipped for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyDreamingFailed(characterName);
      return;
    }

    let apiKey: string;
    try {
      apiKey = resolveApiKey(resolved);
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] scheduled dream skipped for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyDreamingFailed(characterName);
      return;
    }

    const engine = deps.engines.get(characterName);
    const provider = buildProvider(resolved.sdk);
    const displayName = resolveDisplayName(deps.config);

    try {
      await runLibrarianSweep({
        configDir: deps.configDir,
        dataDir: deps.dataDir,
        cacheDir: deps.cacheDir,
        character: characterName,
        displayName,
        resolved,
        apiKey,
        provider,
        engine,
        dreamingConfig: deps.dreamingConfig,
        ledger: deps.ledger,
        ...(deps.cacheForensics !== undefined
          ? { cacheForensics: deps.cacheForensics }
          : {}),
        ...(deps.embedder !== undefined ? { embedder: deps.embedder } : {}),
        ...(deps.searchConfig !== undefined
          ? { searchConfig: deps.searchConfig }
          : {}),
        ...(deps.retrievalConfig !== undefined
          ? { retrievalConfig: deps.retrievalConfig }
          : {}),
        ...(deps.imageGenConfig !== undefined
          ? { imageGenConfig: deps.imageGenConfig }
          : {}),
      });

      // success OR skip (cron gate said "not due") — both clear backoff.
      deps.autonomy.notifyDreamingSuccess(characterName);
    } catch (e) {
      console.warn(
        `[shore-daemon-ts] scheduled dream failed for ${characterName}: ${(e as Error).message}`,
      );
      deps.autonomy.notifyDreamingFailed(characterName);
    }
  };
}
