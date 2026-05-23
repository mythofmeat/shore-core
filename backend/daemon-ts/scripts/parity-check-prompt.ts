#!/usr/bin/env bun
/**
 * Prompt-assembly parity harness.
 *
 * For each fixture under `tests/fixtures/prompt/`, pipe the JSON through:
 *   1. The Rust example binary (built from
 *      `backend/daemon/examples/dump_assemble_prompt.rs`)
 *   2. The TS dump helper at `scripts/_dump-assemble-prompt.ts`
 *
 * Both are spawned with TZ=America/Los_Angeles so time-marker fixtures are
 * reproducible across hosts. The two AssembledPrompt JSONs are deep-diffed
 * and any divergence is reported with a dotted-path mismatch list.
 *
 * Prereq:  cargo build -p shore-daemon --example dump_assemble_prompt
 * Run:     bun run scripts/parity-check-prompt.ts
 */
import { spawn } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";

const TZ = "America/Los_Angeles";
const HERE = path.dirname(new URL(import.meta.url).pathname);
const DAEMON_TS_ROOT = path.resolve(HERE, "..");
const REPO_ROOT = path.resolve(DAEMON_TS_ROOT, "../..");
const FIXTURES_DIR = path.join(DAEMON_TS_ROOT, "tests/fixtures/prompt");
const RUST_BINARY = path.join(
  REPO_ROOT,
  "target/debug/examples/dump_assemble_prompt",
);
const TS_DUMPER = path.join(HERE, "_dump-assemble-prompt.ts");

interface RunResult {
  stdout: string;
  exitCode: number;
}

function pipeJson(
  cmd: string,
  args: string[],
  input: string,
): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      env: { ...process.env, TZ },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => {
      stdout += d.toString("utf8");
    });
    child.stderr.on("data", (d) => {
      stderr += d.toString("utf8");
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) {
        reject(
          new Error(
            `${cmd} ${args.join(" ")} exited ${code}\nstderr: ${stderr}`,
          ),
        );
        return;
      }
      resolve({ stdout, exitCode: code });
    });
    child.stdin.write(input);
    child.stdin.end();
  });
}

function deepDiff(a: unknown, b: unknown, path = ""): string[] {
  if (a === b) return [];
  if (
    typeof a !== typeof b ||
    a === null ||
    b === null ||
    Array.isArray(a) !== Array.isArray(b)
  ) {
    return [`${path || "<root>"}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`];
  }
  if (Array.isArray(a) && Array.isArray(b)) {
    const diffs: string[] = [];
    const n = Math.max(a.length, b.length);
    if (a.length !== b.length) {
      diffs.push(`${path}.length: ${a.length} !== ${b.length}`);
    }
    for (let i = 0; i < n; i++) {
      diffs.push(...deepDiff(a[i], b[i], `${path}[${i}]`));
    }
    return diffs;
  }
  if (typeof a === "object" && typeof b === "object") {
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
    const diffs: string[] = [];
    for (const k of keys) {
      diffs.push(...deepDiff(ao[k], bo[k], path ? `${path}.${k}` : k));
    }
    return diffs;
  }
  return [`${path}: ${JSON.stringify(a)} !== ${JSON.stringify(b)}`];
}

async function main(): Promise<void> {
  try {
    readFileSync(RUST_BINARY);
  } catch {
    console.error(
      `error: ${RUST_BINARY} not found.\n` +
        "build it first: cargo build -p shore-daemon --example dump_assemble_prompt",
    );
    process.exit(2);
  }

  const fixtures = readdirSync(FIXTURES_DIR)
    .filter((n) => n.endsWith(".json"))
    .sort();

  let failed = 0;
  for (const name of fixtures) {
    const fixturePath = path.join(FIXTURES_DIR, name);
    const input = readFileSync(fixturePath, "utf8");
    let rustOut: string;
    let tsOut: string;
    try {
      const [rust, ts] = await Promise.all([
        pipeJson(RUST_BINARY, [], input),
        pipeJson("bun", ["run", TS_DUMPER], input),
      ]);
      rustOut = rust.stdout;
      tsOut = ts.stdout;
    } catch (e) {
      console.error(`[${name}] spawn error: ${(e as Error).message}`);
      failed++;
      continue;
    }

    const rustJson: unknown = JSON.parse(rustOut);
    const tsJson: unknown = JSON.parse(tsOut);
    const diffs = deepDiff(rustJson, tsJson);

    if (diffs.length === 0) {
      console.log(`[${name}] ok`);
    } else {
      failed++;
      console.log(`[${name}] FAIL (${diffs.length} diffs):`);
      for (const d of diffs.slice(0, 20)) console.log(`    ${d}`);
      if (diffs.length > 20) {
        console.log(`    … and ${diffs.length - 20} more`);
      }
    }
  }

  if (failed > 0) {
    console.error(`\n${failed}/${fixtures.length} fixtures diverged`);
    process.exit(1);
  }
  console.log(`\nall ${fixtures.length} fixtures match`);
}

await main();
