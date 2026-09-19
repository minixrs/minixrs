# Documentation and workflow

Which documentation tree owns what, how a slice is built and reviewed, and how markdown in this
repository is formatted.

## Documentation

Canonical docs are an **mdBook in `book/`** (content under `book/src/`, TOC in
`book/src/SUMMARY.md`), published to GitHub Pages on push to `main` via `.github/workflows/docs.yml`
(path-filtered to `book/**`; mdBook pinned to 0.5.3; Pages actions SHA-pinned like `ci.yml`). Write
new documentation there, derived from source — the loose top-level `docs/*.md` files are legacy
bootstrap notes being retired. Two trees under `docs/` are not leftovers and stay. The first is
**`docs/conventions/`** — this tree: the durable working rules contributors and agents follow,
indexed from `CLAUDE.md`. It documents not the system but the work on it, which is why it is not in
`book/`. The second is the planning tree: `docs/plan.md` is the lean live tracker (phase status +
slice summaries), and `docs/plans/` holds the full per-phase slice histories (`phase-2-ipc.md` /
`phase-3-vm.md` / `phase-4-servers.md`), the two inter-phase cleanup trackers (`phase-5-prep.md` and
`phase-6-prep.md` — one PR-sized chunk per session), and the Phase 5 design + slice plan
(`phase-5-musl-fs.md` — locked decisions D1–D13 and slices 5.0–5.11 with per-slice scope/proof; read
it before starting any Phase 5 slice). A pre-phase review belongs **in that tracker**, not in a
loose `*-RECOMMEND.md` at the repo root: those drafts go stale silently because nothing links them
and no PR is obliged to update them. Build locally with `mdbook build book`; output `book/book/` is
gitignored.

To install mdBook or preview the book locally, use the `mdbook-preview` skill.

**Slices are built with the superpowers skills as of 5.10a** — `brainstorming` for the design,
`writing-plans` for the task breakdown, `subagent-driven-development` to execute it (a fresh
subagent per task, a review after each, then a whole-branch review). The per-slice design and plan
documents land in **`docs/superpowers/specs/`** and **`docs/superpowers/plans/`**, named
`YYYY-MM-DD-<topic>-{design,plan}.md`. That is the fourth documentation tree and it is deliberately
narrow: `book/` stays canonical for how the system works, `docs/conventions/` for the rules the work
follows, `docs/plan.md` + `docs/plans/` for slice *status*, and the superpowers tree holds the
reasoning behind one slice — the decisions considered and rejected, the per-task steps, the
verification plan. The phase tracker links to the spec by relative path rather than restating it;
keep it that way, or the two drift and the tracker is the one people read.

Two habits from 5.10a worth keeping, both of which caught real defects that slice-level review
missed. **Give a fresh reviewer the diff as a file** and ask it to verify arithmetic by hand rather
than confirming that tests exist — that is what caught a tautological cap test whose assertion held
with the clause deleted. And **run the whole-branch review against `book/`**: every task in a
subagent run sees one crate, so nobody re-reads the published docs, and 5.10a reached its final gate
with the mdBook still asserting `BDEV_WRITE` answers `EROFS`. `docs.yml` is path-filtered to
`book/**`, so a slice that forgets the book ships a Pages site contradicting its own code, silently
and indefinitely.

A third habit, from 5.10b, and the sharpest one: **the dominant defect class in a subagent-driven
slice is a doc comment or test that was true when written and was falsified by a LATER TASK IN THE
SAME BRANCH.** Five instances in one slice — a `Blocks::zeroed` comment saying mkfs writes no sparse
files (task 2 added them), a `next_dir_chunk` comment saying its second iteration is unreachable at
boot (task 8's probe reaches it), two copies of an indirect-slot count that was simply miscomputed,
and worst, `callnr.rs`'s `vfs_open_payload_offsets_are_ordered_and_disjoint` — whose
`assert_eq!(fields.len(), 2, "a VFS_OPEN payload field was added")` and accompanying "there is no
`flags` field until 5.10b" comment were left untouched **by the task that added the flags field**,
disarming the very tripwire written to catch it. Each task sees one crate and no task re-reads its
neighbours' prose, so nobody catches these. Make it a whole-branch step: grep the touched crates for
every `unreachable`, `nothing reaches this`, `there is no X yet`, and `until slice N` claim, and for
every `assert_eq!(fields.len(), N)` or similar count-the-fields tripwire, and check each against
what the branch actually added. **A tripwire the adding branch does not grow is worse than none**,
because it reads as coverage.

That sweep is owed by a **review-fix round** too, not just by the slice, and a rename or a moved
path is its loudest trigger: a reviewer works finding-by-finding inside one crate, so the copies
living in `book/`, `tests/qemu-boot.expected` and `docs/plans/` are exactly the ones nobody looks
at. 5.10b's review named three falsified claims; grepping the old names across the whole tree found
four more (`book/`'s `insert_entry`, two `/etc/deny` comments in VFS, and the expected-marker file's
commentary on a probe that had been re-aimed). Run `grep -rn '<old name>' --include='*.rs'
--include='*.md' .` before committing, every time something is renamed or relocated.

A review fix that **rewords a claim** owes the same sweep as a rename: 5.11 corrected "the first
`CPF_WRITE`-required refusal" in four files and left the fifth copy in `tests/qemu-boot.expected`'s
commentary. Grep the *phrase* across `tests/ book/ docs/ CLAUDE.md` before committing any review
fix. And read every `book/` chapter a slice touches **end to end once**, not diff-wise: 5.11's two
remaining defects were self-contradictions 190 and 24 lines apart in the same chapter.

**A subagent's report is a claim about its work, not evidence of it.** Verify the artifact, not the
account — a handback can describe prose the agent did not write and cite a report file it never
created. This is also why work dispatched in parallel needs a dedupe pass: two agents given
overlapping source will each write the shared rules, and the duplicates drift immediately.

For the mutation-testing discipline (how a mutation is applied, observed, and reverted; the
boot-marker gotchas; the cases with no available proof), see
[`testing-and-markers.md`](./testing-and-markers.md).

## Review tooling: Hunk

Code review happens in a live Hunk diff session in the user's terminal, driven from here with `hunk
session …` (never `hunk diff`/`hunk show` directly — the TUI is the user's; the `hunk-review` skill
at `hunk skill path hunk-review` has the full CLI). Two standing rules:

- **Open every session with `--experimental`** (`hunk diff --experimental origin/main...HEAD`), so
  notes can carry STML. A session without `stml` in `hunk session context --json`'s
  `experimentalFeatures` rejects markup and silently falls back to plain summaries.
- **Author notes in STML, not plain text** — `hunk session comment apply … --stdin` with a `markup`
  field per item, `--summary` kept as a real one-line fallback. Read `hunk markup guide` first and
  preview at the session's `noteMarkupWidth` with `hunk markup render - --width N`. `<code>` is a
  *block* tag: identifiers inline go in `<c fg="accent">…</c>`; `&harr;` is not an entity, use the
  literal `↔`. A useful shape is a severity `<badge>` + bold title, a rounded `why` box, a rounded
  `fix` box, and a dim `see also` line.
- PR review comments (Grok, a reviewer) get mirrored into the session as one `comment apply` batch,
  one note per thread anchored at the thread's `newLine`, `author` set to the reviewer's name.

## One home is not enough: the pointers have to land

Every rule in this tree has exactly **one** home, and every other file that needs it links there
instead of restating it. Deduplicating is only half of that contract, and it is the half that is
easy to check. The other half is that the links still arrive at the section holding the rule — and a
dedupe pass that verifies only single-ownership will pass a tree full of pointers into nothing.

This has now failed twice in the same week, both times found by a reviewer rather than by the sweep
that had just declared the tree clean: a confused-deputy link into `abi.md` that resolved to a file
with no such rule in it, and slice 5.9's back-link that resolved to the right file but the wrong
section. In both cases the rule *did* have one home. Nothing pointed at it.

So the check is two questions, not one:

1. Does this rule appear in exactly one place?
2. Does every reference to it resolve to **that place** — right file, right section?

Question 2 is mechanical and `tools/check-md-links.py` answers it: every relative markdown link
names a file that exists, and every `#fragment` names a heading that really renders to that anchor.
It runs in CI's `fmt` job and blocks. Run it locally after any move, rename, or dedupe:

```bash
python3 tools/check-md-links.py
```

**Do not hand-roll an anchor slugger instead.** Three separate attempts in this repository got
GitHub's rules wrong and reported working links as broken — collapsing runs of whitespace (GitHub
emits one hyphen per space), stripping `_` (GitHub keeps it, so `#…sys_diagctl` and `#naked-_start…`
are correct), and scanning line by line (dprint reflows prose, so a link's `[label]` and its
`(target)` routinely land on different lines). Acting on any of those first runs would have
"repaired" links that already worked. The script encodes all three corrections.

Question 1 stays manual, and the trap there is the fix itself: consolidating a rule into one section
and then *summarising it* at the old site recreates the duplicate under a different wording, which
drifts faster than a verbatim copy would. The old site gets a sentence naming the rule and a link —
never a paraphrase of what the rule says.

## Markdown formatting

All markdown in this repository wraps prose at **100 columns**. This is mechanical, not a habit.
**`dprint check` blocks in CI** as a step of the `fmt` job (see [`ci.md`](./ci.md)), so unformatted
markdown fails the PR the same way unformatted Rust does.

Configuration is `dprint.json` at the repo root, pinning dprint-plugin-markdown by checksum. Tables,
fenced code blocks, and bare URLs are exempt — dprint leaves them alone. `<!-- dprint-ignore -->`
exempts the block that follows it.

**`textWrap: "always"` reflows a whole paragraph**, so consecutive lines separated only by a newline
are joined before being rewrapped. Anything that must keep its own line has to be a real markdown
block: a list item, a table row, or a paragraph separated by a blank line.

The installer puts the binary at `~/.dprint/bin/dprint`, which is **not** on `PATH` — use the
absolute path or add it.

**`grep -F` lies on formatted prose.** dprint wraps *inside* multi-word backticked tokens, so a
phrase that is present can still fail to match. Verify with whitespace normalised:

```bash
python3 -c "
import re,sys
norm=lambda s: re.sub(r'\s+',' ',s)
print(norm(sys.argv[1]) in norm(open(sys.argv[2],encoding='utf-8').read()))
" 'phrase to find' path/to/file.md
```

**A nested fence must be the last element inside its outer fenced block.** Prose after it makes
dprint close the outer fence early and strand that prose outside the block.

To prove a reformat changed no content, compare the **non-whitespace character sequence** before and
after. `git diff --ignore-all-space` is useless here: a reflow joins and splits lines, which git
sees as hundreds of changed lines.

Rust doc comments are out of scope — rustfmt does not reflow them and there is no gate that would.

```bash
dprint fmt          # format everything
dprint check        # verify, exactly as the fmt job does
```

## Plan status

Slice status is tracked as a GFM checkbox in the slice's own plan file, checked by the slice's own
PR. See [`git-and-prs.md`](./git-and-prs.md) for the full rule — this file does not restate it.
