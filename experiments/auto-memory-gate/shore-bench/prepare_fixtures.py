#!/usr/bin/env python3
"""
Prepare read-only benchmark fixtures from LoCoMo data.

Produces for each conv_id:
  fixtures/<conv_id>/
    memory.db         — Shore-schema sqlite populated with observation facts
    active.jsonl      — last N turns as Shore Message rows (prior history only)
    questions.jsonl   — one line per QA: {question, ground_truth, category, evidence}

Run once. The driver copies these into a fresh tmpdir per question.
"""

import json
import os
import re
import sqlite3
import sys
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent.parent
DATASET = REPO / "shore-daemon/tests/data/locomo10.json"
FIXTURES = ROOT / "fixtures"

WINDOW_SIZE = int(os.environ.get("WINDOW_SIZE", "12"))
CONVS = os.environ.get("CONVS", "conv-26,conv-50").split(",")

# ── Shore Entry schema (mirrored from shore-daemon/src/memory/db.rs) ────────
# Keep synced with SCHEMA_SQL + FTS_SCHEMA_SQL in db.rs.

SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS entries (
    id              TEXT PRIMARY KEY,
    memory_type     TEXT NOT NULL,
    source          TEXT NOT NULL DEFAULT '',
    reason          TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'active',
    confidence      REAL NOT NULL DEFAULT 1.0,
    summary_text    TEXT NOT NULL DEFAULT '',
    topic_tags      TEXT NOT NULL DEFAULT '',
    topic_key       TEXT NOT NULL DEFAULT '',
    start_timestamp TEXT NOT NULL DEFAULT '',
    end_timestamp   TEXT NOT NULL DEFAULT '',
    message_count   INTEGER NOT NULL DEFAULT 0,
    source_entry_ids TEXT NOT NULL DEFAULT '',
    related_entry_ids TEXT NOT NULL DEFAULT '',
    superseded_by   TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    entry_type      TEXT NOT NULL DEFAULT '',
    image_path      TEXT NOT NULL DEFAULT '',
    collated_at     TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS entities (
    entity_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    type        TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS entry_entities (
    entry_id    TEXT NOT NULL,
    entity_id   INTEGER NOT NULL,
    PRIMARY KEY (entry_id, entity_id)
);
CREATE TABLE IF NOT EXISTS changelog (
    changelog_id INTEGER PRIMARY KEY AUTOINCREMENT,
    operation    TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    timestamp    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS changelog_entries (
    changelog_id INTEGER NOT NULL,
    entry_id     TEXT NOT NULL,
    PRIMARY KEY (changelog_id, entry_id)
);
CREATE TABLE IF NOT EXISTS changelog_entities (
    changelog_id INTEGER NOT NULL,
    entity_id    INTEGER NOT NULL,
    PRIMARY KEY (changelog_id, entity_id)
);
CREATE TABLE IF NOT EXISTS flags (
    flag_id     INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id    TEXT NOT NULL,
    flag_type   TEXT NOT NULL,
    reason      TEXT NOT NULL DEFAULT '',
    resolved_at TEXT,
    resolution  TEXT,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS collation_skip (
    entry_id   TEXT NOT NULL,
    phase      TEXT NOT NULL,
    skipped_at TEXT NOT NULL,
    PRIMARY KEY (entry_id, phase)
);
"""

FTS_SCHEMA_SQL = """
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    summary_text, topic_tags, topic_key,
    content=entries, content_rowid=rowid,
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS entries_fts_insert AFTER INSERT ON entries BEGIN
    INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
    VALUES (new.rowid, new.summary_text, new.topic_tags, new.topic_key);
END;
CREATE TRIGGER IF NOT EXISTS entries_fts_update AFTER UPDATE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, summary_text, topic_tags, topic_key)
    VALUES ('delete', old.rowid, old.summary_text, old.topic_tags, old.topic_key);
    INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
    VALUES (new.rowid, new.summary_text, new.topic_tags, new.topic_key);
END;
CREATE TRIGGER IF NOT EXISTS entries_fts_delete AFTER DELETE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, summary_text, topic_tags, topic_key)
    VALUES ('delete', old.rowid, old.summary_text, old.topic_tags, old.topic_key);
END;
"""


def parse_locomo_date(s: str):
    """Return (rfc3339_string, 'D Month YYYY'-human) or ('', '') on parse failure."""
    from datetime import datetime
    cleaned = s.replace(",", "").strip()
    for fmt in ("%I:%M %p on %d %B %Y", "%-I:%M %p on %-d %B %Y"):
        try:
            dt = datetime.strptime(cleaned, fmt)
            return (
                dt.strftime("%Y-%m-%dT%H:%M:%S+00:00"),
                dt.strftime("%-d %B %Y"),
            )
        except ValueError:
            continue
    return ("", "")


def build_memory_db(path: Path, conv: dict, sample_id: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    db = sqlite3.connect(path)
    db.executescript(SCHEMA_SQL)
    db.executescript(FTS_SCHEMA_SQL)

    now = "2026-04-20T00:00:00+00:00"

    # Session dates — keep both RFC3339 (for timestamp fields) and human form
    # (to embed in summary_text, matching how Shore's own memory agent writes
    # entries with date context baked in).
    sess_dates = {}       # sid -> rfc3339
    sess_dates_human = {} # sid -> "8 May 2023"
    for k, v in conv["conversation"].items():
        if k.endswith("_date_time"):
            try:
                sid = int(k.split("_")[1])
                rfc, human = parse_locomo_date(v)
                sess_dates[sid] = rfc
                sess_dates_human[sid] = human
            except ValueError:
                continue

    n = 0
    for obs_key, speakers in conv.get("observation", {}).items():
        if not obs_key.endswith("_observation"):
            continue
        try:
            sid = int(obs_key.split("_")[1])
        except ValueError:
            continue
        for speaker, items in speakers.items():
            for item in items:
                if not isinstance(item, (list, tuple)) or len(item) < 2:
                    continue
                text, dia_id = item[0], item[1]
                if not text:
                    continue
                safe_dia = re.sub(r"[^A-Za-z0-9_-]+", "-", dia_id).strip("-")
                entry_id = f"{sample_id}-{safe_dia}-{uuid.uuid4().hex[:8]}"
                # summary_text is the raw LoCoMo observation. Shore's FTS/vector
                # retrieval now correctly surfaces `start_timestamp` (see
                # FtsHit fix), so we don't need to embed dates in the prose.
                summary = text
                topic_tags = speaker
                topic_key = f"session_{sid}"
                ts = sess_dates.get(sid, "")
                db.execute(
                    """
                    INSERT INTO entries (
                        id, memory_type, source, reason, status, confidence,
                        summary_text, topic_tags, topic_key,
                        start_timestamp, end_timestamp, message_count,
                        source_entry_ids, related_entry_ids, superseded_by,
                        created_at, updated_at, entry_type, image_path, collated_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        entry_id, "episodic",
                        f"locomo:{sample_id}:{dia_id}",
                        "benchmark observation", "active", 1.0,
                        summary, topic_tags, topic_key,
                        ts, ts, 1,
                        "", "", "",
                        now, now, "", "", "",
                    ),
                )
                n += 1
    db.commit()
    db.close()
    return n


def flatten_conv(conv: dict):
    flat = []
    skeys = sorted(
        (k for k in conv["conversation"]
         if k.startswith("session_")
         and not k.endswith("_date_time")
         and not k.endswith("_summary")),
        key=lambda s: int(s.split("_")[1]),
    )
    for k in skeys:
        sid = int(k.split("_")[1])
        for t in conv["conversation"][k]:
            flat.append({
                "session": sid,
                "speaker": t.get("speaker"),
                "dia_id": t.get("dia_id"),
                "text": t.get("text", ""),
            })
    return flat


def build_active_jsonl(path: Path, flat_turns: list, window: int = WINDOW_SIZE):
    """Write last `window` turns as Shore Message rows.
    Alternating user/assistant; we arbitrarily map speaker_a → user, speaker_b → assistant.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    window_turns = flat_turns[-window:] if len(flat_turns) > window else flat_turns
    # Assign base timestamps spaced 1 minute apart ending at 2026-04-19T23:59:00
    from datetime import datetime, timedelta
    end = datetime(2026, 4, 19, 23, 59, 0)
    # pick roles — first speaker seen in turns = user, the other = assistant
    seen = []
    for t in window_turns:
        if t["speaker"] not in seen:
            seen.append(t["speaker"])
    user_speaker = seen[0] if seen else None

    lines = []
    for i, t in enumerate(window_turns):
        ts = end - timedelta(minutes=(len(window_turns) - i))
        role = "user" if t["speaker"] == user_speaker else "assistant"
        msg = {
            "msg_id": f"m_{uuid.uuid4().hex}",
            "role": role,
            "timestamp": ts.isoformat() + "+00:00",
            "images": [],
            "content_blocks": [{"type": "text", "text": f"{t['speaker']}: {t['text']}"}],
        }
        lines.append(json.dumps(msg))
    path.write_text("\n".join(lines) + ("\n" if lines else ""))
    return len(lines), user_speaker


def write_questions(path: Path, qas: list):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for qa in qas:
            if qa["category"] not in (1, 2, 3, 4):
                continue
            if qa.get("answer") is None:
                continue
            f.write(json.dumps({
                "question": qa["question"],
                "ground_truth": str(qa["answer"]),
                "category": qa["category"],
                "evidence": qa.get("evidence", []),
            }) + "\n")


def main():
    data = json.loads(DATASET.read_text())
    for conv_id in CONVS:
        conv = next(c for c in data if c["sample_id"] == conv_id)
        conv_dir = FIXTURES / conv_id
        conv_dir.mkdir(parents=True, exist_ok=True)
        n_entries = build_memory_db(conv_dir / "memory.db", conv, conv_id)
        flat = flatten_conv(conv)
        n_turns, user_speaker = build_active_jsonl(conv_dir / "active.jsonl", flat)
        write_questions(conv_dir / "questions.jsonl", conv["qa"])
        print(f"{conv_id}:")
        print(f"  memory.db: {n_entries} entries")
        print(f"  active.jsonl: {n_turns} turns (user_speaker = {user_speaker})")
        print(f"  questions.jsonl: {sum(1 for _ in (conv_dir / 'questions.jsonl').open())} QAs")


if __name__ == "__main__":
    main()
