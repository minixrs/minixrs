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
the network.

Usage:
    tools/check-md-links.py [root]     # check the tree (default: repo root)
    tools/check-md-links.py --self-test  # prove the check actually fails
"""

from __future__ import annotations

import re
import sys
import tempfile
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

# Every pattern below is deliberately linear: no nested or alternating
# quantifiers, so none of them can backtrack super-linearly. Inline
# `[label](target)` with an optional title; labels containing `[` and targets
# containing `(` are not used in this tree and are not matched.
LINK_RE = re.compile(r"\[([^\[\]]*)\]\(\s*([^()\s]+)(?:\s+\"[^\"]*\")?\s*\)", re.S)
HEADING_RE = re.compile(r"^(#{1,6})[ \t]+(.*)$")
FENCE_RE = re.compile(r"^[ \t]*(`{3,}|~{3,})")
HTML_TAG_RE = re.compile(r"<a[ \t\r\n][^>]*>", re.I)
HTML_ATTR_RE = re.compile(r"(?:name|id)[ \t]*=[ \t]*[\"']([^\"']*)[\"']", re.I)

EXTERNAL_PREFIXES = ("http://", "https://", "mailto:", "//")


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
    text = re.sub(r"<[^>]*>", "", text)  # inline HTML tags
    return text


def slug(heading: str) -> str:
    """GitHub's anchor slug for a heading's rendered text."""
    s = strip_inline_markup(heading).strip().lower()
    s = s.rstrip("#").strip()  # closed ATX heading: `## Title ##`
    # Keep word characters, spaces and hyphens; drop everything else outright.
    s = "".join(c for c in s if c.isalnum() or c in " -_")
    return s.replace(" ", "-")


def blank_fenced_blocks(text: str) -> list[str]:
    """Return the document's lines with every fenced block blanked out.

    Blanking rather than deleting keeps line numbers aligned with the source.
    """
    out: list[str] = []
    fence: str | None = None
    for line in text.splitlines():
        m = FENCE_RE.match(line)
        if m:
            marker = m.group(1)[0]
            if fence is None:
                fence = marker
            elif marker == fence:
                fence = None
            out.append("")
        else:
            out.append("" if fence is not None else line)
    return out


def anchors_of(path: Path) -> set[str]:
    """Every fragment `path` defines: heading slugs plus explicit HTML anchors."""
    found: set[str] = set()
    seen: dict[str, int] = {}
    for line in blank_fenced_blocks(path.read_text(encoding="utf-8")):
        for tag in HTML_TAG_RE.findall(line):
            found.update(HTML_ATTR_RE.findall(tag))
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


def iter_links(body: str):
    """Yield `(lineno, target)` for each local link in a fence-blanked body.

    The whole document is scanned at once, not line by line: `textWrap:
    "always"` reflows prose, so a link's `[label]` and its `(target)` routinely
    end up on different lines and a per-line scan silently skips it.
    """
    for m in LINK_RE.finditer(body):
        target = m.group(2).strip("<>")
        if not target or target.startswith(EXTERNAL_PREFIXES):
            continue
        yield body.count("\n", 0, m.start()) + 1, target


def check_file(md: Path, root: Path, anchor_cache: dict[Path, set[str]]) -> tuple[int, list[str]]:
    """Check one file. Returns `(links checked, failure messages)`."""
    here = md.relative_to(root).as_posix()
    body = "\n".join(blank_fenced_blocks(md.read_text(encoding="utf-8")))
    checked = 0
    failures: list[str] = []
    for lineno, target in iter_links(body):
        checked += 1
        path_part, _, frag = target.partition("#")
        dest = (md.parent / path_part).resolve() if path_part else md
        if not dest.exists():
            failures.append(f"{here}:{lineno}: missing file -> {target}")
        elif frag and dest.suffix == ".md":
            if dest not in anchor_cache:
                anchor_cache[dest] = anchors_of(dest)
            if frag not in anchor_cache[dest]:
                failures.append(f"{here}:{lineno}: no such anchor -> {target}")
    return checked, failures


def is_excluded(path: Path, root: Path) -> bool:
    rel = path.relative_to(root).as_posix()
    return any(rel == d or rel.startswith(d + "/") for d in EXCLUDED_DIRS)


def check_tree(root: Path) -> tuple[int, int, list[str]]:
    """Check every markdown file under `root`. Returns `(files, links, failures)`."""
    files = sorted(p for p in root.rglob("*.md") if not is_excluded(p, root))
    anchor_cache: dict[Path, set[str]] = {}
    checked = 0
    failures: list[str] = []
    for md in files:
        n, f = check_file(md, root, anchor_cache)
        checked += n
        failures.extend(f)
    return len(files), checked, failures


def self_test() -> int:
    """Prove the check fails on breakage it is supposed to catch.

    A gate nobody has seen fail reads as coverage without being any. Each case
    is a tree that must produce exactly one failure, plus a control that must
    produce none -- including the two shapes a hand-rolled checker misses: an
    anchor whose heading contains `_`, and a link whose label and target were
    reflowed onto different lines.
    """
    cases: list[tuple[str, dict[str, str], int]] = [
        ("control: everything resolves", {
            "a.md": "# Title\n\nSee [b](b.md) and [sec](b.md#a-section).\n",
            "b.md": "# B\n\n## A section\n\ntext\n",
        }, 0),
        ("missing file", {"a.md": "[gone](nope.md)\n"}, 1),
        ("missing anchor", {
            "a.md": "[bad](b.md#no-such-heading)\n",
            "b.md": "## A section\n",
        }, 1),
        ("same-file anchor typo", {"a.md": "# Title\n\n[x](#titel)\n"}, 1),
        ("underscore preserved in heading", {
            "a.md": "[d](b.md#how-a-server-prints-sys_diagctl)\n",
            "b.md": "## How a server prints: `SYS_DIAGCTL`\n",
        }, 0),
        ("leading underscore preserved", {
            "a.md": "[s](b.md#naked-_start-and-the-fallback)\n",
            "b.md": "## Naked `_start` and the fallback\n",
        }, 0),
        ("link reflowed across lines", {
            "a.md": "See [the section\nover here](b.md#a-section) for more.\n",
            "b.md": "## A section\n",
        }, 0),
        ("reflowed link with a broken anchor", {
            "a.md": "See [the section\nover here](b.md#gone) for more.\n",
            "b.md": "## A section\n",
        }, 1),
        ("duplicate headings get -1", {
            "a.md": "[one](b.md#dup) and [two](b.md#dup-1)\n",
            "b.md": "## Dup\n\n## Dup\n",
        }, 0),
        ("link inside a fence is ignored", {
            "a.md": "```\n[nope](does-not-exist.md)\n```\n",
        }, 0),
        ("heading inside a fence is not an anchor", {
            "a.md": "[x](b.md#fenced)\n",
            "b.md": "```\n## Fenced\n```\n",
        }, 1),
        ("explicit HTML anchor", {
            "a.md": "[x](b.md#manual)\n",
            "b.md": '<a id="manual"></a>\n\ntext\n',
        }, 0),
        ("external links are skipped", {
            "a.md": "[e](https://example.invalid/nope) [m](mailto:a@b.c)\n",
        }, 0),
    ]

    failed = 0
    for name, files, expected in cases:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            for rel, content in files.items():
                (root / rel).write_text(content, encoding="utf-8")
            _, _, failures = check_tree(root)
            got = len(failures)
            ok = got == expected
            failed += 0 if ok else 1
            print(f"  {'ok  ' if ok else 'FAIL'} {name}: expected {expected}, got {got}")
            if not ok:
                for f in failures:
                    print(f"        {f}")

    total = len(cases)
    print(f"check-md-links: self-test {total - failed}/{total} cases passed")
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()

    root = Path(argv[1] if len(argv) > 1 else ".").resolve()
    files, checked, failures = check_tree(root)
    if not files:
        print(f"check-md-links: no markdown found under {root}", file=sys.stderr)
        return 1

    for f in failures:
        print(f"check-md-links: {f}")
    print(
        f"check-md-links: {checked} relative link(s) in {files} file(s); "
        f"{len(failures)} broken"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
