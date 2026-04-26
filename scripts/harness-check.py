#!/usr/bin/env python3
"""Validate Shore's agent-harness knowledge base and structural guardrails."""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

REQUIRED_FILES = [
    "AGENTS.md",
    "CLAUDE.md",
    "README.md",
    "GOALS.md",
    "FEATURES.md",
    "CONFIGURATION.md",
    "ARCHITECTURE.md",
    "DECISIONS.md",
    "docs/README.md",
    "docs/HARNESS_ENGINEERING.md",
    "docs/PLANS.md",
    "docs/QUALITY_SCORE.md",
    "docs/RELIABILITY.md",
    "docs/SECURITY.md",
    "docs/dev-info/INVARIANTS.md",
    "docs/dev-info/PROMPT_CACHING.md",
    "docs/dev-info/QUIRKS.md",
    "docs/design-docs/index.md",
    "docs/product-specs/index.md",
    "docs/exec-plans/README.md",
    "docs/exec-plans/active/README.md",
    "docs/exec-plans/completed/README.md",
    "docs/exec-plans/tech-debt-tracker.md",
    "docs/references/harness-engineering.md",
    "dev/mcp/README.md",
    "dev/test-harness/src/lib.rs",
]

AGENTS_REQUIRED_STRINGS = [
    "GOALS.md",
    "docs/README.md",
    "ARCHITECTURE.md",
    "docs/HARNESS_ENGINEERING.md",
    "docs/dev-info/INVARIANTS.md",
    "docs/RELIABILITY.md",
    "docs/SECURITY.md",
    "docs/QUALITY_SCORE.md",
    "python3 scripts/harness-check.py",
]

DOC_INDEX_REQUIRED_STRINGS = [
    "HARNESS_ENGINEERING.md",
    "PLANS.md",
    "QUALITY_SCORE.md",
    "RELIABILITY.md",
    "SECURITY.md",
    "dev-info/INVARIANTS.md",
    "exec-plans/README.md",
    "references/harness-engineering.md",
]

MAX_AGENTS_LINES = 120
CONFLICT_RE = re.compile(r"^(<<<<<<<(?: .*)?|=======$|>>>>>>>(?: .*)?)$")
SKIP_DIRS = {".git", "target", "node_modules", "dist", "build", ".next"}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def repo_files() -> list[Path]:
    try:
        proc = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-co", "--exclude-standard"],
            check=True,
            text=True,
            capture_output=True,
        )
        return [ROOT / line for line in proc.stdout.splitlines() if line]
    except (OSError, subprocess.CalledProcessError):
        files: list[Path] = []
        for path in ROOT.rglob("*"):
            if not path.is_file():
                continue
            if any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
                continue
            files.append(path)
        return files


def looks_binary(path: Path) -> bool:
    try:
        with path.open("rb") as handle:
            chunk = handle.read(4096)
    except OSError:
        return True
    return b"\0" in chunk


def check_required_files(errors: list[str]) -> None:
    for rel in REQUIRED_FILES:
        if not (ROOT / rel).is_file():
            fail(errors, f"required harness file is missing: {rel}")


def check_agents_map(errors: list[str]) -> None:
    path = ROOT / "AGENTS.md"
    if not path.exists():
        return
    text = read_text(path)
    line_count = len(text.splitlines())
    if line_count > MAX_AGENTS_LINES:
        fail(errors, f"AGENTS.md has {line_count} lines; keep it <= {MAX_AGENTS_LINES}")
    for needle in AGENTS_REQUIRED_STRINGS:
        if needle not in text:
            fail(errors, f"AGENTS.md must link or mention {needle}")


def check_doc_index(errors: list[str]) -> None:
    path = ROOT / "docs/README.md"
    if not path.exists():
        return
    text = read_text(path)
    for needle in DOC_INDEX_REQUIRED_STRINGS:
        if needle not in text:
            fail(errors, f"docs/README.md must link or mention {needle}")


def check_conflict_markers(errors: list[str]) -> None:
    for path in repo_files():
        if any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
            continue
        if looks_binary(path):
            continue
        try:
            text = read_text(path)
        except UnicodeDecodeError:
            continue
        for lineno, line in enumerate(text.splitlines(), start=1):
            if CONFLICT_RE.match(line):
                rel = path.relative_to(ROOT)
                fail(errors, f"unresolved conflict marker in {rel}:{lineno}")


def check_architecture_workspace_members(errors: list[str]) -> None:
    cargo_path = ROOT / "Cargo.toml"
    arch_path = ROOT / "ARCHITECTURE.md"
    if not cargo_path.exists() or not arch_path.exists():
        return
    cargo = tomllib.loads(read_text(cargo_path))
    members = cargo.get("workspace", {}).get("members", [])
    arch = read_text(arch_path)
    for member in members:
        if member not in arch:
            fail(errors, f"workspace member {member!r} is missing from ARCHITECTURE.md")


def check_prompt_tool_names(errors: list[str]) -> None:
    prompt_path = ROOT / "backend/daemon/src/engine/prompt.rs"
    if not prompt_path.exists():
        return
    text = read_text(prompt_path)
    production_text = text.split("#[cfg(test)]", 1)[0]
    for removed in ("memory_search", "memory_read"):
        if removed in production_text:
            fail(
                errors,
                f"daemon prompt guidance must not mention removed tool name `{removed}`",
            )


def main() -> int:
    errors: list[str] = []
    check_required_files(errors)
    check_agents_map(errors)
    check_doc_index(errors)
    check_conflict_markers(errors)
    check_architecture_workspace_members(errors)
    check_prompt_tool_names(errors)

    if errors:
        print("harness-check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("harness-check: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
