#!/usr/bin/env python3
"""Resolve jj merge conflicts in .beads/issues.jsonl.

Resolution heuristic: for each issue, pick the version with:
  1. Most advanced status: closed > in_progress > open
  2. Most recent updated_at timestamp on tie

Usage:
    python3 scripts/resolve-beads-merge.py [path/to/issues.jsonl]

Defaults to .beads/issues.jsonl in the current directory.
Writes the resolved file in-place (no conflict markers).
"""

import json
import sys
from pathlib import Path

STATUS_RANK = {"closed": 2, "in_progress": 1, "open": 0}


def parse_records(path: Path) -> dict[str, list[dict]]:
    """Collect all JSON records from a possibly-conflicted JSONL file.

    jj conflict format uses ' ', '-', '+' line prefixes in diff sections
    and '+++++++'/'%%%%%%%'/'\\\\\\\\' as section headers. We strip the
    single-character diff prefix and attempt to parse every resulting line
    as JSON, accumulating all versions of each issue.
    """
    versions: dict[str, list[dict]] = {}
    for raw in path.read_text().splitlines():
        # Strip the single-char diff prefix jj uses (' ', '-', '+')
        stripped = raw[1:] if raw and raw[0] in (" ", "-", "+") else raw
        if not (stripped.startswith("{") and stripped.endswith("}")):
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if "id" not in obj:
            continue
        versions.setdefault(obj["id"], []).append(obj)
    return versions


def best_version(variants: list[dict]) -> dict:
    return max(
        variants,
        key=lambda v: (
            STATUS_RANK.get(v.get("status", ""), -1),
            v.get("updated_at", ""),
        ),
    )


def resolve(path: Path) -> None:
    versions = parse_records(path)
    resolved = {k: best_version(vs) for k, vs in versions.items()}
    ordered = sorted(resolved.values(), key=lambda v: v.get("created_at", ""))
    path.write_text(
        "".join(json.dumps(obj, ensure_ascii=False) + "\n" for obj in ordered)
    )
    print(f"resolved {len(ordered)} issues -> {path}", file=sys.stderr)


if __name__ == "__main__":
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".beads/issues.jsonl")
    resolve(path)
