/**
 * exec sandbox tests — allowlist enforcement, path-arg validation, and
 * proof that no shell is invoked.
 *
 * Mirrors the exec test cases in `backend/daemon/src/tools/workspace.rs`.
 */
import { describe, expect, it } from "bun:test";
import fs from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import type { ToolContext } from "../src/tools/registry.ts";
import { ToolError } from "../src/tools/registry.ts";
import { execHandler, shellSplit } from "../src/tools/workspace.ts";

function setup(): { workspace: string; ctx: ToolContext } {
  const root = mkdtempSync(path.join(tmpdir(), "shore-exec-test-"));
  const workspace = path.join(root, "workspace");
  fs.mkdirSync(workspace, { recursive: true });
  return {
    workspace,
    ctx: {
      characterName: "test",
      characterConfigDir: root,
      characterDataDir: path.join(root, "data"),
      workspaceDir: workspace,
      configDir: root,
      imageDir: path.join(root, "images"),
      engine: undefined as unknown as ToolContext["engine"],
      searchConfig: {
        api_key_env: "TAVILY_API_KEY",
        max_results: 5,
        search_depth: "basic",
        include_answer: true,
      },
      retrievalConfig: { max_file_bytes: 1024 * 1024 },
    },
  };
}

describe("shellSplit (POSIX shell-words)", () => {
  it("splits on whitespace", () => {
    expect(shellSplit("ls -la /tmp")).toEqual(["ls", "-la", "/tmp"]);
  });

  it("preserves single-quoted content verbatim", () => {
    expect(shellSplit("echo 'hello world'")).toEqual(["echo", "hello world"]);
  });

  it("interprets double-quoted content with escapes", () => {
    expect(shellSplit('echo "she said \\"hi\\""')).toEqual([
      "echo",
      'she said "hi"',
    ]);
  });

  it("backslash escapes a single char outside quotes", () => {
    expect(shellSplit("foo\\ bar baz")).toEqual(["foo bar", "baz"]);
  });

  it("throws on unclosed quotes", () => {
    expect(() => shellSplit("echo 'unterminated")).toThrow();
    expect(() => shellSplit('echo "unterminated')).toThrow();
  });
});

describe("exec allowlist", () => {
  it("runs an allowlisted command", async () => {
    const { ctx } = setup();
    const r = JSON.parse(await execHandler.execute({ command: "pwd" }, ctx));
    expect(r.exit_code).toBe(0);
    expect(r.stdout).toContain("workspace");
  });

  it("rejects a non-allowlisted command", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute({ command: "rm -rf /" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects absolute-path command names", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute({ command: "/usr/bin/git status" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects shell chaining (parses as a single command)", async () => {
    const { ctx } = setup();
    // `pwd; pwd` shell-splits to ["pwd;", "pwd"]; the first token has
    // a semicolon attached, so it doesn't match the allowlist.
    expect(
      execHandler.execute({ command: "pwd; pwd" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects empty / whitespace-only commands", async () => {
    const { ctx } = setup();
    expect(execHandler.execute({ command: "" }, ctx)).rejects.toThrow(
      ToolError,
    );
    expect(execHandler.execute({ command: "   " }, ctx)).rejects.toThrow(
      ToolError,
    );
  });
});

describe("exec path-arg validation", () => {
  it("rejects absolute-path arguments", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute({ command: "cat /etc/passwd" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects parent-traversal arguments", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute({ command: "rg tea ../" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects absolute paths inside =value arguments", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute(
        { command: "cargo --manifest-path=/tmp/Cargo.toml test" },
        ctx,
      ),
    ).rejects.toThrow(ToolError);
  });

  it("rejects absolute paths in workdir arguments (`git -C /tmp status`)", async () => {
    const { ctx } = setup();
    expect(
      execHandler.execute({ command: "git -C /tmp status" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("rejects file: URL arguments", async () => {
    const { workspace, ctx } = setup();
    fs.writeFileSync(path.join(workspace, "foo.txt"), "x");
    expect(
      execHandler.execute({ command: "cat file:///etc/passwd" }, ctx),
    ).rejects.toThrow(ToolError);
  });

  it("allows workspace-relative path arguments", async () => {
    const { workspace, ctx } = setup();
    fs.mkdirSync(path.join(workspace, "src"));
    fs.writeFileSync(path.join(workspace, "src/note.txt"), "tea");
    const r = JSON.parse(
      await execHandler.execute({ command: "cat src/note.txt" }, ctx),
    );
    expect(r.stdout).toBe("tea");
    expect(r.exit_code).toBe(0);
  });
});

describe("exec workdir", () => {
  it("uses the workdir as cwd when provided", async () => {
    const { workspace, ctx } = setup();
    fs.mkdirSync(path.join(workspace, "subdir"));
    const r = JSON.parse(
      await execHandler.execute(
        { command: "pwd", workdir: "subdir" },
        ctx,
      ),
    );
    expect(r.stdout).toContain("subdir");
  });
});
