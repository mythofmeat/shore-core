#!/usr/bin/env python3
"""
One-time migration: export SQLite memory entries to markdown files.

Usage:
    python3 scripts/migrate-memory.py <character> [data_dir] [config_dir]

Defaults:
    data_dir: $XDG_DATA_HOME/shore (or ~/.local/share/shore)
    config_dir: $XDG_CONFIG_HOME/shore (or ~/.config/shore)

Writes:
    {config_dir}/characters/{character}/workspace/memory/migrated/{id}.md
    {config_dir}/characters/{character}/workspace/memory/migrated/{topic_key}_{id}.md  (if topic_key set)
    {config_dir}/characters/{character}/workspace/memory/migrated/.migration_complete   (sentinel)

The SQLite database is NOT modified or deleted.
"""

import os
import re
import sqlite3
import sys
from datetime import datetime, timezone


def sanitize_filename(name: str) -> str:
    """Mirror of MarkdownMemoryStore::sanitize_filename."""
    return re.sub(r"[^a-zA-Z0-9\-_]", "-", name)


def migrate(character: str, data_dir: str, config_dir: str) -> int:
    db_path = os.path.join(data_dir, character, "memory", "memory.db")
    if not os.path.exists(db_path):
        print(f"No database found at {db_path}", file=sys.stderr)
        return 0

    out_dir = os.path.join(
        config_dir,
        "characters",
        character,
        "workspace",
        "memory",
        "migrated",
    )
    os.makedirs(out_dir, exist_ok=True)

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()
    cursor.execute(
        """
        SELECT id, topic_key, summary_text
        FROM entries
        ORDER BY created_at DESC
        """
    )

    count = 0
    for row in cursor:
        entry_id = row["id"]
        topic_key = row["topic_key"] or ""
        summary = row["summary_text"] or ""

        if topic_key:
            filename = f"{sanitize_filename(topic_key)}_{entry_id}.md"
        else:
            filename = f"{entry_id}.md"

        content = f"# {topic_key}\n\n{summary}\n"
        path = os.path.join(out_dir, filename)

        with open(path, "w", encoding="utf-8") as f:
            f.write(content)
        count += 1

    conn.close()

    # Write sentinel
    sentinel = os.path.join(out_dir, ".migration_complete")
    with open(sentinel, "w", encoding="utf-8") as f:
        f.write(f"Migration completed at {datetime.now(timezone.utc).isoformat()}\n")
        f.write(f"Migrated {count} entries\n")

    print(f"Migrated {count} entries to {out_dir}")
    return count


def main() -> int:
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <character> [data_dir]", file=sys.stderr)
        return 1

    character = sys.argv[1]
    if len(sys.argv) >= 3:
        data_dir = sys.argv[2]
    else:
        xdg_data = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
        data_dir = os.path.join(xdg_data, "shore")

    if len(sys.argv) >= 4:
        config_dir = sys.argv[3]
    else:
        xdg_config = os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config"))
        config_dir = os.path.join(xdg_config, "shore")

    migrate(character, data_dir, config_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main())
