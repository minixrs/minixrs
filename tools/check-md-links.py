#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-3-Clause
# Copyright (c) 2026 The minix.rs Authors
"""Verify that every relative markdown link in the repository resolves.

A rule with one home is only reachable if the pointers at it survive. This
checks both halves of that: the target file exists, and -- when the link
carries a `#fragment` -- the target file actually has a heading that renders
to that anchor.

Anchors follow GitHub's slug rules, which two hand-rolled generators have now
got wrong in this repository:

  * one hyphen per space, never collapsed ("a  b" -> "a--b");
  * `_` is preserved, not stripped;
  * every other non-alphanumeric character is dropped, not replaced;
  * repeated headings get `-1`, `-2`, ... in document order.

External links (http, https, mailto) are out of scope -- nothing here reaches
the network. Usage: `tools/check-md-links.py [root]` (default: repo root).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# `docs/superpowers/` is excluded on purpose: those are frozen per-slice records
# whose "links" are often quoted instructions for a path relative to some other
# file, not links from the plan itself. `.superpowers/` is git-ignored scratch.
EXCLUDED_DIRS = {
    ".git",
    ".superpowers",
    "target",
    "external",
    "node_modules",
    "book/book",
    "docs/superpowers",
}

# Inline `[text](target)`. Reference-style and bare autolinks are not used here.
LINK_RE = re.compile(
    r"\[(?:[^\[\]]|\[[^\[\]]*\])*\]\(\s*([^()\s]*(?:\([^()]*\)[^()\s]*)*)\s*\)",
    re.S,
)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
HTML_ANCHOR_RE = re.compile(r"""<a\s+[^>]*\b(?:name|id)\s*=\s*["']([^"']+)["']""", re.I)


def strip_inline_markup(text: str) -> str:
    """Reduce heading source to the text GitHub slugs."""
    text = re.sub(r"!\[([^\]]*)\]\([^)]*\)", r"\1", text)  # images -> alt
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)  # links -> label
    text = re.sub(r"\[([^\]]*)\]\[[^\]]*\]", r"\1", text)  # ref links -> label
    text = text.replace("`", "")
    # Only paired emphasis markers, and never a bare `_`: GitHub keeps the
    # underscore in `SYS_DIAGCTL` and in `_start`, and an anchor generator that
    # strips it reports working links as broken (this one did, twice).
    text = re.sub(r"(\*\*|~~)", "", text)
    text = re.sub(r"<[^>]+>", "", text)  # inline HTML tags
    return text


def slug(heading: str) -> str:
    """GitHub's anchor slug for a heading's rendered text."""
    s = strip_inline_markup(heading).strip().lower()
    # Keep word characters, spaces and hyphens; drop everything else outright.
    s = "".join(c for c in s if c.isalnum() or c in " -_")
    return s.replace(" ", "-")


def anchors_of(path: Path) -> set[str]:
    """Every fragment `path` defines: heading slugs plus explicit HTML anchors."""
    found: set[str] = set()
    seen: dict[str, int] = {}
    fence: str | None = None
    for line in path.read_text(encoding="utf-8").splitlines():
        m = FENCE_RE.match(line)
        if m:
            marker = m.group(1)[0]
            if fence is None:
                fence = marker
            elif marker == fence:
                fence = None
            continue
        if fence is not None:
            continue
        found.update(HTML_ANCHOR_RE.findall(line))
        h = HEADING_RE.match(line)
        if not h:
            continue
        base = slug(h.group(2))
        if not base:
            continue
        n = seen.get(base, 0)
        seen[base] = n + 1
        found.add(base if n == 0 else f"{base}-{n}")
    return found


def is_excluded(path: Path, root: Path) -> bool:
    rel = path.relative_to(root).as_posix()
    return any(rel == d or rel.startswith(d + "/") for d in EXCLUDED_DIRS)


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    files = sorted(p for p in root.rglob("*.md") if not is_excluded(p, root))
    if not files:
        print(f"check-md-links: no markdown found under {root}", file=sys.stderr)
        return 1

    anchor_cache: dict[Path, set[str]] = {}
    failures: list[str] = []
    checked = 0

    for md in files:
        text = md.read_text(encoding="utf-8")
        # Blank out fenced blocks so example links inside them are not checked.
        lines = text.splitlines()
        fence: str | None = None
        for i, line in enumerate(lines):
            m = FENCE_RE.match(line)
            if m:
                marker = m.group(1)[0]
                if fence is None:
                    fence = marker
                elif marker == fence:
                    fence = None
                lines[i] = ""
                continue
            if fence is not None:
                lines[i] = ""

        # Scan the whole blanked document, not line by line: `textWrap: "always"`
        # reflows prose, so a link's `[text]` and its `(target)` routinely end up
        # on different lines and a per-line scan silently skips it.
        body = "\n".join(lines)
        here = md.relative_to(root).as_posix()
        for m in LINK_RE.finditer(body):
            target = m.group(1).split(" ", 1)[0].strip("<>")
            if not target or target.startswith(("http://", "https://", "mailto:", "//")):
                continue
            lineno = body.count("\n", 0, m.start()) + 1
            path_part, _, frag = target.partition("#")
            dest = (md.parent / path_part).resolve() if path_part else md
            checked += 1
            if not dest.exists():
                failures.append(f"{here}:{lineno}: missing file -> {target}")
                continue
            if not frag or dest.suffix != ".md":
                continue
            if dest not in anchor_cache:
                anchor_cache[dest] = anchors_of(dest)
            if frag not in anchor_cache[dest]:
                failures.append(f"{here}:{lineno}: no such anchor -> {target}")

    for f in failures:
        print(f"check-md-links: {f}")
    print(
        f"check-md-links: {checked} relative link(s) in {len(files)} file(s); "
        f"{len(failures)} broken"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
