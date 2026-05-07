#!/usr/bin/env python3
"""Convert SillyTavern JSONL chats into Shore compacted history segments."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


@dataclass
class ConvertedFile:
    path: Path
    sort_key: tuple[str, str]
    messages: list[dict[str, Any]]
    alternatives: int


def load_jsonl(path: Path) -> tuple[list[tuple[int, dict[str, Any]]], int]:
    records: list[tuple[int, dict[str, Any]]] = []
    invalid = 0
    with path.open("r", encoding="utf-8") as handle:
        for line_no, raw in enumerate(handle, start=1):
            line = raw.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                invalid += 1
                continue
            if isinstance(value, dict):
                records.append((line_no, value))
    return records, invalid


def norm_name(value: str) -> str:
    return re.sub(r"\s+", " ", value.strip().casefold())


def content_block(text: str) -> dict[str, str]:
    return {"type": "text", "text": text}


def message_id(source_label: str, rel_path: str, line_no: int, role: str, text: str) -> str:
    digest = hashlib.sha1(
        f"{source_label}\0{rel_path}\0{line_no}\0{role}\0{text}".encode("utf-8")
    ).hexdigest()[:24]
    safe_label = re.sub(r"[^a-zA-Z0-9]+", "_", source_label).strip("_").lower()
    return f"m_import_{safe_label}_{digest}"


def timestamp_from_filename(path: Path) -> str:
    name = path.name
    match = re.search(
        r"(\d{4}-\d{2}-\d{2})(?:[@_ -]+(\d{2})h?(\d{2})m?(\d{2})?)?",
        name,
    )
    if not match:
        return ""
    date = match.group(1)
    hour = match.group(2) or "00"
    minute = match.group(3) or "00"
    second = match.group(4) or "00"
    return f"{date}T{hour}:{minute}:{second}"


def sort_key_for_timestamp(timestamp: str, fallback_name: str) -> tuple[str, str]:
    if not timestamp:
        return ("9999-12-31T23:59:59", fallback_name)
    candidate = timestamp.replace("Z", "+00:00")
    try:
        parsed = datetime.fromisoformat(candidate)
        if parsed.tzinfo is not None:
            parsed = parsed.astimezone(timezone.utc).replace(tzinfo=None)
        return (parsed.isoformat(timespec="microseconds"), fallback_name)
    except ValueError:
        pass

    for fmt in ("%B %d, %Y %I:%M%p", "%b %d, %Y %I:%M%p"):
        try:
            parsed = datetime.strptime(timestamp, fmt)
            return (parsed.isoformat(timespec="microseconds"), fallback_name)
        except ValueError:
            pass

    fallback_timestamp = timestamp_from_filename(Path(fallback_name))
    if fallback_timestamp:
        return sort_key_for_timestamp(fallback_timestamp, fallback_name)
    return ("9999-12-31T23:59:59", fallback_name)


def role_from_sillytavern(
    record: dict[str, Any], character_names: set[str], user_names: set[str]
) -> str | None:
    if record.get("is_system") is True:
        return "system"
    if isinstance(record.get("is_user"), bool):
        return "user" if record["is_user"] else "assistant"

    name = record.get("name")
    if isinstance(name, str):
        normalized = norm_name(name)
        if normalized in character_names:
            return "assistant"
        if normalized in user_names:
            return "user"
        return "user"
    return None


def selected_alternative_index(text: str, swipes: list[str], raw_index: Any) -> int | None:
    for index, swipe in enumerate(swipes):
        if swipe == text:
            return index
    if isinstance(raw_index, int) and swipes:
        return max(0, min(raw_index, len(swipes) - 1))
    return None


def alternatives_from_sillytavern(
    record: dict[str, Any], text: str, timestamp: str
) -> tuple[list[dict[str, Any]], int | None]:
    raw_swipes = record.get("swipes")
    if not isinstance(raw_swipes, list):
        return [], None

    raw_info = record.get("swipe_info")
    swipe_info = raw_info if isinstance(raw_info, list) else []
    alternatives: list[dict[str, Any]] = []
    seen: set[str] = set()

    for index, raw_swipe in enumerate(raw_swipes):
        if not isinstance(raw_swipe, str) or not raw_swipe.strip():
            continue
        if raw_swipe in seen:
            continue
        seen.add(raw_swipe)

        alt_timestamp = timestamp
        if index < len(swipe_info) and isinstance(swipe_info[index], dict):
            raw_date = swipe_info[index].get("send_date")
            if isinstance(raw_date, str) and raw_date:
                alt_timestamp = raw_date

        alternatives.append(
            {
                "images": [],
                "content_blocks": [content_block(raw_swipe)],
                "timestamp": alt_timestamp,
            }
        )

    alt_index = selected_alternative_index(
        text,
        [a["content_blocks"][0]["text"] for a in alternatives],
        record.get("swipe_id"),
    )
    return alternatives, alt_index


def convert_record(
    record: dict[str, Any],
    rel_path: str,
    line_no: int,
    source_label: str,
    path: Path,
    character_names: set[str],
    user_names: set[str],
) -> dict[str, Any] | None:
    if "chat_metadata" in record:
        return None

    if isinstance(record.get("mes"), str):
        text = record["mes"]
        timestamp = (
            record.get("send_date") or record.get("timestamp") or timestamp_from_filename(path)
        )
        role = role_from_sillytavern(record, character_names, user_names)
    elif isinstance(record.get("message"), str):
        text = record["message"]
        timestamp = record.get("timestamp") or timestamp_from_filename(path)
        role = role_from_sillytavern(record, character_names, user_names)
    elif isinstance(record.get("content"), str) and record.get("role") in {
        "user",
        "assistant",
        "system",
    }:
        text = record["content"]
        timestamp = record.get("timestamp") or timestamp_from_filename(path)
        role = record["role"]
    else:
        return None

    if not isinstance(timestamp, str) or not timestamp:
        timestamp = timestamp_from_filename(path) or datetime.now(timezone.utc).isoformat()
    if role is None or not text.strip():
        return None

    message: dict[str, Any] = {
        "msg_id": message_id(source_label, rel_path, line_no, role, text),
        "role": role,
        "timestamp": timestamp,
        "images": [],
        "content_blocks": [content_block(text)],
    }

    if role == "assistant" and isinstance(record.get("swipes"), list):
        alternatives, alt_index = alternatives_from_sillytavern(record, text, timestamp)
        if alternatives:
            message["alternatives"] = alternatives
            message["alt_count"] = len(alternatives)
            if alt_index is not None:
                message["alt_index"] = alt_index

    return message


def discover_files(source: Path) -> list[Path]:
    if source.is_file():
        return [source]
    return sorted(
        path for path in source.rglob("*") if path.is_file() and not path.name.startswith(".")
    )


def convert_source(
    source: Path,
    source_label: str,
    character_names: set[str],
    user_names: set[str],
) -> tuple[list[ConvertedFile], int, int]:
    converted: list[ConvertedFile] = []
    skipped_files = 0
    invalid_lines = 0

    for path in discover_files(source):
        records, invalid = load_jsonl(path)
        invalid_lines += invalid
        messages: list[dict[str, Any]] = []
        alternatives = 0
        rel_path = str(path.relative_to(source)) if source.is_dir() else path.name

        for line_no, record in records:
            message = convert_record(
                record,
                rel_path,
                line_no,
                source_label,
                path,
                character_names,
                user_names,
            )
            if message is None:
                continue
            alternatives += len(message.get("alternatives", []))
            messages.append(message)

        if not messages:
            skipped_files += 1
            continue

        first_timestamp = messages[0]["timestamp"]
        converted.append(
            ConvertedFile(
                path=path,
                sort_key=sort_key_for_timestamp(first_timestamp, path.name),
                messages=messages,
                alternatives=alternatives,
            )
        )

    converted.sort(key=lambda item: item.sort_key)
    return converted, skipped_files, invalid_lines


def load_manifest(character_dir: Path) -> dict[str, Any]:
    manifest_path = character_dir / "compaction.json"
    if not manifest_path.exists():
        return {"segments": [], "total_compacted_messages": 0}
    with manifest_path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def import_segments(
    converted: list[ConvertedFile],
    character_dir: Path,
    segment_prefix: str,
    source_label: str,
    position: str,
    replace: bool,
    apply: bool,
) -> dict[str, Any]:
    manifest_path = character_dir / "compaction.json"
    segments_dir = character_dir / "segments"
    manifest = load_manifest(character_dir)
    existing_segments = list(manifest.get("segments") or [])

    if replace:
        existing_segments = [
            entry
            for entry in existing_segments
            if not str(entry.get("file", "")).startswith(f"{segment_prefix}_")
        ]
    else:
        existing_imports = [
            entry.get("file")
            for entry in existing_segments
            if str(entry.get("file", "")).startswith(f"{segment_prefix}_")
        ]
        if existing_imports:
            raise SystemExit(
                f"Refusing to duplicate import; found existing {segment_prefix} entries. "
                "Use --replace to rebuild them."
            )

    imported_entries: list[dict[str, Any]] = []
    now = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    for index, item in enumerate(converted, start=1):
        segment_file = f"{segment_prefix}_{index:04}.jsonl"
        imported_entries.append(
            {
                "file": segment_file,
                "message_count": len(item.messages),
                "compacted_at": now,
                "source": source_label,
                "source_path": str(item.path),
            }
        )

    if position == "prepend":
        next_segments = imported_entries + existing_segments
    else:
        next_segments = existing_segments + imported_entries

    next_manifest = {
        "segments": next_segments,
        "total_compacted_messages": sum(
            int(entry.get("message_count") or 0) for entry in next_segments
        ),
    }

    if apply:
        character_dir.mkdir(parents=True, exist_ok=True)
        segments_dir.mkdir(parents=True, exist_ok=True)

        if replace:
            for path in segments_dir.glob(f"{segment_prefix}_*.jsonl"):
                path.unlink()

        for index, item in enumerate(converted, start=1):
            segment_path = segments_dir / f"{segment_prefix}_{index:04}.jsonl"
            if segment_path.exists() and not replace:
                raise SystemExit(f"Refusing to overwrite existing segment: {segment_path}")
            lines = [json.dumps(message, ensure_ascii=False) for message in item.messages]
            segment_path.write_text("\n".join(lines) + "\n", encoding="utf-8")

        if manifest_path.exists():
            backup = manifest_path.with_name(
                f"{manifest_path.name}.bak-import-{segment_prefix}-{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
            )
            shutil.copy2(manifest_path, backup)
        write_json(manifest_path, next_manifest)

    return {
        "apply": apply,
        "source_files_with_messages": len(converted),
        "imported_messages": sum(len(item.messages) for item in converted),
        "imported_alternatives": sum(item.alternatives for item in converted),
        "new_segment_entries": len(imported_entries),
        "existing_segment_entries": len(existing_segments),
        "total_compacted_messages_after": next_manifest["total_compacted_messages"],
        "first_imported_file": str(converted[0].path) if converted else "",
        "last_imported_file": str(converted[-1].path) if converted else "",
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--character-data-dir", required=True, type=Path)
    parser.add_argument("--source-label", default="sillytavern")
    parser.add_argument("--segment-prefix", default="legacy_sillytavern")
    parser.add_argument("--position", choices=["prepend", "append"], default="prepend")
    parser.add_argument("--character-name", action="append", default=[])
    parser.add_argument("--user-name", action="append", default=[])
    parser.add_argument("--replace", action="store_true")
    parser.add_argument("--apply", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    source = args.source.expanduser().resolve()
    character_dir = args.character_data_dir.expanduser().resolve()

    if not source.exists():
        raise SystemExit(f"source does not exist: {source}")

    character_names = {norm_name(character_dir.name), *map(norm_name, args.character_name)}
    user_names = set(map(norm_name, args.user_name))

    converted, skipped_files, invalid_lines = convert_source(
        source,
        args.source_label,
        character_names,
        user_names,
    )
    if not converted:
        raise SystemExit("no convertible chat messages found")

    summary = import_segments(
        converted,
        character_dir,
        args.segment_prefix,
        args.source_label,
        args.position,
        args.replace,
        args.apply,
    )
    summary["skipped_files"] = skipped_files
    summary["invalid_json_lines"] = invalid_lines
    print(json.dumps(summary, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
