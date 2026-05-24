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
  });
});
