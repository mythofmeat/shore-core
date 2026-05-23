#!/usr/bin/env bun
/**
 * TS counterpart to `backend/daemon/examples/dump_assemble_prompt.rs`.
 * Reads PromptParams JSON on stdin, calls `assemblePrompt`, prints the
 * AssembledPrompt as JSON on stdout. Used by parity-check-prompt.ts.
 */
import { assemblePrompt, type PromptParams } from "../src/engine/prompt.ts";

async function main(): Promise<void> {
  const chunks: Uint8Array[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk as Uint8Array);
  }
  const input: PromptParams = JSON.parse(Buffer.concat(chunks).toString("utf8"));
  const out = assemblePrompt(input);
  process.stdout.write(JSON.stringify(out));
}

await main();
