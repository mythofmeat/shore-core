/**
 * SQLite-backed append-only ledger for LLM call recording.
 *
 * Mirrors `backend/ledger/src/ledger.rs` and uses Bun's built-in sqlite
 * binding. The schema is table/column-compatible so Rust and TS daemons
 * can share `ledger.db`.
 */

import { Database } from "bun:sqlite";
import fs from "node:fs";
import path from "node:path";

import type { CacheForensics } from "./cache_forensics.ts";
import { CacheTracker, type CacheAnomaly, type CacheState } from "./cache_tracker.ts";
import { isAnthropicPricing, toOpenRouterId, type PricingEngine } from "./pricing.ts";

type SqlValue = string | bigint | NodeJS.TypedArray | number | boolean | null;
type NamedBindings = Record<string, SqlValue>;
type SqlRow = Record<string, unknown>;

const SCHEMA = `
CREATE TABLE IF NOT EXISTS calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                  TEXT    NOT NULL,
    character           TEXT    NOT NULL,
    provider            TEXT    NOT NULL,
    api_key_name        TEXT,
    model               TEXT    NOT NULL,
    call_type           TEXT    NOT NULL,
    input_tokens        INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    cache_read_tokens   INTEGER NOT NULL,
    cache_write_tokens  INTEGER NOT NULL,
    cache_ttl           TEXT    DEFAULT '1h',
    total_ms            INTEGER NOT NULL,
    ttft_ms             INTEGER NOT NULL,
    finish_reason       TEXT    NOT NULL,
    thinking_enabled    INTEGER NOT NULL,
    cache_state         TEXT,
    cache_anomaly       TEXT,
    input_cost          REAL,
    output_cost         REAL,
    cache_read_cost     REAL,
    cache_write_cost    REAL,
    cost_source         TEXT    DEFAULT 'pricing_catalog',
    total_cost          REAL
);

CREATE TABLE IF NOT EXISTS pricing (
    model_id              TEXT PRIMARY KEY,
    input_per_token       REAL NOT NULL,
    output_per_token      REAL NOT NULL,
    cache_read_per_token  REAL NOT NULL,
    cache_write_per_token REAL NOT NULL,
    fetched_at            TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS usage_budget_warnings (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    budget_name    TEXT NOT NULL,
    period_start   TEXT NOT NULL,
    threshold      TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    UNIQUE (budget_name, period_start, threshold)
);

CREATE INDEX IF NOT EXISTS idx_calls_ts        ON calls (ts);
CREATE INDEX IF NOT EXISTS idx_calls_character ON calls (character);
CREATE INDEX IF NOT EXISTS idx_calls_provider  ON calls (provider);
CREATE INDEX IF NOT EXISTS idx_calls_anomaly   ON calls (cache_anomaly) WHERE cache_anomaly IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_budget_warnings_window
    ON usage_budget_warnings (budget_name, period_start);
`;

const USAGE_KIND_EXPR = `CASE
    WHEN call_type = 'heartbeat_tool_loop' THEN 'heartbeat'
    WHEN call_type = 'message' AND finish_reason = 'tool_use' THEN 'message_with_tools'
    WHEN call_type = 'tool_loop' THEN 'message_with_tools'
    WHEN call_type = 'message' THEN 'message_no_tools'
    ELSE call_type
END`;

const TSV_HEADER = "ts\tcharacter\tprovider\tapi_key_name\tmodel\tcall_type\t" +
  "input_tokens\toutput_tokens\tcache_read_tokens\tcache_write_tokens\tcache_ttl\t" +
  "total_ms\tttft_ms\tfinish_reason\tthinking_enabled\t" +
  "cache_state\tcache_anomaly\t" +
  "input_cost\toutput_cost\tcache_read_cost\tcache_write_cost\tcost_source\ttotal_cost";

export interface CallRow {
  ts: string;
  character: string;
  provider: string;
  api_key_name?: string;
  model: string;
  call_type: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  cache_ttl?: string;
  total_ms: number;
  ttft_ms: number;
  finish_reason: string;
  thinking_enabled: boolean;
  cache_state?: CacheState;
  cache_anomaly?: CacheAnomaly;
  input_cost?: number;
  output_cost?: number;
  cache_read_cost?: number;
  cache_write_cost?: number;
  cost_source?: string;
  total_cost?: number;
}

export interface RecordCallInput {
  provider: string;
  apiKeyName?: string;
  model: string;
  callType: string;
  character: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  totalMs: number;
  ttftMs: number;
  finishReason: string;
  thinkingEnabled: boolean;
  cacheTtl?: string;
  ts?: string;
  /**
   * Provider-reported total cost (e.g. OpenRouter `cost`). When set, the row
   * is recorded with `cost_source = "provider_reported"` and per-component
   * costs are nulled to match the Rust ledger.
   */
  totalCostOverride?: number;
  /**
   * Optional pricing engine used to populate per-component costs from the
   * cached catalog. Best-effort: a missing entry leaves the cost columns
   * null without failing the insert.
   */
  pricing?: PricingEngine;
}

export interface QueryFilter {
  since?: string;
  until?: string;
  character?: string;
  provider?: string;
  api_key_name?: string;
  model?: string;
  call_type?: string;
  usage_kinds?: string[];
}

export interface UsageTotals {
  call_count: number;
  total_input: number;
  total_output: number;
  total_cache_read: number;
  total_cache_write: number;
  total_cost: number;
}

export interface UsageSummary {
  provider: string;
  model: string;
  call_count: number;
  total_input: number;
  total_output: number;
  total_cache_read: number;
  total_cache_write: number;
  total_cost: number;
}

export interface CallTypeSummary extends Omit<UsageTotals, "call_count"> {
  call_type: string;
  call_count: number;
}

export interface UsageKindSummary extends Omit<UsageTotals, "call_count"> {
  usage_kind: string;
  call_count: number;
}

export interface ApiKeySummary extends Omit<UsageTotals, "call_count"> {
  provider: string;
  api_key_name: string;
  call_count: number;
}

export class Ledger {
  private readonly cacheTrackers = new Map<string, CacheTracker>();

  private constructor(private readonly db: Database) {
    this.db.exec(SCHEMA);
    this.migrate();
  }

  static open(filePath: string): Ledger {
    fs.mkdirSync(path.dirname(filePath), { recursive: true });
    return new Ledger(new Database(filePath));
  }

  static openInMemory(): Ledger {
    return new Ledger(new Database(":memory:"));
  }

  close(): void {
    this.db.close();
  }

  insert(row: CallRow): number {
    const stmt = this.db.query<unknown, NamedBindings>(`
      INSERT INTO calls (
        ts, character, provider, api_key_name, model, call_type,
        input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
        cache_ttl, total_ms, ttft_ms, finish_reason, thinking_enabled,
        cache_state, cache_anomaly,
        input_cost, output_cost, cache_read_cost, cache_write_cost, cost_source, total_cost
      ) VALUES (
        $ts, $character, $provider, $api_key_name, $model, $call_type,
        $input_tokens, $output_tokens, $cache_read_tokens, $cache_write_tokens,
        $cache_ttl, $total_ms, $ttft_ms, $finish_reason, $thinking_enabled,
        $cache_state, $cache_anomaly,
        $input_cost, $output_cost, $cache_read_cost, $cache_write_cost, $cost_source, $total_cost
      )
    `);
    const result = stmt.run(rowToParams(row));
    return Number(result.lastInsertRowid);
  }

  recordCall(input: RecordCallInput, forensics?: CacheForensics): number {
    const ts = input.ts ?? new Date().toISOString();
    const hasCacheMetrics = input.cacheReadTokens > 0 || input.cacheWriteTokens > 0;
    let cacheState: CacheState | undefined;
    let cacheAnomaly: CacheAnomaly | undefined;

    if (
      callTypeAffectsCacheTracker(input.callType) &&
      (hasCacheMetrics || isAnthropicPricing(input.provider, input.model))
    ) {
      let tracker = this.cacheTrackers.get(input.character);
      if (tracker === undefined) {
        tracker = new CacheTracker();
        this.cacheTrackers.set(input.character, tracker);
      }
      const observed = tracker.observe({
        ts,
        model: input.model,
        thinkingEnabled: input.thinkingEnabled,
        cacheReadTokens: input.cacheReadTokens,
        cacheWriteTokens: input.cacheWriteTokens,
        callType: input.callType,
      });
      cacheState = observed.state;
      cacheAnomaly = observed.anomaly;
    }

    if (hasCacheMetrics && forensics !== undefined) {
      forensics.logResponse({
        callId: 0,
        model: input.model,
        character: input.character,
        callType: input.callType,
        inputTokens: input.inputTokens,
        outputTokens: input.outputTokens,
        cacheReadTokens: input.cacheReadTokens,
        cacheCreationTokens: input.cacheWriteTokens,
      });
    }

    const totalCostOverride = input.totalCostOverride;
    const cost = totalCostOverride === undefined
      ? input.pricing?.calculateCost({
          provider: input.provider,
          model: input.model,
          inputTokens: input.inputTokens,
          outputTokens: input.outputTokens,
          cacheReadTokens: input.cacheReadTokens,
          cacheWriteTokens: input.cacheWriteTokens,
          cacheTtl: input.cacheTtl,
        })
      : undefined;

    return this.insert({
      ts,
      character: input.character,
      provider: input.provider,
      ...(input.apiKeyName !== undefined ? { api_key_name: input.apiKeyName } : {}),
      model: input.model,
      call_type: input.callType,
      input_tokens: input.inputTokens,
      output_tokens: input.outputTokens,
      cache_read_tokens: input.cacheReadTokens,
      cache_write_tokens: input.cacheWriteTokens,
      ...(input.cacheTtl !== undefined ? { cache_ttl: input.cacheTtl } : {}),
      total_ms: input.totalMs,
      ttft_ms: input.ttftMs,
      finish_reason: input.finishReason,
      thinking_enabled: input.thinkingEnabled,
      ...(cacheState !== undefined ? { cache_state: cacheState } : {}),
      ...(cacheAnomaly !== undefined ? { cache_anomaly: cacheAnomaly } : {}),
      ...(cost !== undefined ? { input_cost: cost.input } : {}),
      ...(cost !== undefined ? { output_cost: cost.output } : {}),
      ...(cost !== undefined ? { cache_read_cost: cost.cache_read } : {}),
      ...(cost !== undefined ? { cache_write_cost: cost.cache_write } : {}),
      cost_source: totalCostOverride !== undefined ? "provider_reported" : "pricing_catalog",
      ...(totalCostOverride !== undefined
        ? { total_cost: totalCostOverride }
        : cost !== undefined
          ? { total_cost: cost.total }
          : {}),
    });
  }

  /**
   * Rewrite per-component cost columns and `total_cost` for every row whose
   * `(provider, model)` resolves to the given OpenRouter model id and whose
   * cost_source is `pricing_catalog`. Provider-reported rows are left
   * untouched. Returns the number of rows updated.
   *
   * Mirrors `backend/daemon/src/commands/usage.rs::recalculate_costs` to
   * the extent the catalog port covers: per-model recalc against the
   * memory/DB pricing cache.
   */
  recalculateCosts(modelId: string, pricing: PricingEngine): {
    updated: number;
    total: number;
    failures: Array<{ id: number; reason: string }>;
  } {
    const target = pricing.getCachedPricing(modelId);
    if (target === undefined) {
      return { updated: 0, total: 0, failures: [{ id: 0, reason: `no cached pricing for ${modelId}` }] };
    }

    const rows = this.db
      .query<{
        id: number;
        provider: string;
        model: string;
        input_tokens: number;
        output_tokens: number;
        cache_read_tokens: number;
        cache_write_tokens: number;
        cache_ttl: string | null;
        cost_source: string | null;
      }, []>(
        `SELECT id, provider, model, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, cache_ttl, cost_source
           FROM calls
          WHERE cost_source IS NULL OR cost_source = 'pricing_catalog'`,
      )
      .all();

    const matching = rows.filter((row) => toOpenRouterId(row.provider, row.model) === modelId);
    let updated = 0;
    const failures: Array<{ id: number; reason: string }> = [];
    const update = this.db.query(
      `UPDATE calls
          SET input_cost = $input_cost,
              output_cost = $output_cost,
              cache_read_cost = $cache_read_cost,
              cache_write_cost = $cache_write_cost,
              total_cost = $total_cost,
              cost_source = 'pricing_catalog'
        WHERE id = $id`,
    );
    for (const row of matching) {
      const cost = pricing.calculateCost({
        provider: row.provider,
        model: row.model,
        inputTokens: row.input_tokens,
        outputTokens: row.output_tokens,
        cacheReadTokens: row.cache_read_tokens,
        cacheWriteTokens: row.cache_write_tokens,
        cacheTtl: row.cache_ttl ?? undefined,
      });
      if (cost === undefined) {
        failures.push({ id: row.id, reason: `pricing missing for ${modelId}` });
        continue;
      }
      update.run({
        $input_cost: cost.input,
        $output_cost: cost.output,
        $cache_read_cost: cost.cache_read,
        $cache_write_cost: cost.cache_write,
        $total_cost: cost.total,
        $id: row.id,
      });
      updated++;
    }
    return { updated, total: matching.length, failures };
  }

  recent(limit: number): CallRow[] {
    return allRows(
      this.db,
      "SELECT * FROM calls ORDER BY id DESC LIMIT $limit",
      { $limit: limit },
    ).map(rowFromSqlite);
  }

  lastAnthropicCall(character: string): CallRow | undefined {
    const row = getRow(
      this.db,
      `SELECT * FROM calls
        WHERE character = $character
          AND provider = 'anthropic'
          AND call_type != 'compaction'
        ORDER BY id DESC
        LIMIT 1`,
      { $character: character },
    );
    return row === undefined ? undefined : rowFromSqlite(row);
  }

  usageTotals(filter: QueryFilter = {}): UsageTotals {
    const { whereClause, params } = buildWhere(filter);
    const row = getRow(
      this.db,
      `SELECT COUNT(*) as call_count,
              COALESCE(SUM(input_tokens), 0) as total_input,
              COALESCE(SUM(output_tokens), 0) as total_output,
              COALESCE(SUM(cache_read_tokens), 0) as total_cache_read,
              COALESCE(SUM(cache_write_tokens), 0) as total_cache_write,
              TOTAL(total_cost) as total_cost
         FROM calls
         ${whereClause}`,
      params,
    );
    return totalsFromRow(row ?? {});
  }

  usageSummary(filter: QueryFilter = {}): UsageSummary[] {
    const { whereClause, params } = buildWhere(filter);
    return allRows(
      this.db,
      `SELECT provider, model,
              COUNT(*) as call_count,
              COALESCE(SUM(input_tokens), 0) as total_input,
              COALESCE(SUM(output_tokens), 0) as total_output,
              COALESCE(SUM(cache_read_tokens), 0) as total_cache_read,
              COALESCE(SUM(cache_write_tokens), 0) as total_cache_write,
              TOTAL(total_cost) as total_cost
         FROM calls
         ${whereClause}
        GROUP BY provider, model
        ORDER BY total_cost DESC`,
      params,
    ).map((row) => ({
      provider: String(row["provider"]),
      model: String(row["model"]),
      ...totalsFromRow(row),
    }));
  }

  usageSummaryByCallType(filter: QueryFilter = {}): CallTypeSummary[] {
    const { whereClause, params } = buildWhere(filter);
    return allRows(
      this.db,
      `SELECT call_type,
              COUNT(*) as call_count,
              COALESCE(SUM(input_tokens), 0) as total_input,
              COALESCE(SUM(output_tokens), 0) as total_output,
              COALESCE(SUM(cache_read_tokens), 0) as total_cache_read,
              COALESCE(SUM(cache_write_tokens), 0) as total_cache_write,
              TOTAL(total_cost) as total_cost
         FROM calls
         ${whereClause}
        GROUP BY call_type
        ORDER BY total_cost DESC, call_count DESC`,
      params,
    ).map((row) => ({
      call_type: String(row["call_type"]),
      ...totalsFromRow(row),
    }));
  }

  usageSummaryByUsageKind(filter: QueryFilter = {}): UsageKindSummary[] {
    const { whereClause, params } = buildWhere(filter);
    return allRows(
      this.db,
      `SELECT usage_kind,
              COUNT(*) as call_count,
              COALESCE(SUM(input_tokens), 0) as total_input,
              COALESCE(SUM(output_tokens), 0) as total_output,
              COALESCE(SUM(cache_read_tokens), 0) as total_cache_read,
              COALESCE(SUM(cache_write_tokens), 0) as total_cache_write,
              TOTAL(total_cost) as total_cost
         FROM (
           SELECT ${USAGE_KIND_EXPR} as usage_kind,
                  input_tokens, output_tokens, cache_read_tokens,
                  cache_write_tokens, total_cost
             FROM calls
             ${whereClause}
         )
        GROUP BY usage_kind
        ORDER BY total_cost DESC, call_count DESC`,
      params,
    ).map((row) => ({
      usage_kind: String(row["usage_kind"]),
      ...totalsFromRow(row),
    }));
  }

  usageSummaryByApiKey(filter: QueryFilter = {}): ApiKeySummary[] {
    const { whereClause, params } = buildWhere(filter);
    return allRows(
      this.db,
      `SELECT provider,
              COALESCE(api_key_name, 'unknown') as api_key_name,
              COUNT(*) as call_count,
              COALESCE(SUM(input_tokens), 0) as total_input,
              COALESCE(SUM(output_tokens), 0) as total_output,
              COALESCE(SUM(cache_read_tokens), 0) as total_cache_read,
              COALESCE(SUM(cache_write_tokens), 0) as total_cache_write,
              TOTAL(total_cost) as total_cost
         FROM calls
         ${whereClause}
        GROUP BY provider, COALESCE(api_key_name, 'unknown')
        ORDER BY total_cost DESC, call_count DESC`,
      params,
    ).map((row) => ({
      provider: String(row["provider"]),
      api_key_name: String(row["api_key_name"]),
      ...totalsFromRow(row),
    }));
  }

  queryAnomalies(filter: QueryFilter = {}): CallRow[] {
    const { whereClause, params } = buildWhere(filter);
    const anomalyClause = whereClause.length === 0
      ? "WHERE cache_anomaly IS NOT NULL"
      : `${whereClause} AND cache_anomaly IS NOT NULL`;
    return allRows(
      this.db,
      `SELECT * FROM calls ${anomalyClause} ORDER BY id DESC`,
      params,
    ).map(rowFromSqlite);
  }

  exportTsv(filter: QueryFilter = {}): string {
    const { whereClause, params } = buildWhere(filter);
    const rows = allRows(
      this.db,
      `SELECT * FROM calls ${whereClause} ORDER BY id ASC`,
      params,
    ).map(rowFromSqlite);
    return [TSV_HEADER, ...rows.map(rowToTsv)].join("\n");
  }

  activeAnthropicCharacters(filter: QueryFilter = {}): Array<[string, CallRow]> {
    const { whereClause, params } = buildWhere(filter);
    const anthropicCond = "(provider = 'anthropic' OR model LIKE 'anthropic/%')";
    const providerCond = whereClause.length === 0
      ? `WHERE ${anthropicCond}`
      : `${whereClause} AND ${anthropicCond}`;
    return allRows(
      this.db,
      `SELECT c.* FROM calls c
        INNER JOIN (
          SELECT character, MAX(id) as max_id
          FROM calls
          ${providerCond}
          GROUP BY character
        ) latest ON c.id = latest.max_id
        ORDER BY c.id DESC`,
      params,
    ).map((row) => {
      const call = rowFromSqlite(row);
      return [call.character, call];
    });
  }

  warmStreak(character: string): number {
    const rows = allRows(
      this.db,
      "SELECT cache_state FROM calls WHERE character = $character ORDER BY id DESC LIMIT 10000",
      { $character: character },
    );
    let count = 0;
    for (const row of rows) {
      if (row["cache_state"] === "warm") {
        count++;
      } else {
        break;
      }
    }
    return count;
  }

  private migrate(): void {
    addColumnIfMissing(this.db, "calls", "cache_ttl", "TEXT DEFAULT '1h'");
    addColumnIfMissing(this.db, "calls", "api_key_name", "TEXT");
    addColumnIfMissing(this.db, "calls", "cost_source", "TEXT DEFAULT 'pricing_catalog'");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_calls_api_key ON calls (provider, api_key_name)");
    this.db.exec(`
      UPDATE calls
         SET cost_source = 'provider_reported'
       WHERE total_cost IS NOT NULL
         AND input_cost IS NULL
         AND output_cost IS NULL
         AND cache_read_cost IS NULL
         AND cache_write_cost IS NULL
    `);
  }
}

/**
 * Internal accessor for sibling modules (PricingEngine, budget evaluation)
 * that need to share the same SQLite handle. Intentionally not part of
 * Ledger's public surface.
 */
export function ledgerDatabase(ledger: Ledger): Database {
  // Reach through the private field. `Ledger`'s constructor + `open` flows
  // keep the DB lifecycle bound to the Ledger instance, so callers must not
  // close this handle directly.
  return (ledger as unknown as { db: Database }).db;
}

function rowToParams(row: CallRow): NamedBindings {
  return {
    $ts: row.ts,
    $character: row.character,
    $provider: row.provider,
    $api_key_name: row.api_key_name ?? null,
    $model: row.model,
    $call_type: row.call_type,
    $input_tokens: row.input_tokens,
    $output_tokens: row.output_tokens,
    $cache_read_tokens: row.cache_read_tokens,
    $cache_write_tokens: row.cache_write_tokens,
    $cache_ttl: row.cache_ttl ?? null,
    $total_ms: row.total_ms,
    $ttft_ms: row.ttft_ms,
    $finish_reason: row.finish_reason,
    $thinking_enabled: row.thinking_enabled ? 1 : 0,
    $cache_state: row.cache_state ?? null,
    $cache_anomaly: row.cache_anomaly ?? null,
    $input_cost: row.input_cost ?? null,
    $output_cost: row.output_cost ?? null,
    $cache_read_cost: row.cache_read_cost ?? null,
    $cache_write_cost: row.cache_write_cost ?? null,
    $cost_source: row.cost_source ?? null,
    $total_cost: row.total_cost ?? null,
  };
}

function rowFromSqlite(row: unknown): CallRow {
  const r = row as SqlRow;
  return {
    ts: String(r["ts"]),
    character: String(r["character"]),
    provider: String(r["provider"]),
    ...(typeof r["api_key_name"] === "string"
      ? { api_key_name: r["api_key_name"] }
      : {}),
    model: String(r["model"]),
    call_type: String(r["call_type"]),
    input_tokens: Number(r["input_tokens"]),
    output_tokens: Number(r["output_tokens"]),
    cache_read_tokens: Number(r["cache_read_tokens"]),
    cache_write_tokens: Number(r["cache_write_tokens"]),
    ...(typeof r["cache_ttl"] === "string" ? { cache_ttl: r["cache_ttl"] } : {}),
    total_ms: Number(r["total_ms"]),
    ttft_ms: Number(r["ttft_ms"]),
    finish_reason: String(r["finish_reason"]),
    thinking_enabled: Number(r["thinking_enabled"]) !== 0,
    ...(typeof r["cache_state"] === "string"
      ? { cache_state: r["cache_state"] as CacheState }
      : {}),
    ...(typeof r["cache_anomaly"] === "string"
      ? { cache_anomaly: r["cache_anomaly"] as CacheAnomaly }
      : {}),
    ...(typeof r["input_cost"] === "number" ? { input_cost: r["input_cost"] } : {}),
    ...(typeof r["output_cost"] === "number" ? { output_cost: r["output_cost"] } : {}),
    ...(typeof r["cache_read_cost"] === "number"
      ? { cache_read_cost: r["cache_read_cost"] }
      : {}),
    ...(typeof r["cache_write_cost"] === "number"
      ? { cache_write_cost: r["cache_write_cost"] }
      : {}),
    ...(typeof r["cost_source"] === "string" ? { cost_source: r["cost_source"] } : {}),
    ...(typeof r["total_cost"] === "number" ? { total_cost: r["total_cost"] } : {}),
  };
}

function addColumnIfMissing(
  db: Database,
  table: string,
  column: string,
  spec: string,
): void {
  const columns = db.query<SqlRow, []>(`PRAGMA table_info(${table})`).all();
  if (columns.some((col) => col["name"] === column)) return;
  db.exec(`ALTER TABLE ${table} ADD COLUMN ${column} ${spec}`);
}

function callTypeAffectsCacheTracker(callType: string): boolean {
  return callType !== "dreaming";
}

interface WhereClause {
  whereClause: string;
  params: NamedBindings;
}

function buildWhere(filter: QueryFilter): WhereClause {
  const clauses: string[] = [];
  const params: NamedBindings = {};
  let idx = 0;

  const add = (expr: string, value: string): void => {
    const key = `$p${idx}`;
    idx++;
    clauses.push(expr.replace("?", key));
    params[key] = value;
  };

  if (filter.since !== undefined) add("ts >= ?", filter.since);
  if (filter.until !== undefined) add("ts <= ?", filter.until);
  if (filter.character !== undefined) add("character = ?", filter.character);
  if (filter.provider !== undefined) add("provider = ?", filter.provider);
  if (filter.api_key_name !== undefined) {
    add("COALESCE(api_key_name, 'unknown') = ?", filter.api_key_name);
  }
  if (filter.model !== undefined) add("model = ?", filter.model);
  if (filter.call_type !== undefined) add("call_type = ?", filter.call_type);

  if (filter.usage_kinds !== undefined && filter.usage_kinds.length > 0) {
    const placeholders: string[] = [];
    for (const kind of filter.usage_kinds) {
      const key = `$p${idx}`;
      idx++;
      params[key] = kind;
      placeholders.push(key);
    }
    clauses.push(`(${USAGE_KIND_EXPR}) IN (${placeholders.join(", ")})`);
  }

  return {
    whereClause: clauses.length === 0 ? "" : `WHERE ${clauses.join(" AND ")}`,
    params,
  };
}

function allRows(db: Database, sql: string, params: NamedBindings): SqlRow[] {
  if (Object.keys(params).length === 0) {
    return db.query<SqlRow, []>(sql).all();
  }
  return db.query<SqlRow, NamedBindings>(sql).all(params);
}

function getRow(db: Database, sql: string, params: NamedBindings): SqlRow | undefined {
  if (Object.keys(params).length === 0) {
    return db.query<SqlRow, []>(sql).get() ?? undefined;
  }
  return db.query<SqlRow, NamedBindings>(sql).get(params) ?? undefined;
}

function totalsFromRow(row: SqlRow): UsageTotals {
  return {
    call_count: Number(row["call_count"] ?? 0),
    total_input: Number(row["total_input"] ?? 0),
    total_output: Number(row["total_output"] ?? 0),
    total_cache_read: Number(row["total_cache_read"] ?? 0),
    total_cache_write: Number(row["total_cache_write"] ?? 0),
    total_cost: Number(row["total_cost"] ?? 0),
  };
}

function rowToTsv(row: CallRow): string {
  return [
    row.ts,
    row.character,
    row.provider,
    row.api_key_name ?? "",
    row.model,
    row.call_type,
    String(row.input_tokens),
    String(row.output_tokens),
    String(row.cache_read_tokens),
    String(row.cache_write_tokens),
    row.cache_ttl ?? "",
    String(row.total_ms),
    String(row.ttft_ms),
    row.finish_reason,
    String(row.thinking_enabled),
    row.cache_state ?? "",
    row.cache_anomaly ?? "",
    optNumber(row.input_cost),
    optNumber(row.output_cost),
    optNumber(row.cache_read_cost),
    optNumber(row.cache_write_cost),
    row.cost_source ?? "",
    optNumber(row.total_cost),
  ].join("\t");
}

function optNumber(value: number | undefined): string {
  return value === undefined ? "" : String(value);
}
