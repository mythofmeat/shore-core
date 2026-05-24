import fs from "node:fs";
import path from "node:path";

import { characterMetadata, discoverCharacters } from "../characters/registry.ts";
import type { ConversationEngine } from "../engine/engine.ts";
import {
  asArgs,
  CommandError,
  requiredString,
  type CommandContext,
} from "./types.ts";

const BOOTSTRAP_FILES = [
  "SOUL.md",
  "USER.md",
  "AGENTS.md",
  "TOOLS.md",
  "HEARTBEAT.md",
];

export function listCharacters(
  ctx: CommandContext,
  engine?: ConversationEngine,
): Record<string, unknown> {
  const seen = new Set<string>();
  const characters = [];
  if (engine !== undefined) {
    seen.add(engine.name());
    characters.push(characterMetadata(ctx.configSource.configDir, engine.name()));
  }
  for (const name of discoverCharacters(ctx.configSource.configDir)) {
    if (seen.has(name)) continue;
    characters.push(characterMetadata(ctx.configSource.configDir, name));
  }
  return { characters };
}

export function switchCharacter(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const name = requiredString(args, "name");
  if (name === engine.name()) {
    return { character: name, changed: false };
  }
  if (!characterConfigExists(ctx.configSource.configDir, name)) {
    throw new CommandError("not_found", `Character not found: ${name}`);
  }
  return { character: name, changed: true };
}

export function characterInfo(
  ctx: CommandContext,
  engine: ConversationEngine,
  rawArgs: unknown,
): Record<string, unknown> {
  const args = asArgs(rawArgs);
  const requested = typeof args["name"] === "string" && args["name"].length > 0
    ? args["name"]
    : engine.name();
  if (requested !== engine.name() && !characterConfigExists(ctx.configSource.configDir, requested)) {
    throw new CommandError("not_found", `Character not found: ${requested}`);
  }

  const charDir = path.join(ctx.configSource.configDir, "characters", requested);
  const workspaceDir = characterWorkspaceDir(ctx.configSource.configDir, requested);
  const definitionPath = path.join(workspaceDir, "SOUL.md");
  const hasDefinition = isFile(definitionPath);
  const definitionPreview = hasDefinition
    ? fs.readFileSync(definitionPath, "utf8").slice(0, 500)
    : null;
  const bootstrapFiles = BOOTSTRAP_FILES.filter((name) => isFile(path.join(workspaceDir, name)));
  const dataDir = path.join(ctx.dataDir, requested);
  const pending = pendingDeferredEditPaths(dataDir);

  return {
    name: requested,
    active: requested === engine.name(),
    config_dir: charDir,
    workspace_dir: workspaceDir,
    has_definition: hasDefinition,
    definition_preview: definitionPreview,
    bootstrap_files: bootstrapFiles,
    has_config_override: isFile(path.join(charDir, "config.toml")),
    pending_deferred_edits: pending,
    data_dir: dataDir,
    has_data: exists(dataDir),
  };
}

function characterConfigExists(configDir: string, character: string): boolean {
  const dir = path.join(configDir, "characters", character);
  return exists(dir);
}

function characterWorkspaceDir(configDir: string, character: string): string {
  return path.join(configDir, "characters", character, "workspace");
}

function pendingDeferredEditPaths(dataDir: string): string[] {
  const pendingDir = path.join(dataDir, "deferred_edits");
  let names: string[];
  try {
    names = fs.readdirSync(pendingDir);
  } catch {
    return [];
  }
  return names.sort().map((name) => path.join(pendingDir, name));
}

function exists(p: string): boolean {
  try {
    fs.accessSync(p);
    return true;
  } catch {
    return false;
  }
}

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}
