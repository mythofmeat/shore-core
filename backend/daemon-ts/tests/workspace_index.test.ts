import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import type { Embedder } from "../src/llm/embed.ts";
import {
  cosineSimilarity,
  hybridSearch,
} from "../src/memory/workspace_index.ts";
import { defaultRetrievalConfig } from "../src/tools/registry.ts";

class TopicEmbedder implements Embedder {
  callCount = 0;
  inputCount = 0;

  constructor(private readonly topics: string[]) {}

  async embed(inputs: string[]): Promise<number[][]> {
    this.callCount += 1;
    this.inputCount += inputs.length;
    return inputs.map((input) => this.vectorFor(input));
  }

  modelId(): string {
    return "topic-test";
  }

  dimensions(): number {
    return this.topics.length;
  }

  private vectorFor(text: string): number[] {
    const lower = text.toLowerCase();
    return this.topics.map((topic) => (lower.includes(topic) ? 1 : 0));
  }
}

function setup(): { workspace: string; indexPath: string } {
  const root = mkdtempSync(path.join(tmpdir(), "shore-index-test-"));
  const workspace = path.join(root, "workspace");
  const indexPath = path.join(root, "cache", "workspace_index.json");
  fs.mkdirSync(workspace, { recursive: true });
  return { workspace, indexPath };
}

function writeFile(root: string, rel: string, body: string): void {
  const full = path.join(root, rel);
  fs.mkdirSync(path.dirname(full), { recursive: true });
  fs.writeFileSync(full, body);
}

describe("workspace_index hybrid search", () => {
  it("cosine handles zero and mismatched vectors", () => {
    expect(cosineSimilarity([0, 0], [1, 0])).toBe(0);
    expect(cosineSimilarity([1, 0], [1, 0, 0])).toBe(0);
  });

  it("promotes a semantic-only match in vector mode", async () => {
    const { workspace, indexPath } = setup();
    writeFile(workspace, "a.md", "# Notes\n\nThe garden is full of tea plants.");
    writeFile(workspace, "b.md", "# Other\n\nThe accountant filed taxes.");

    const embedder = new TopicEmbedder(["tea", "garden", "tax"]);
    const result = await hybridSearch(
      workspace,
      defaultRetrievalConfig(),
      "growing tea in the garden",
      "vector",
      embedder,
      indexPath,
    );

    expect(result.files[0]?.displayPath).toBe("a.md");
    expect(result.files[0]?.semanticScore ?? 0).toBeGreaterThan(0);
  });

  it("reuses cached file vectors on unchanged workspaces", async () => {
    const { workspace, indexPath } = setup();
    writeFile(workspace, "a.md", "tea notes");
    writeFile(workspace, "b.md", "rust notes");

    const embedder = new TopicEmbedder(["tea", "rust"]);
    await hybridSearch(
      workspace,
      defaultRetrievalConfig(),
      "tea",
      "hybrid",
      embedder,
      indexPath,
    );
    const callsAfterFirst = embedder.callCount;
    const inputsAfterFirst = embedder.inputCount;

    await hybridSearch(
      workspace,
      defaultRetrievalConfig(),
      "tea",
      "hybrid",
      embedder,
      indexPath,
    );

    expect(embedder.callCount).toBe(callsAfterFirst + 1);
    expect(embedder.inputCount).toBe(inputsAfterFirst + 1);
  });

  it("records oversize files without embedding them", async () => {
    const { workspace, indexPath } = setup();
    const cfg = { ...defaultRetrievalConfig(), max_file_bytes: 4 };
    writeFile(workspace, "small.md", "tea");
    writeFile(workspace, "large.md", "tea that is too large");

    const embedder = new TopicEmbedder(["tea"]);
    const result = await hybridSearch(
      workspace,
      cfg,
      "tea",
      "hybrid",
      embedder,
      indexPath,
    );

    expect(result.skippedBinaryOrLarge).toBe(1);
    expect(result.embeddedFiles).toBe(1);
    const index = JSON.parse(fs.readFileSync(indexPath, "utf8"));
    expect(index.entries["large.md"].embedded).toBe(false);
    expect(index.entries["large.md"].reason).toBe("oversize");
  });
});
