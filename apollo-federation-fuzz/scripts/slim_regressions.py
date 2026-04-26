#!/usr/bin/env python3
"""Retrofit regression artefacts to the slim format.

Old format had four sections: SUBGRAPHS, SUPERGRAPH, OPERATION, DIFF
(or PANIC). The supergraph is deterministic from the subgraphs + composer
and the diff is re-derived by replaying through both planners, so both
sections are redundant. Eliding them shrinks each artefact by ~70-90%.

We also synthesise a one-line `summary:` header by inspecting the old
DIFF/PANIC payload before it gets stripped, so the slim file stays
human-readable.
"""

from __future__ import annotations
import re
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1] / "tests" / "regressions"


def split_sections(text: str) -> dict[str, str]:
    # Find markers in document order. Sorting by position (not by name) is
    # essential — earlier versions of this script keyed by name and lost
    # the OPERATION block whenever it didn't sort alphabetically next.
    found: list[tuple[str, int]] = [
        (m.group(1), m.start())
        for m in re.finditer(r"^=== (\w+) ===\s*$", text, flags=re.MULTILINE)
    ]
    found.sort(key=lambda kv: kv[1])
    out: dict[str, str] = {}
    for i, (name, start) in enumerate(found):
        end = found[i + 1][1] if i + 1 < len(found) else len(text)
        body_start = text.find("\n", start) + 1
        out[name] = text[body_start:end].rstrip() + "\n"
    return out


def summary_for(diff: str | None, panic: str | None) -> str:
    if panic:
        # Pull the first head_panic / base_panic line.
        m = re.search(r'(head_panic|base_panic)=Some\("([^"]+)"\)', panic)
        if m:
            return f"PANIC: {m.group(2)}"
        return "PANIC"
    if not diff:
        return "(no diff captured)"
    tags = []
    if "on Query" in diff:
        tags.append("PR #7580")
    if '"Condition":' in diff:
        tags.append("FED-505 Condition")
    if re.search(r'^\+.*"sub_selection":', diff, flags=re.MULTILINE):
        tags.append("defer sub_selection (C)")
    if (
        re.search(r'^-.*"Field": "', diff, flags=re.MULTILINE)
        and re.search(r'^\+.*"Field": \{', diff, flags=re.MULTILINE)
    ):
        tags.append("defer Field repr (D)")
    return " + ".join(tags) if tags else "(uncategorised diff — possible Class E)"


def slim(path: Path) -> tuple[int, int]:
    original = path.read_text()
    if "=== SUBGRAPHS ===" not in original:
        return (len(original.splitlines()), len(original.splitlines()))
    sections = split_sections(original)
    header = original.split("=== SUBGRAPHS ===", 1)[0].rstrip()
    summary = summary_for(sections.get("DIFF"), sections.get("PANIC"))
    subgraphs = sections.get("SUBGRAPHS", "").rstrip() + "\n"
    operation = sections.get("OPERATION", "").rstrip() + "\n"

    new = (
        f"{header}\n"
        f"summary: {summary}\n\n"
        f"=== SUBGRAPHS ===\n{subgraphs}\n"
        f"=== OPERATION ===\n{operation}"
    )
    before = len(original.splitlines())
    after = len(new.splitlines())
    path.write_text(new)
    return (before, after)


def main() -> int:
    before_total = 0
    after_total = 0
    files = sorted(ROOT.rglob("*.txt"))
    for f in files:
        before, after = slim(f)
        before_total += before
        after_total += after
        print(f"  {after:4d} ({before:4d} -> {after:4d})  {f.relative_to(ROOT)}")
    print(f"\ntotal: {before_total} -> {after_total} lines  ({100*(before_total-after_total)/before_total:.1f}% reduction)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
