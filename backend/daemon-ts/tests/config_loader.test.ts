import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { loadConfig } from "../src/config/loader.ts";

function setupConfig(body: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), "shore-config-test-"));
  fs.writeFileSync(path.join(dir, "config.toml"), body);
  return dir;
}

describe("config loader retrieval slices", () => {
  it("loads defaults.embedding, embedding profiles, and memory retrieval caps", () => {
    const dir = setupConfig(`
[defaults]
model = "haiku"
embedding = "text-large"
display_name = "Ren"

[embedding.text-large]
model_id = "text-embedding-3-large"
api_key_env = "OPENAI_API_KEY"
dimensions = 3072

[memory.retrieval]
mode = "hybrid"
max_file_bytes = 1234
max_indexed_files = 77
max_total_indexed_bytes = 9999
max_embed_chars_per_file = 333
binary = "metadata"

[memory.dreaming]
enabled = true
frequency = "0 6 * * 1"
max_tool_rounds = 4

[advanced]
cache_forensics = true

[usage]
timezone = "utc"
allow_compaction_over_budget = false

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = false
fallback_heartbeat_interval = "2h"
dormant_after_heartbeat_turns = 7
dormant_after_idle_time = "3d"
minimum_heartbeat_latency = "45m"
max_tool_rounds = 5
wrap_up_grace_rounds = 2

[behavior.tool_use]
enabled = false
max_iterations = 3

[behavior.tool_use.tools]
roll_dice = false
web_search = false
`);

    const config = loadConfig(dir);
    expect(config.app.defaults.model).toBe("haiku");
    expect(config.app.defaults.embedding).toBe("text-large");
    expect(config.app.defaults.display_name).toBe("Ren");
    expect(config.embedding["text-large"]?.model_id).toBe("text-embedding-3-large");
    expect(config.memory.retrieval).toEqual({
      mode: "hybrid",
      max_file_bytes: 1234,
      max_indexed_files: 77,
      max_total_indexed_bytes: 9999,
      max_embed_chars_per_file: 333,
      binary: "metadata",
    });
    expect(config.memory.dreaming).toEqual({
      enabled: true,
      frequency: "0 6 * * 1",
      max_tool_rounds: 4,
    });
    expect(config.app.advanced.cache_forensics).toBe(true);
    expect(config.app.usage).toEqual({
      timezone: "utc",
      allow_compaction_over_budget: false,
      budgets: [],
      spike_warnings: {
        enabled: false,
        period: "hour",
        multiplier: 3,
        min_cost_usd: 1,
      },
    });
    expect(config.app.behavior.autonomy).toEqual({
      enabled: true,
      heartbeat: {
        enabled: false,
        fallbackHeartbeatIntervalSecs: 7200,
        dormantAfterHeartbeatTurns: 7,
        dormantAfterIdleTimeSecs: 259200,
        minimumHeartbeatLatencySecs: 2700,
        maxToolRounds: 5,
        wrapUpGraceRounds: 2,
      },
    });
    expect(config.app.behavior.tool_use).toEqual({
      enabled: false,
      max_iterations: 3,
      tools: {
        roll_dice: false,
        web_search: false,
      },
    });
  });

  it("parses [[usage.budgets]] entries and [usage.spike_warnings]", () => {
    const dir = setupConfig(`
[usage]
timezone = "utc"

[usage.spike_warnings]
enabled = true
period = "day"
multiplier = 2.5
min_cost_usd = 5

[[usage.budgets]]
name = "daily"
period = "day"
cost_usd = 25
warn_at = [0.5, 0.9]
limit = "block"
reset_hour = 6

[[usage.budgets]]
name = "monthly"
period = "month"
cost_usd = 500
reset_day_of_month = 15
usage_kind = ["heartbeat", "compaction"]
`);

    const config = loadConfig(dir);
    expect(config.app.usage.spike_warnings).toEqual({
      enabled: true,
      period: "day",
      multiplier: 2.5,
      min_cost_usd: 5,
    });
    expect(config.app.usage.budgets.length).toBe(2);
    expect(config.app.usage.budgets[0]).toMatchObject({
      name: "daily",
      period: "day",
      cost_usd: 25,
      warn_at: [0.5, 0.9],
      limit: "block",
      reset_hour: 6,
    });
    expect(config.app.usage.budgets[1]).toMatchObject({
      name: "monthly",
      period: "month",
      cost_usd: 500,
      reset_day_of_month: 15,
      usage_kind: ["heartbeat", "compaction"],
    });
  });

  it("notifications default to disabled with Rust-matching event defaults", () => {
    const dir = setupConfig("");
    const config = loadConfig({ configDir: dir });
    expect(config.app.notifications).toEqual({
      enabled: false,
      generation_threshold_ms: 0,
      events: {
        autonomous_message: true,
        compaction_complete: true,
        error: true,
        message_complete: false,
        usage_warning: true,
      },
    });
  });

  it("notifications honors [notifications] + [notifications.events] overrides", () => {
    const dir = setupConfig(`
[notifications]
enabled = true
generation_threshold = "30s"

[notifications.events]
message_complete = true
usage_warning = false
`);
    const config = loadConfig({ configDir: dir });
    expect(config.app.notifications).toEqual({
      enabled: true,
      generation_threshold_ms: 30_000,
      events: {
        autonomous_message: true,
        compaction_complete: true,
        error: true,
        message_complete: true,
        usage_warning: false,
      },
    });
  });

  it("loads an explicit config file and merges conf.d relative to its parent", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "shore-explicit-config-test-"));
    fs.mkdirSync(path.join(dir, "conf.d"));
    const file = path.join(dir, "preview.toml");
    fs.writeFileSync(file, `
[defaults]
model = "base"
`);
    fs.writeFileSync(path.join(dir, "conf.d", "10-overlay.toml"), `
[defaults]
display_name = "Preview User"
`);

    const config = loadConfig({ configDir: dir, configFile: file });
    expect(config.app.defaults.model).toBe("base");
    expect(config.app.defaults.display_name).toBe("Preview User");
  });
});
