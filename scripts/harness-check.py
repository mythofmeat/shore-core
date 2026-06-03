#!/usr/bin/env python3
"""Validate Shore's agent-harness knowledge base and structural guardrails."""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from urllib.parse import unquote
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

REQUIRED_FILES = [
    "CLAUDE.md",
    "README.md",
    "CONFIGURATION.md",
    "ARCHITECTURE.md",
    "CHANGELOG.md",
    "dev/test-harness/src/lib.rs",
]

# CLAUDE.md is the repo-root agent entry map (formerly AGENTS.md). Distinct
# from the per-character workspace AGENTS.md prompt file.
ENTRY_MAP_REQUIRED_STRINGS = [
    "README.md",
    "ARCHITECTURE.md",
    "CONFIGURATION.md",
    "CHANGELOG.md",
    "python3 scripts/harness-check.py",
]

MAX_ENTRY_MAP_LINES = 120
CONFLICT_RE = re.compile(r"^(<<<<<<<(?: .*)?|=======$|>>>>>>>(?: .*)?)$")
MD_LINK_RE = re.compile(r"(?<!!)\[[^\]\n]+\]\(([^)\n]+)\)")
URI_SCHEME_RE = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")
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
        return [
            ROOT / line
            for line in proc.stdout.splitlines()
            if line and (ROOT / line).exists()
        ]
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


def check_entry_map(errors: list[str]) -> None:
    path = ROOT / "CLAUDE.md"
    if not path.exists():
        return
    text = read_text(path)
    line_count = len(text.splitlines())
    if line_count > MAX_ENTRY_MAP_LINES:
        fail(errors, f"CLAUDE.md has {line_count} lines; keep it <= {MAX_ENTRY_MAP_LINES}")
    for needle in ENTRY_MAP_REQUIRED_STRINGS:
        if needle not in text:
            fail(errors, f"CLAUDE.md must link or mention {needle}")


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


def markdown_files() -> list[Path]:
    return [
        path
        for path in repo_files()
        if path.suffix.lower() == ".md"
        and not any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts)
    ]


def link_target(raw_target: str) -> str:
    target = raw_target.strip()
    if target.startswith("<"):
        end = target.find(">")
        if end != -1:
            return target[1:end].strip()
    return target.split()[0] if target else target


def check_markdown_links(errors: list[str]) -> None:
    for path in markdown_files():
        text = read_text(path)
        for match in MD_LINK_RE.finditer(text):
            target = link_target(match.group(1))
            if (
                not target
                or target.startswith("#")
                or URI_SCHEME_RE.match(target)
                or target.startswith("//")
            ):
                continue

            target_path = unquote(target.split("#", 1)[0])
            if not target_path:
                continue
            resolved = (path.parent / target_path).resolve(strict=False)
            if not resolved.exists():
                rel = path.relative_to(ROOT)
                fail(errors, f"broken local markdown link in {rel}: {target}")


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
    check_entry_map(errors)
    check_conflict_markers(errors)
    check_architecture_workspace_members(errors)
    check_markdown_links(errors)
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
