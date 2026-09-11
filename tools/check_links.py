#!/usr/bin/env python3
"""Verify that every relative Markdown link and anchor in the repo resolves.

The documentation plan treats doc drift as a CI concern rather than a review
concern. This is the smallest useful version of that: a link that points at a
file or heading which does not exist fails the build.
"""
from __future__ import annotations

import re
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*$", re.MULTILINE)
FENCE = re.compile(r"```.*?```", re.DOTALL)


def slug(heading: str) -> str:
    """Reproduce GitHub's heading-to-anchor rule closely enough to be useful."""
    text = re.sub(r"`([^`]*)`", r"\1", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = unicodedata.normalize("NFKD", text).lower()
    text = re.sub(r"[^\w\s-]", "", text)
    # GitHub maps each remaining space to one hyphen; it does not collapse runs,
    # so "A — B" becomes "a--b" once the em dash is stripped.
    return re.sub(r"\s", "-", text.strip())


def anchors(path: Path) -> set[str]:
    body = FENCE.sub("", path.read_text(encoding="utf-8"))
    seen: dict[str, int] = {}
    out: set[str] = set()
    for _, heading in HEADING.findall(body):
        base = slug(heading)
        n = seen.get(base, 0)
        seen[base] = n + 1
        out.add(base if n == 0 else f"{base}-{n}")
    return out


def main() -> int:
    docs = sorted(p for p in ROOT.rglob("*.md") if ".git" not in p.parts)
    anchor_cache: dict[Path, set[str]] = {}
    failures: list[str] = []

    for doc in docs:
        body = FENCE.sub("", doc.read_text(encoding="utf-8"))
        for target in LINK.findall(body):
            if target.startswith(("http://", "https://", "mailto:", "#!")):
                continue
            file_part, _, anchor = target.partition("#")
            resolved = doc.parent / file_part if file_part else doc
            resolved = resolved.resolve()
            if not resolved.exists():
                failures.append(f"{doc.relative_to(ROOT)}: missing file -> {target}")
                continue
            if not anchor or resolved.suffix != ".md":
                continue
            if resolved not in anchor_cache:
                anchor_cache[resolved] = anchors(resolved)
            if anchor.lower() not in anchor_cache[resolved]:
                failures.append(f"{doc.relative_to(ROOT)}: missing anchor -> {target}")

    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"checked {len(docs)} markdown files, {len(failures)} broken links")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
