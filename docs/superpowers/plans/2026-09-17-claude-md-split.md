# CLAUDE.md Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce `CLAUDE.md` from 650 lines / 138 KB to a ~150-line index of non-negotiables and
links, moving durable rules into nine `docs/conventions/` area files and per-slice design records
into `docs/plans/phase-N-*.md`, while adopting dprint as the mechanical 100-column markdown
formatter and replacing the post-merge slice-completion convention with self-checked GFM checkboxes.

**Architecture:** Content moves are governed by a single routing table (below) that assigns every
line of the current `CLAUDE.md` to exactly one destination. Near-verbatim section moves come first,
then the distillation tasks that split mixed rule/record bullets, then the trackers, then one
isolated tree-wide reformat. A sentence-level checklist built in Task 1 is the acceptance gate: a
rule that appears zero times after the split is a silent regression, and nothing else in this branch
would catch it.

**Tech Stack:** Markdown, dprint 0.57.4 with dprint-plugin-markdown 0.24.0 (WASM, checksum-pinned),
git.

**Spec:** `docs/superpowers/specs/2026-09-17-claude-md-split-design.md`

## Global Constraints

- **Branch:** all work lands on `chore/claude-md-split`. Never commit to `main`. Never push, never
  open a PR, never trigger CI — surface the branch for review instead.
- **Every commit:** `git commit --signoff`. GPG signing is on by default; never `--no-gpg-sign`,
  never `--no-verify`. Verify with `git log -1 --format='%G?'` (expect `G`) and `git log -1
  --format='%(trailers:key=Signed-off-by)'` (expect a line).
- **Commit trailer:** end each message with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- **No invented content.** Every sentence in a destination file must be traceable to a line range of
  the pre-split `CLAUDE.md`, to the spec, or to this plan. Rewording for imperative voice is
  expected; adding new rules is not. If a rule seems wrong, leave it and note it in the task's
  report — correcting it is a separate change.
- **No content loss.** Each bullet is either moved, split across destinations, or deliberately
  dropped with the drop recorded in the Task 1 checklist. Silence is not a decision.
- **Formatting:** prose wraps at 100 characters. Run `dprint fmt` on every file you touch before
  committing (available from Task 2 onward). Tables, fenced code blocks, and bare URLs are exempt
  and dprint leaves them alone.
- **The paragraph-joining rule:** `textWrap: "always"` reflows a whole markdown *paragraph*, so
  consecutive lines separated only by a newline are joined before rewrapping. Anything that must
  keep its own line has to be a real markdown block — a list item, a table row, or a paragraph
  separated by a blank line.
- **Pinned dprint plugin:**
  `https://plugins.dprint.dev/markdown-0.24.0.wasm@cf7e65674b7eb5d91f85152ae9020dc8b3e73bd7944efcbfb05df71081217764`
- **No code changes.** This branch touches no `.rs`, `.S`, `.toml` (except adding `dprint.json`) or
  CI workflow file. No QEMU boot is required. If you find yourself editing a crate, stop.

## Routing table

Line numbers refer to `CLAUDE.md` **as of commit `74df1a7`** (650 lines). Capture a pristine copy in
Task 1; do not re-derive line numbers from a partially edited file.

| Source lines                         | Content                                                                                                                                                                                                                                                       | Destination                                                                    |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| 1–34                                 | Title, Project Overview, Reference Codebase                                                                                                                                                                                                                   | **stays** in `CLAUDE.md`                                                       |
| 35–159                               | `## Build`                                                                                                                                                                                                                                                    | `docs/conventions/build-and-boot.md`                                           |
| 160–258                              | `## CI`                                                                                                                                                                                                                                                       | `docs/conventions/ci.md`                                                       |
| 259–296                              | `## Commits`                                                                                                                                                                                                                                                  | `docs/conventions/git-and-prs.md`                                              |
| 297–305                              | `## Architecture`                                                                                                                                                                                                                                             | **stays** in `CLAUDE.md`                                                       |
| 306–307                              | `## Code Conventions` heading                                                                                                                                                                                                                                 | dropped (heading only)                                                         |
| 308, 310, 319, 320, 340, 341         | SPDX, `checked_add`, `Display`/`f.pad`, forward decls, no_std-under-test clippy traps, `doc_lazy_continuation`                                                                                                                                                | `docs/conventions/rust-style.md`                                               |
| 309, 311, 312, 314–318, 321–324, 339 | release-only asserts, `// SAFETY:`, `Option<ProcNr>` lists, `.S` rules, NLL borrow-ending, run-queue admission, `UnsafeCell` tables, IPC slice passing, EL1→EL0 flush, `rts_set`/`rts_unset`, target vs caller-local dispatch, no `cfg(test)` in `kernel/src` | `docs/conventions/kernel.md`                                                   |
| 313, 337, 338                        | `Message` types in `kernel-shared`, zero `unsafe`, `no_std` + `extern crate std` in tests                                                                                                                                                                     | `docs/conventions/abi.md`                                                      |
| 325–330                              | Slices 4.3–4.8 (scheduler delegation, alarms, signals, fork/exit/wait, exec, init)                                                                                                                                                                            | `docs/plans/phase-4-servers.md` + rule extracts                                |
| 331                                  | QEMU trace forensics                                                                                                                                                                                                                                          | `docs/conventions/testing-and-markers.md`                                      |
| 332                                  | Kernel crate is bare-metal only                                                                                                                                                                                                                               | `kernel.md` (rule) + `docs/plans/phase-5-prep.md` (record)                     |
| 333                                  | `boot-stubs` feature                                                                                                                                                                                                                                          | `build-and-boot.md` (rule) + `phase-5-prep.md` (record)                        |
| 334                                  | `NR_SERVED_PROCS`                                                                                                                                                                                                                                             | `abi.md` (rule) + `phase-5-prep.md` (record)                                   |
| 335, 336, 353                        | Feature-unification debugging, `cargo tree -e build`, adding a workspace crate                                                                                                                                                                                | `build-and-boot.md`                                                            |
| 342–345, 347–352                     | Freestanding server builds, `brand!()`, `rerun-if-changed`, ELF `cfg_attr`, SEF, boot `ipc_to`, VM regions, `VMCTL_PT_UNMAP`, MXBI, DS                                                                                                                        | `docs/conventions/servers-and-drivers.md`                                      |
| 346                                  | EL0 has no console                                                                                                                                                                                                                                            | `servers-and-drivers.md` (rule) + `testing-and-markers.md` (verify via traces) |
| 354                                  | Slice 5.0 errno ABI + generated headers                                                                                                                                                                                                                       | `abi.md` + `docs/plans/phase-5-musl-fs.md`                                     |
| 355                                  | Slice 5.1 fault-safe uaccess + `SYS_DIAGCTL`                                                                                                                                                                                                                  | `kernel.md` + `phase-5-musl-fs.md`                                             |
| 356                                  | Slice 5.2 grants                                                                                                                                                                                                                                              | `kernel.md` + `abi.md` + `phase-5-musl-fs.md`                                  |
| 357–358                              | Slice 5.3 TTY, device memory, CDEV band                                                                                                                                                                                                                       | `kernel.md` + `servers-and-drivers.md` + `abi.md` + `phase-5-musl-fs.md`       |
| 359–360                              | Slice 5.4 VFS write path                                                                                                                                                                                                                                      | `servers-and-drivers.md` + `phase-5-musl-fs.md`                                |
| 361–362                              | Slice 5.5 exec initial stack                                                                                                                                                                                                                                  | `kernel.md` + `phase-5-musl-fs.md`                                             |
| 363–364                              | Slice 5.6 musl port                                                                                                                                                                                                                                           | `build-and-boot.md` + `abi.md` + `phase-5-musl-fs.md`                          |
| 365–366                              | Slice 5.7 BDEV + ramdisk + mkfs                                                                                                                                                                                                                               | `servers-and-drivers.md` + `phase-5-musl-fs.md`                                |
| 367–368                              | P3c SDK flavor                                                                                                                                                                                                                                                | `build-and-boot.md` + `ci.md` + `phase-5-musl-fs.md`                           |
| 369                                  | Slice 5.8 MFS + FS band                                                                                                                                                                                                                                       | `servers-and-drivers.md` + `abi.md` + `phase-5-musl-fs.md`                     |
| 370–419                              | Slice 5.9 exec-from-FS                                                                                                                                                                                                                                        | `kernel.md` + `phase-5-musl-fs.md`                                             |
| 420–533                              | Slices 5.10a / 5.10b                                                                                                                                                                                                                                          | `servers-and-drivers.md` (rules) + `phase-5-musl-fs.md` (records)              |
| 534–561                              | Slice 5.11 `/dev/null`, `/dev/zero`, `CDEV_READ`                                                                                                                                                                                                              | `servers-and-drivers.md` + `abi.md` + `phase-5-musl-fs.md`                     |
| 563–634                              | `## Documentation`                                                                                                                                                                                                                                            | `docs/conventions/docs-and-workflow.md`                                        |
| 635–651                              | `## Review tooling: Hunk`                                                                                                                                                                                                                                     | `docs/conventions/docs-and-workflow.md`                                        |

## The distillation rule

Tasks 5–10 and 12–13 apply this to every bullet they own:

- A sentence phrased as a standing instruction that binds future work — "take the granter from the
  kernel-stamped `m_source`, never from the payload"; "the kernel never dereferences a user VA" —
  goes to its **area file**, restated imperatively, with provenance reduced to a parenthetical slice
  number.
- A sentence describing what a slice established, measured, or learned — "the mutation matrix
  confirmed both orderings are unreachable"; "the budget went to 600 s because the last marker moved
  from 26.90% to 61.61%" — goes to **`docs/plans/phase-N-*.md`** beside that slice's entry.
- A bullet containing both is split, and the area file carries a one-line relative link back to the
  plan section so an arbitrary-looking rule can be traced to the failure that produced it.

## File structure

```
CLAUDE.md                            rewritten: ~150 lines, index + non-negotiables
dprint.json                          new: formatter config
.git-blame-ignore-revs               new: names the reformat commit
docs/conventions/README.md           new: index of the nine area files
docs/conventions/build-and-boot.md   new: build, boot budget, hello flavors, inspection
docs/conventions/ci.md               new: the eleven gates, pre-push checklist, SDK gap
docs/conventions/git-and-prs.md      new: branching, signing, DCO, PR atomicity
docs/conventions/kernel.md           new: kernel-internal patterns
docs/conventions/servers-and-drivers.md  new: user-space crate patterns
docs/conventions/abi.md              new: kernel-shared, errno, bands, D8
docs/conventions/rust-style.md       new: source hygiene and lint traps
docs/conventions/testing-and-markers.md  new: markers, mutation tests, trace forensics
docs/conventions/docs-and-workflow.md    new: doc trees, slice process, review tooling, format
docs/plans/phase-4-servers.md        modified: slice 4.3–4.8 records appended per slice
docs/plans/phase-5-musl-fs.md        modified: slice 5.0–5.11 records appended per slice
docs/plans/phase-5-prep.md           modified: chunk 3/4/7 records appended
docs/plan.md                         modified: status-convention note, 5.11 closed, Phase 6 boxes
docs/superpowers/plans/2026-09-17-routing-checklist.md  temporary; deleted in Task 16
```

---

### Task 1: Routing checklist — the acceptance contract

Builds the sentence-level inventory every later task is measured against. Nothing else in this
branch can detect a rule that silently vanishes.

**Files:**

- Create: `docs/superpowers/plans/2026-09-17-routing-checklist.md`
- Read: `CLAUDE.md` (pristine, at commit `74df1a7`)

**Interfaces:**

- Produces: `docs/superpowers/plans/2026-09-17-routing-checklist.md`, a table with columns `| id |
  source line | imperative sentence (abridged to ~90 chars) | destination | done |`. Ids are `R001`,
  `R002`, … in source order. Tasks 3–13 tick their rows; Task 16 asserts none are untitled or
  unticked.

- [ ] **Step 1: Snapshot the pristine file**

```bash
cd /Users/kevinbarnard/src/minixrs
mkdir -p /tmp/claude-md-split
git show 74df1a7:CLAUDE.md > /tmp/claude-md-split/CLAUDE.md.orig
wc -l /tmp/claude-md-split/CLAUDE.md.orig   # expect 650
```

- [ ] **Step 2: Extract candidate imperative sentences**

This is a first pass to be hand-corrected, not an oracle. It splits on sentence boundaries and keeps
sentences carrying an obligation verb.

```bash
cd /Users/kevinbarnard/src/minixrs
python3 - <<'PY' > /tmp/claude-md-split/candidates.tsv
import re
pat = re.compile(r'\b(must|never|always|do not|don\'t|keep|use|prefer|avoid|run |add |'
                 r'reserve|require|mandatory|forbidden|only|stays|shall)\b', re.I)
for n, line in enumerate(open('/tmp/claude-md-split/CLAUDE.md.orig', encoding='utf-8'), 1):
    for s in re.split(r'(?<=[.;])\s+', line.strip()):
        if len(s) > 25 and pat.search(s):
            print(f"{n}\t{s[:90]}")
PY
wc -l /tmp/claude-md-split/candidates.tsv
```

- [ ] **Step 3: Hand-correct into the checklist**

Read `/tmp/claude-md-split/candidates.tsv` alongside the pristine file. The regex over-matches (a
sentence about what a *slice* did often contains "must") and under-matches (rules phrased as bare
statements of fact, e.g. "`kernel-shared` carries zero `unsafe`"). Correct both directions. Assign
each surviving row a destination from the plan's routing table. Write
`docs/superpowers/plans/2026-09-17-routing-checklist.md` with this header and one row per rule:

```markdown
# Routing checklist (temporary)

Built by Task 1 of `2026-09-17-claude-md-split.md`. Every row is a durable rule in the pre-split
`CLAUDE.md`. Tasks 3–13 tick `done` as each rule lands. Task 16 asserts every row is ticked and
deletes this file.

| id   | src | rule                                                                        | destination   | done |
| ---- | --- | --------------------------------------------------------------------------- | ------------- | ---- |
| R001 | 308 | every new `.rs`/`.S` begins with SPDX + copyright, before any other content | rust-style.md |      |
```

- [ ] **Step 4: Verify coverage of every routed line range**

Every routing-table range that is not "dropped" must contribute at least one checklist row, or it
was skimmed rather than read.

```bash
cd /Users/kevinbarnard/src/minixrs
python3 - <<'PY'
import re
rows = [l for l in open('docs/superpowers/plans/2026-09-17-routing-checklist.md',
                        encoding='utf-8') if re.match(r'\|\s*R\d+\s*\|', l)]
srcs = sorted({int(l.split('|')[2].strip()) for l in rows})
print(f"rows={len(rows)} distinct source lines={len(srcs)}")
ranges = [(35,159),(160,258),(259,296),(308,353),(354,561),(563,634),(635,651)]
for lo, hi in ranges:
    hit = [s for s in srcs if lo <= s <= hi]
    print(f"  {lo:4d}-{hi:4d}: {len(hit)} rows" + ("   <-- EMPTY, re-read" if not hit else ""))
PY
```

Expected: every range reports at least one row; no `EMPTY` lines.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs: routing checklist for the CLAUDE.md split

Sentence-level inventory of every durable rule in the pre-split CLAUDE.md,
with the destination each one is routed to. Temporary: consumed and deleted
by the final verification task.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
git log -1 --format='%G? %(trailers:key=Signed-off-by)'
```

---

### Task 2: dprint adoption

**Files:**

- Create: `dprint.json`
- Read: `docs/superpowers/specs/2026-09-17-claude-md-split-design.md` (Formatting section)

**Interfaces:**

- Produces: a working `dprint fmt` / `dprint check` for every later task. The binary is **not**
  vendored; each task installs it if absent.

- [ ] **Step 1: Install dprint if absent**

```bash
command -v dprint || curl -fsSL https://dprint.dev/install.sh | sh
dprint --version   # expect 0.57.x or newer
```

If the install script is unavailable, download the release archive for the host triple from
`https://github.com/dprint/dprint/releases/latest` and put the binary on `PATH`.

- [ ] **Step 2: Write the config**

```json
{
  "lineWidth": 100,
  "markdown": {
    "textWrap": "always",
    "emphasisKind": "asterisks",
    "strongKind": "asterisks",
    "unorderedListKind": "dashes"
  },
  "excludes": [
    "target/**",
    "external/**",
    "book/book/**"
  ],
  "plugins": [
    "https://plugins.dprint.dev/markdown-0.24.0.wasm@cf7e65674b7eb5d91f85152ae9020dc8b3e73bd7944efcbfb05df71081217764"
  ]
}
```

The three style keys are load-bearing, not cosmetic: the plugin defaults to `_underscore_` emphasis
and would otherwise rewrite every `*word*` in the tree. The excludes matter too — `external/**` is
submodule content this repo does not own.

- [ ] **Step 3: Verify the config behaves as the spec claims**

Run against a throwaway copy; this must not modify the tree yet.

```bash
cd /Users/kevinbarnard/src/minixrs
rm -rf /tmp/dprint-probe && mkdir -p /tmp/dprint-probe
cp CLAUDE.md docs/plan.md /tmp/dprint-probe/
cp dprint.json /tmp/dprint-probe/
(cd /tmp/dprint-probe && dprint fmt >/dev/null)
python3 - <<'PY'
for f in ('CLAUDE.md', 'plan.md'):
    p = f'/tmp/dprint-probe/{f}'
    w = max(len(l.rstrip('\n')) for l in open(p, encoding='utf-8'))
    fence = any(len(l.rstrip('\n')) > 100 and not l.startswith('|')
                for l in open(p, encoding='utf-8'))
    print(f"{f}: max_char_width={w}")
PY
grep -c '◀\|✓' /tmp/dprint-probe/plan.md
```

Expected: `CLAUDE.md` max width 100; `plan.md` max width 100; glyph count 39 (unchanged — dprint
must not eat `◀`/`✓`).

- [ ] **Step 4: Verify the ignore escape hatch**

```bash
printf '<!-- dprint-ignore -->\n%s\n\n%s\n' \
  "a deliberately very long line that must survive untouched because it is preceded by an ignore directive over one hundred chars" \
  "ordinary prose that is also a deliberately very long line and therefore must be wrapped by dprint because it exceeds the width" \
  > /tmp/dprint-probe/ig.md
(cd /tmp/dprint-probe && dprint fmt ig.md >/dev/null)
awk '{print NR": "length}' /tmp/dprint-probe/ig.md
```

Expected: line 2 stays over 100; the ordinary paragraph is wrapped to ≤ 100.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
python3 -c "import json; json.load(open('dprint.json')); print('config parses')"
git add dprint.json
git commit --signoff -m "chore: dprint config for 100-column markdown

Pins dprint-plugin-markdown 0.24.0 by checksum. textWrap: always reflows
prose to 100 characters; tables, fenced code and bare URLs are left alone,
and <!-- dprint-ignore --> is the escape hatch. The three style keys stop
the plugin rewriting every *word* to _word_.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: `build-and-boot.md` and `ci.md` — the near-verbatim moves

Two files, one gate: both are moves of existing top-level sections with only heading-level and
wrapping changes. A reviewer checking "did the Build section survive" and "did the CI section
survive" is running the same check twice.

**Files:**

- Create: `docs/conventions/build-and-boot.md`, `docs/conventions/ci.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 35–159 and 160–258
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md` (tick rows)

**Interfaces:**

- Consumes: the routing checklist from Task 1; `dprint` from Task 2.
- Produces: `docs/conventions/build-and-boot.md` and `docs/conventions/ci.md`. Later tasks link to
  these by relative path from `CLAUDE.md` (`docs/conventions/…`) and from each other (`./…`).

- [ ] **Step 1: Create `build-and-boot.md` from lines 35–159**

```bash
cd /Users/kevinbarnard/src/minixrs
mkdir -p docs/conventions
{
  echo '# Build and boot'
  echo
  echo 'How to build minix.rs, how to boot it under QEMU, and how to inspect what came out.'
  echo 'Rules that bind every build live here; see [`ci.md`](./ci.md) for what CI enforces.'
  echo
  sed -n '36,159p' /tmp/claude-md-split/CLAUDE.md.orig
} > docs/conventions/build-and-boot.md
```

Then, by hand: demote the section's inner headings by one level if any exist, and confirm the
opening prose reads as a standalone document rather than a section of a larger one.

- [ ] **Step 2: Fold in the build-related rules from elsewhere**

Append, restated imperatively per the distillation rule, from `CLAUDE.md.orig`:

- line 333 — the `boot-stubs` cargo feature: default-on, lives on **two** crates (kernel and PM),
  `kernel/build.rs` threads `--no-default-features` through to the nested PM build, and it is
  deliberately **not** on `kernel-shared` because feature unification would make it impossible to
  turn off. Keep the rule; the chunk-3 narrative goes to `phase-5-prep.md` in Task 13.
- line 335 — diagnose a feature that will not turn off with `cargo tree -p <crate>
  --no-default-features -e features -i <shared-crate>`.
- line 336 — check any "this adds a dependency/compile cost" claim with `cargo tree -p
  minixrs-kernel -e build` before accepting it, review findings included.
- line 353 — adding a workspace crate: append to `members` in the root `Cargo.toml`, use literal
  manifest fields plus `publish = false` for anything internal, and add a pure-I/O `main.rs` to
  `sonar.coverage.exclusions`.
- lines 363–364 and 367–368 — the **rules** only: the three hello flavors in strict preference order
  (`$MINIXRS_SDK` → in-tree musl sysroot → `worker` packed as `hello`), never write inside
  `$MINIXRS_SDK`, all output goes to `target/hello/`, and a usable SDK that fails to build panics
  rather than demoting. The measurements and the port narrative go to `phase-5-musl-fs.md`.

- [ ] **Step 3: Create `ci.md` from lines 160–258**

```bash
cd /Users/kevinbarnard/src/minixrs
{
  echo '# CI gates'
  echo
  echo 'What runs on every PR, which gates block, and what to run locally before pushing.'
  echo 'See [`build-and-boot.md`](./build-and-boot.md) for the commands themselves.'
  echo
  sed -n '161,258p' /tmp/claude-md-split/CLAUDE.md.orig
} > docs/conventions/ci.md
```

Then append two things. First, the SDK CI-coverage gap from lines 367–368: no CI job installs an
SDK, so the SDK hello flavor has **zero** coverage and a clang driver regression ships green; the
mitigation is the local three-boot matrix. Second, a `## Deferred: a markdown format gate`
subsection recording the recipe from the spec, so the follow-up is small. Its wording:

> `dprint check` is not yet a CI gate. When it becomes one, add it to the **existing** `fmt` job
> rather than creating a twelfth blocking gate — two steps, `curl -fsSL
> https://dprint.dev/install.sh | sh` followed by `~/.dprint/bin/dprint check`.
> `dprint/check-action` would work too, but it is another third-party action to SHA-pin for no gain.

- [ ] **Step 4: Format and verify anchors survived**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/build-and-boot.md docs/conventions/ci.md
dprint check docs/conventions/build-and-boot.md docs/conventions/ci.md && echo "FORMAT OK"
for s in "cargo kernel-aarch64" "check-boot-log.sh" "MINIXRS_SDK" "boot-stubs" \
         "cargo tree -p minixrs-kernel -e build"; do
  printf '%-40s %s\n' "$s" "$(grep -cF "$s" docs/conventions/build-and-boot.md)"
done
for s in "qemu-smoke" "clippy-kernel" "check-dco.sh" "Cargo.lock" "SONAR_TOKEN"; do
  printf '%-40s %s\n' "$s" "$(grep -cF "$s" docs/conventions/ci.md)"
done
```

Expected: every count ≥ 1. A zero means content was dropped in the `sed` range.

- [ ] **Step 5: Tick the checklist rows and commit**

Mark `done` for every routing-checklist row whose destination is `build-and-boot.md` or `ci.md`.

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/build-and-boot.md docs/conventions/ci.md \
        docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): build-and-boot and ci area files

Moves CLAUDE.md's Build and CI sections out verbatim, folding in the
build-adjacent rules that were stranded among the slice records: the
boot-stubs feature, the two cargo tree diagnostics, workspace-crate
addition, and the three hello flavors.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: `git-and-prs.md` — the moved section plus the new completion rule

This task carries the substantive workflow change, which is why it is gated separately from Task 3's
mechanical moves.

**Files:**

- Create: `docs/conventions/git-and-prs.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 259–296; the spec's "Status convention" section
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Produces: `docs/conventions/git-and-prs.md`. Task 11 links to it from `CLAUDE.md`; Task 14 links
  to it from the plan trackers' status note.

- [ ] **Step 1: Move the Commits section**

```bash
cd /Users/kevinbarnard/src/minixrs
{
  echo '# Git, commits, and pull requests'
  echo
  echo 'Branching, signing, sign-off, and what a PR owes the plan trackers.'
  echo
  sed -n '260,296p' /tmp/claude-md-split/CLAUDE.md.orig
} > docs/conventions/git-and-prs.md
```

- [ ] **Step 2: Add the branch and push policy from the user's global rules**

These are in the global `CLAUDE.md`, not the project one, and the area file must restate them so the
file stands alone: feature branch always (`feature/`, `bugfix/`, `chore/`, `release/`), never commit
to `main`, PRs land by regular merge commit (never squash), and in auto mode committing is fine
while **pushing, opening a PR, or triggering CI requires explicit approval**.

- [ ] **Step 3: Add the new completion rule**

Append this section verbatim, ending with the checkbox example so the nested fence stays last:

````markdown
## A PR marks its own work complete

A PR is atomic: it contains the work **and** the record that the work is done. The PR that
implements a slice checks that slice's box in `docs/plan.md` and in the matching
`docs/plans/phase-N-*.md`, in the same PR, as part of the same change.

Marking completion is never a follow-up commit, never a separate PR, and never a cleanup task
inherited by the next slice. Those are the shapes that go stale — the tracker carried a wrong status
at slices 4.8, 5.9, and 5.11 for exactly this reason.

No PR number, no merge date, no "next" pointer, no "pending merge" state. **The first unchecked box
in plan order is the next slice.** Which PR and when are questions `git log` and the GitHub PR list
answer better than a hand-maintained line.

Lines in the older form — `✓ shipped (PR #N, merged YYYY-MM-DD)` — are retired-form history, kept
because rewriting them would churn four files to no benefit. Never write a new one.

Status is a GFM checkbox and nothing else:

```markdown
- [x] **5.11** /dev/null + /dev/zero on the memory driver + CDEV_READ
- [ ] **6.1** SYS_IRQCTL + HARDWARE NOTIFY
```
````

- [ ] **Step 4: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/git-and-prs.md
dprint check docs/conventions/git-and-prs.md && echo "FORMAT OK"
for s in "--signoff" "no-gpg-sign" "no-verify" "check-dco.sh" "merge commit" \
         "first unchecked box" "atomic"; do
  printf '%-30s %s\n' "$s" "$(grep -cF "$s" docs/conventions/git-and-prs.md)"
done
```

Expected: every count ≥ 1.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/git-and-prs.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): git-and-prs, and the PR-marks-its-own-work rule

Moves CLAUDE.md's Commits section out and adds the convention change: a
slice's own PR checks its own box, status is a GFM checkbox with no PR
number or merge date, and the pending-merge state is retired. Recording
completion after the merge is what went stale at 4.8, 5.9 and 5.11.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: `rust-style.md`

**Files:**

- Create: `docs/conventions/rust-style.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 308, 310, 319, 320, 340, 341
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Produces: `docs/conventions/rust-style.md`. `kernel.md` and `servers-and-drivers.md` link to it
  rather than restating the lint traps.

- [ ] **Step 1: Write the file**

Six rules, each its own subsection, restated imperatively and keeping every concrete detail:

1. **SPDX header** (line 308) — Rust line-comment form and `.S` block-comment form, both exact;
   `.toml`/`.ld`/`.conf` get none.
2. **`checked_add`, not `+`** (line 310) — applies to `server-rt` and **any** `servers/`,
   `drivers/`, `fs/`, `userland/` crate, because `[profile.release]` sets `overflow-checks = false`
   so `off + 4` wraps in the shipped binary while panicking under `cargo test`. Give every new
   payload accessor the `usize::MAX` test.
3. **`Display` + `f.pad`** (line 319) — render through `arrayvec::ArrayString<N>` and call `f.pad`;
   `write!` from inside `Display::fmt` ignores the outer width spec.
4. **Forward declarations** (line 320) — module-level `#![allow(dead_code)]` with a one-line comment
   naming the consuming slice.
5. **`no_std` crates linted in their std test config** (line 340) — the full trap list:
   `assertions_on_constants` on const-only `assert!`, `empty_loop`, the const-evaluator's lack of
   iterators, literal-only `assert!` messages, `assert_eq!(a.min(b), a)` for ordering comparisons, a
   `const _` may not reference a `static`, `as_chunks::<N>()` over `chunks_exact(N)`,
   `int_plus_one`, and `manual_is_multiple_of` (which fires inside `const _` too).
6. **`doc_lazy_continuation`** (line 341) — a doc-comment line starting `+ `, `- ` or `* ` parses as
   a bullet; reword so continuation lines do not begin with a list marker.

- [ ] **Step 2: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/rust-style.md
dprint check docs/conventions/rust-style.md && echo "FORMAT OK"
for s in "SPDX-License-Identifier: BSD-3-Clause" "checked_add" "overflow-checks = false" \
         "ArrayString" "assertions_on_constants" "manual_is_multiple_of" \
         "doc_lazy_continuation" "as_chunks"; do
  printf '%-40s %s\n' "$s" "$(grep -cF "$s" docs/conventions/rust-style.md)"
done
```

Expected: every count ≥ 1.

- [ ] **Step 3: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/rust-style.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): rust-style area file

SPDX headers, checked_add in release-profile crates, Display via f.pad,
forward-declaration allows, the no_std-under-test clippy traps, and
doc_lazy_continuation.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: `kernel.md`

The largest distillation task: thirteen standalone rules plus the rule halves of five slice records.

**Files:**

- Create: `docs/conventions/kernel.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 309, 311, 312, 314–318, 321–324, 332, 339, 355,
  356, 357–358, 361–362, 370–419
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: `rust-style.md` (Task 5) — link to it rather than restating lint traps.
- Produces: `docs/conventions/kernel.md`.

- [ ] **Step 1: Write the standalone rules**

Lines 309, 311, 312, 314–318, 321–324, 339, 332 — move each near-verbatim, one subsection each,
grouped as: *crate shape* (309 release-only asserts, 332 bare-metal-only and its three consequences,
339 no `#[cfg(test)]` under `kernel/src/`), *unsafe and statics* (311 `// SAFETY:`, 318 `UnsafeCell`
newtypes, 316 NLL borrow-ending and the refactor hazard), *assembly* (314, 315), and *IPC and
scheduling* (312, 317, 321, 322, 323, 324).

- [ ] **Step 2: Extract the rule halves of the kernel slice records**

From each record, keep only what binds future work:

- **355 (5.1)** — the kernel never dereferences a user VA; every byte goes through `mm/uaccess.rs`;
  an unmapped page is `EFAULT`, not an EL1 abort, so no exception-fixup table; `copy_to_user_as`
  must check `Prot::writable`; writes are all-or-nothing; `ipc/message.rs` carries zero `unsafe`.
- **356 (5.2)** — grants are re-read from the granter's address space on every call and nothing is
  cached; `verify_grant`'s check order; a walk miss is hidden as `EPERM`, not `EFAULT`; `CPF_MAGIC`
  additionally requires the granter's `SYS_PROC`; a server holding `SYS_COPY`/ `SYS_SAFECOPY` takes
  the granter from the kernel-stamped `m_source`, **never** from the payload.
- **357–358 (5.3)** — read `MAIR_EL1`, never write it; `map_page_in`'s two asserts (`prot.device ⇒
  ¬RAM`, `¬prot.device ⇒ RAM`) and the five leaf sweeps that depend on them; fork re-maps a device
  leaf rather than copying it; `resolve_copyable` rejects a device leaf either way.
- **361–362 (5.5)** — `SYS_EXEC` builds the SysV initial frame so musl's crt runs unpatched; auxv
  order is fixed by the caller; both frame failures run `teardown_addrspace` and return **before**
  the point of no return; `AT_PHDR` needs the `FILEHDR PHDRS` linker-script idiom.
- **370–419 (5.9)** — `ElfSource` is an enum, not a `dyn` trait; the grant is validated by
  `verify_grant`, not a second copy of its checks; the read completes before the point of no return;
  a leading `/` is `PM_EXEC`'s only discriminator; `argv[0]` is the path's basename, never the path.

Every narrative sentence — measurements, "what this slice proved", mutation results — stays out;
Task 13 puts it in `phase-5-musl-fs.md`. Each extracted group gets a one-line link back, e.g.
`(slice 5.2 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))`.

- [ ] **Step 3: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/kernel.md
dprint check docs/conventions/kernel.md && echo "FORMAT OK"
for s in "// SAFETY:" "UnsafeCell" "forced-target" "never dereference" "uaccess" \
         "Prot::writable" "m_source" "MAIR_EL1" "point of no return" "basename"; do
  printf '%-30s %s\n' "$s" "$(grep -ciF "$s" docs/conventions/kernel.md)"
done
grep -c 'phase-5-musl-fs.md' docs/conventions/kernel.md   # expect >= 4 back-links
```

Expected: every count ≥ 1; back-links ≥ 4.

- [ ] **Step 4: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/kernel.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): kernel area file

The kernel-internal rules: crate shape, unsafe and static-table discipline,
assembly placement, IPC and scheduling invariants, plus the rule halves of
slices 5.1, 5.2, 5.3, 5.5 and 5.9. The narrative halves stay behind for the
phase-5 tracker.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: `servers-and-drivers.md`

**Files:**

- Create: `docs/conventions/servers-and-drivers.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 342–352, 357–358, 359–360, 365–366, 369,
  420–533, 534–561
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: `kernel.md` (Task 6) for the kernel-side halves of the shared slices — link, do not
  duplicate.
- Produces: `docs/conventions/servers-and-drivers.md`.

- [ ] **Step 1: Write the standalone rules (342–352)**

Freestanding `#![no_std]`/`#![no_main]` builds with the custom target and the shared nested
`CARGO_TARGET_DIR`; `brand!()` on every user-space binary crate and the `.note.minixrs.ident` /
`KEEP()` linker rule; the `rerun-if-changed` obligation when a boot server gains a path dependency;
ELF-only attributes behind `cfg_attr(target_os = "minixrs", …)` and why `#![forbid(unsafe_code)]` is
unusable on a freestanding binary; EL0 has no console so behaviour is verified through kernel
traces; the SEF receive-loop shape and the `m_source`-gated classifier; `init_boot_image` fills
`ipc_to` only over `[0, n_active)` so a higher priv slot needs its reverse edge opened explicitly;
VM's region table and the out-of-region SIGSEGV; `VMCTL_PT_UNMAP` returns `EINVAL` rather than
panicking; the MXBI archive and how to add a boot server; DS discovery, publish-at-SEF-init, and
DS's own in-process exception.

- [ ] **Step 2: Extract the server-side rule halves of slices 5.3, 5.4, 5.7, 5.8, 5.10, 5.11**

- **5.3** — a driver replies to an unknown `m_type` (its clients all SENDREC, and a dropped request
  blocks the caller forever); a negative `SYS_SAFECOPY` result is relayed verbatim; `CDEV_WRITE`
  over `CDEV_MAX_IO` is a short write, not a failure; PL011 offsets are duplicated by design.
- **5.4** — VFS absorbs short writes and clamps `off` with `.min(len)`; break on `n == 0`; report
  partial progress on an error after progress; reply `ENOSYS` to an unknown `m_type`; the grant's
  owner is the kernel-stamped `m_source`, never a payload field.
- **5.7** — a block driver must not depend on the filesystem format; `BDEV` refuses an over-long or
  out-of-range request with `EINVAL` rather than short-reading; every step of a pre-map
  `.expect()`s.
- **5.8** — a 4 KiB block buffer is a `.bss` static, not a `main`-frame local, because a server
  stack is one page; the `Blocks` capability token makes holding a block across the next fetch a
  borrow error; a `BDEV_READ` failure becomes `EIO` while a `SYS_SAFECOPY` failure is relayed
  verbatim; MFS is degraded, never fatal, past `sef_startup`; every device-derived loop bound has a
  cap; never hold an fd-table borrow across a SENDREC; `alloc_in` scans upwards.
- **5.10a/5.10b** — the single-block-buffer step order; bitmap bit set before the zone number is
  stored; the write-back condition keys on "zone assigned **or** size grew"; copy client bytes into
  the staging buffer before anything is allocated; `find_free_slot` must let `Occupied` beat `Free`
  across every block; write every denial probe so a growing capability makes it fail loudly, and
  spell unknown-request probes band-relative.
- **5.11** — minors are a per-driver namespace; the memory driver never clamps; the four-field CDEV
  parse lives in `server-rt::cdev` while validation stays per driver.

- [ ] **Step 3: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/servers-and-drivers.md
dprint check docs/conventions/servers-and-drivers.md && echo "FORMAT OK"
for s in "brand!()" "rerun-if-changed" "cfg_attr" "sef_startup" "MXBI" "DS" \
         "unknown \`m_type\`" ".bss" "band-relative" "m_source"; do
  printf '%-30s %s\n' "$s" "$(grep -ciF "$s" docs/conventions/servers-and-drivers.md)"
done
```

Expected: every count ≥ 1.

- [ ] **Step 4: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/servers-and-drivers.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): servers-and-drivers area file

Freestanding build shape, branding, SEF and DS, MXBI packing, boot privilege
wiring, and the server-side rule halves of slices 5.3, 5.4, 5.7, 5.8, 5.10
and 5.11 -- short-write contracts, error-relay rules, the one-page-stack
discipline, and how to write a denial probe that fails loudly.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 8: `abi.md`

**Files:**

- Create: `docs/conventions/abi.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 313, 334, 337, 338, 354, 356, 357–358, 363–364,
  369, 534–561
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Produces: `docs/conventions/abi.md`. `kernel.md` and `servers-and-drivers.md` link here for band
  numbers and the freeze rather than restating them.

- [ ] **Step 1: Write the file**

- `Message` types live in `kernel-shared` and are shared across all crates (313).
- `kernel-shared` carries zero `unsafe` — byte-level ABI helpers decode field-by-field and tests tie
  the codec to the real layout via `offset_of!` (337).
- `kernel-shared` is unconditionally `no_std`; a `#[cfg(test)]` module may declare `extern crate
  std;` locally (338).
- `NR_SERVED_PROCS` is the one user-process capacity ceiling; all three per-process server tables
  derive from it and each carries a `const _` guard. Never reintroduce an independent literal (334).
- Errno bands (354): the POSIX block at magnitudes `1..=40` at classic-MINIX values identical to
  Linux/musl, the MINIX-specific IPC band at `>= 200`, and nothing in the `41..=199` gap. Constants
  are stored negated; add an errno by adding **one** line to the `errnos!` invocation, never by
  editing a second list.
- Generated C headers (354): C11 keywords only, `minixrs/ipc.h` includes nothing, every process gets
  both `<NAME>_PROC_NR` and `<NAME>_EP`, and `errno.h` asserts but never defines the POSIX block.
  **Adding a request number means editing `tools/gen-c-headers/src/callnr_h.rs`** — the per-band
  `members` lists are hand-maintained, so run `cargo test -p minixrs-gen-c-headers` after any band
  change, not just `cargo gen-c-headers`.
- Request-band allocation: `PM 0x700`, `VFS 0x800`, `FS 0x900`, `BDEV 0xA00`, `CDEV 0xB00`, `VM
  0xC00`, `SEF 0xD00`, `DS 0xE00`, `SCHED 0xF00`. Only ascending order is load-bearing, and
  `0x700..0xC00` is **fully allocated** — a tenth band needs a home outside that span.
- The grant ABI (356): flat `#[repr(C)] GrantEntry` with layout pinned by `offset_of!`, MINIX CPF
  flag values, `GRANT_SHIFT = 20` id packing.
- `uspace.rs` is an *address* ABI and is deliberately **not** emitted in the generated C headers
  (357–358).
- **D8 ABI freeze** (363–364): past slice 5.6, `Message` layout, call numbers, endpoints and errnos
  change only via a deliberate ABI-bump PR touching both repos. A musl fork rebase and the
  `external/musl` bump land in the **same** PR, because the port branch is force-pushed.

- [ ] **Step 2: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/abi.md
dprint check docs/conventions/abi.md && echo "FORMAT OK"
for s in "NR_SERVED_PROCS" "errnos!" "callnr_h.rs" "0x700" "0xF00" "offset_of!" \
         "D8" "force-pushed"; do
  printf '%-30s %s\n' "$s" "$(grep -cF "$s" docs/conventions/abi.md)"
done
```

Expected: every count ≥ 1.

- [ ] **Step 3: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/abi.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): abi area file

kernel-shared's zero-unsafe and no_std rules, the NR_SERVED_PROCS ceiling,
the two errno bands and the errnos! single-source rule, the generated-header
constraints including the hand-maintained callnr_h.rs band lists, the full
request-band allocation, and the D8 freeze.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 9: `testing-and-markers.md`

**Files:**

- Create: `docs/conventions/testing-and-markers.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 331, 346, and the mutation-testing paragraphs in
  563–634
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: `build-and-boot.md` (Task 3) for the boot commands themselves — link, do not restate.
- Produces: `docs/conventions/testing-and-markers.md`. Task 10's `docs-and-workflow.md` links here
  for the mutation-test discipline rather than duplicating it.

- [ ] **Step 1: Write the file**

Four sections:

1. **Verifying server behaviour** (346) — servers run at EL0 and cannot print; verify through
   kernel-side traces (`[pf]`, `[ksys …]`, `[ipc N]`), never server-side logging. Trace sampling is
   asymmetric: `[ipc N]` head-traces the first ~12 calls plus every 100th, `[ksys N]` samples every
   100th with no head carve-out.
2. **Trace forensics** (331) — the modulo sampler almost never catches low-rate callers; zero
   sampled lines is not evidence a caller is stuck; verify via downstream head-carved traces or a
   temporary `[DBG]` trace removed before committing.
3. **Markers** — the marker files test first occurrences only; `check-boot-log.sh` works on a
   partial log; its PASS total counts expected **and** forbidden lines; never copy a count from a
   plan, recompute it from the constant.
4. **Mutation testing** — the apply/observe/revert discipline; never revert with `git checkout
   <file>` (snapshot to the scratchpad and restore from there, then `diff -q` and `grep -rn
   MUTATION`); `git checkout -- <untracked file>` errors rather than restoring, so snapshot files a
   slice **adds** too; a mutation that fails to compile is indistinguishable from one that worked,
   so `grep -a 'error\[E' <log>` before recording any observation; iterate in the stub-free config;
   not every correct invariant has a mutation that moves a marker, and the honest record says so;
   run `cargo test` under the mutation too before recording a row as uncovered; a guard's mutation
   moves the marker of the probe that exists for that guard, not one on a healthy path.

- [ ] **Step 2: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/testing-and-markers.md
dprint check docs/conventions/testing-and-markers.md && echo "FORMAT OK"
for s in "check-boot-log.sh" "first occurrences" "MUTATION" "error\[E" "stub-free" \
         "head-carve" "EL0"; do
  printf '%-30s %s\n' "$s" "$(grep -ciF "$s" docs/conventions/testing-and-markers.md)"
done
```

Expected: every count ≥ 1.

- [ ] **Step 3: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/testing-and-markers.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): testing-and-markers area file

How to verify a server that cannot print, what the trace samplers do and do
not catch, the marker-file contract, and the mutation-testing discipline
including the restore hazards and the compile-failure false negative.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 10: `docs-and-workflow.md`

**Files:**

- Create: `docs/conventions/docs-and-workflow.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 563–634 and 635–651
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: `testing-and-markers.md` (Task 9) — link for mutation discipline.
- Produces: `docs/conventions/docs-and-workflow.md`, which owns the formatting rule that every other
  file obeys.

- [ ] **Step 1: Move the Documentation and Hunk sections**

```bash
cd /Users/kevinbarnard/src/minixrs
{
  echo '# Documentation and workflow'
  echo
  echo 'Which documentation tree owns what, how a slice is built and reviewed, and how markdown'
  echo 'in this repository is formatted.'
  echo
  sed -n '564,651p' /tmp/claude-md-split/CLAUDE.md.orig
} > docs/conventions/docs-and-workflow.md
```

Move the mutation-testing paragraph out — it now lives in `testing-and-markers.md` — and replace it
with a one-line link.

- [ ] **Step 2: Add the formatting section**

Append this section, ending with the command example so the nested fence stays last:

````markdown
## Markdown formatting

All markdown in this repository wraps prose at **100 columns**. This is mechanical, not a habit.

Configuration is `dprint.json` at the repo root, pinning dprint-plugin-markdown by checksum. Tables,
fenced code blocks, and bare URLs are exempt — dprint leaves them alone. `<!-- dprint-ignore -->`
exempts the block that follows it.

**`textWrap: "always"` reflows a whole paragraph**, so consecutive lines separated only by a newline
are joined before being rewrapped. Anything that must keep its own line has to be a real markdown
block: a list item, a table row, or a paragraph separated by a blank line.

Rust doc comments are out of scope — rustfmt does not reflow them and there is no gate that would.

```bash
dprint fmt          # format everything
dprint check        # verify, as CI will
```
````

- [ ] **Step 3: Add the plan-status pointer**

One short subsection stating that slice status is a GFM checkbox checked by the slice's own PR, with
a link to [`git-and-prs.md`](./git-and-prs.md) for the full rule. Do not restate it — one owner.

- [ ] **Step 4: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/conventions/docs-and-workflow.md
dprint check docs/conventions/docs-and-workflow.md && echo "FORMAT OK"
for s in "mdBook" "docs/plan.md" "superpowers" "hunk session" "dprint fmt" \
         "dprint-ignore" "100 columns"; do
  printf '%-30s %s\n' "$s" "$(grep -ciF "$s" docs/conventions/docs-and-workflow.md)"
done
grep -c 'testing-and-markers.md' docs/conventions/docs-and-workflow.md
```

Expected: every count ≥ 1, including the link to `testing-and-markers.md`.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/conventions/docs-and-workflow.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(conventions): docs-and-workflow area file

The documentation trees and who owns what, the superpowers slice process and
its review habits, Hunk review tooling, and the dprint-backed 100-column
formatting rule including the paragraph-joining consequence.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 11: `docs/conventions/README.md` and the CLAUDE.md rewrite

**Files:**

- Create: `docs/conventions/README.md`
- Rewrite: `CLAUDE.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 1–34 and 297–305

**Interfaces:**

- Consumes: all nine area files (Tasks 3–10) — they must exist before this task runs, because the
  index links to each by name.
- Produces: the ~150-line `CLAUDE.md` every session loads.

- [ ] **Step 1: Write the index**

`docs/conventions/README.md`: one short paragraph explaining that these files hold the project's
working rules and that `CLAUDE.md` links here, then a table of the nine files with a one-line "read
this before …" for each.

- [ ] **Step 2: Rewrite CLAUDE.md**

Structure, in order:

1. `# minix.rs` and the one-paragraph description (lines 1–4, kept).
2. `## Project Overview` (lines 5–15, kept).
3. `## Reference Codebase` (lines 16–34, kept — it is orientation, not a convention).
4. `## Architecture` (lines 297–305, kept — nine lines pointing at `book/`).
5. `## Non-negotiables` — the rules that bind every task regardless of area, each one line:
   - Read the relevant `docs/conventions/` file before working in that area.
   - Every new `.rs`/`.S` begins with the SPDX + copyright header.
   - Every commit is `--signoff`d and GPG-signed; never `--no-gpg-sign`, never `--no-verify`.
   - Never commit to `main`; branch first. Committing in auto mode is fine; **pushing, opening a PR,
     or triggering CI needs explicit approval**.
   - A PR marks its own work complete — check the box in the same PR.
   - Markdown prose wraps at 100 columns; run `dprint fmt`.
   - The D8 ABI freeze: `Message` layout, call numbers, endpoints and errnos change only by a
     deliberate ABI-bump PR touching both repos.
   - The kernel is bare-metal only and never host-built; QEMU is the verification path.
   - Verify before claiming: run the command, read the output, then say it passes.
6. `## Where things are documented` — the link table: the nine area files, plus `book/` (canonical
   how-it-works), `docs/plan.md` + `docs/plans/` (slice status and history), and `docs/superpowers/`
   (per-slice specs and plans).

Every other section of the old file is now a link. Do not restate rules that live in an area file.

- [ ] **Step 3: Verify size and that nothing orphaned**

```bash
cd /Users/kevinbarnard/src/minixrs
wc -l CLAUDE.md                      # expect 130-180
dprint fmt CLAUDE.md && dprint check CLAUDE.md && echo "FORMAT OK"
for f in build-and-boot ci git-and-prs kernel servers-and-drivers abi rust-style \
         testing-and-markers docs-and-workflow; do
  grep -qF "docs/conventions/$f.md" CLAUDE.md && echo "linked: $f" || echo "MISSING LINK: $f"
done
ls docs/conventions/*.md | wc -l     # expect 10 (nine areas + README)
```

Expected: 10 files, nine `linked:` lines, no `MISSING LINK`.

- [ ] **Step 4: Verify every link resolves**

```bash
cd /Users/kevinbarnard/src/minixrs
python3 - <<'PY'
import re, os, glob
bad = 0
for p in ['CLAUDE.md'] + glob.glob('docs/conventions/*.md'):
    base = os.path.dirname(p)
    for m in re.finditer(r'\]\((\.{0,2}[^)#\s]+\.md)', open(p, encoding='utf-8').read()):
        t = os.path.normpath(os.path.join(base, m.group(1)))
        if not os.path.exists(t):
            print(f"BROKEN {p} -> {m.group(1)}"); bad += 1
print("broken links:", bad)
PY
```

Expected: `broken links: 0`.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add CLAUDE.md docs/conventions/README.md
git commit --signoff -m "docs: CLAUDE.md becomes an index of non-negotiables and links

650 lines / 138 KB down to roughly 150. Orientation and architecture stay;
everything else is now a link into docs/conventions/, with the rules that
bind every task regardless of area kept inline as non-negotiables.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 12: Phase 4 slice records

**Files:**

- Modify: `docs/plans/phase-4-servers.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 325–330
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: `kernel.md` and `servers-and-drivers.md` — any rule already extracted there is **not**
  repeated here; the record links forward to it.
- Produces: per-slice record subsections in `docs/plans/phase-4-servers.md`.

- [ ] **Step 1: Locate the insertion points**

```bash
cd /Users/kevinbarnard/src/minixrs
grep -n '^###\|^- \*\*4\.' docs/plans/phase-4-servers.md | head -40
```

Each of slices 4.3, 4.4, 4.5, 4.6b, 4.7 and 4.8 gets its record appended to that slice's existing
section, under a `#### Design record` heading. If a slice has no section of its own, add one in
numeric order.

- [ ] **Step 2: Move the six records**

Lines 325 (4.3 delegatable scheduler), 326 (4.4 alarms), 327 (4.5 signals), 328 (4.6b
fork/exit/wait), 329 (4.7 exec) and 330 (4.8 init + wrap-up) move near-verbatim. Keep every
constant, endpoint number and invariant. Drop only sentences already extracted as rules into an area
file, replacing each with a link.

- [ ] **Step 3: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/plans/phase-4-servers.md
dprint check docs/plans/phase-4-servers.md && echo "FORMAT OK"
for s in "SCHEDULING_NO_QUANTUM" "alarm_at" "SYS_GETKSIG" "FORK_POOL_BASE" \
         "EXEC_ONLY_PROC_NR" "INIT_PROC_NR"; do
  printf '%-30s %s\n' "$s" "$(grep -cF "$s" docs/plans/phase-4-servers.md)"
done
```

Expected: every count ≥ 1 — each is a distinctive token from one of the six records.

- [ ] **Step 4: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/plans/phase-4-servers.md docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(plans): phase-4 slice design records

Moves the 4.3-4.8 records out of CLAUDE.md and beside the slices they
document: scheduler delegation, per-proc alarms, minimal signals, PM-driven
fork/exit/wait, PM-driven exec, and init.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 13: Phase 5 slice records

The largest move: twelve records totalling roughly 60 KB.

**Files:**

- Modify: `docs/plans/phase-5-musl-fs.md`, `docs/plans/phase-5-prep.md`
- Read: `/tmp/claude-md-split/CLAUDE.md.orig` lines 332, 333, 334, 354–561
- Modify: `docs/superpowers/plans/2026-09-17-routing-checklist.md`

**Interfaces:**

- Consumes: all four technical area files — a rule already extracted is linked, not repeated.
- Produces: per-slice `#### Design record` subsections in `docs/plans/phase-5-musl-fs.md`, and three
  chunk records in `docs/plans/phase-5-prep.md`.

- [ ] **Step 1: Move the three prep-chunk records**

Lines 332 (chunk 7, de-hosting the kernel crate), 333 (chunk 3, the `boot-stubs` feature) and 334
(chunk 4, `NR_SERVED_PROCS`) go to `docs/plans/phase-5-prep.md` beside their chunk entries. The rule
halves already live in `kernel.md`, `build-and-boot.md` and `abi.md` respectively — link forward.

- [ ] **Step 2: Locate the phase-5 insertion points**

```bash
cd /Users/kevinbarnard/src/minixrs
grep -n '^#### Slice 5\.\|^### Slice 5\.' docs/plans/phase-5-musl-fs.md
```

- [ ] **Step 3: Move the twelve slice records**

In source order: 354 (5.0), 355 (5.1), 356 (5.2), 357–358 (5.3), 359–360 (5.4), 361–362 (5.5),
363–364 (5.6), 365–366 (5.7), 367–368 (P3c — its own subsection, it is out-of-band toolchain work),
369 (5.8), 370–419 (5.9), 420–480 (5.10a), 481–533 (5.10b), 534–561 (5.11).

Preserve every measurement verbatim — the boot-ratio percentages, the marker names, the byte counts.
These are the parts that cannot be re-derived. Drop only sentences already extracted as rules.

- [ ] **Step 4: Format and verify**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/plans/phase-5-musl-fs.md docs/plans/phase-5-prep.md
dprint check docs/plans/phase-5-musl-fs.md docs/plans/phase-5-prep.md && echo "FORMAT OK"
for s in "61.61%" "26.90%" "GRANT_SHIFT" "ATTR_IDX_NORMAL" "vfs.long ok" \
         "EXEC_STACK_PROBE_PASS" "ROOTFS_IMAGE_BLOCKS" "find_free_slot" \
         "dev.console ok" "46,664"; do
  printf '%-30s %s\n' "$s" "$(grep -cF "$s" docs/plans/phase-5-musl-fs.md)"
done
for s in "forced-target" "boot-stubs" "NR_SERVED_PROCS"; do
  printf '%-30s %s\n' "$s" "$(grep -cF "$s" docs/plans/phase-5-prep.md)"
done
```

Expected: every count ≥ 1. A zero on a percentage or a byte count means a measurement was lost,
which is the one thing this task cannot regenerate.

- [ ] **Step 5: Commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git add docs/plans/phase-5-musl-fs.md docs/plans/phase-5-prep.md \
        docs/superpowers/plans/2026-09-17-routing-checklist.md
git commit --signoff -m "docs(plans): phase-5 slice and prep-chunk design records

Moves the 5.0-5.11 records and the P3c toolchain record out of CLAUDE.md and
beside the slices they document, plus the three prep-chunk records. Every
measurement -- boot ratios, marker names, image sizes -- is preserved
verbatim; these are the parts that cannot be re-derived.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 14: Status convention in the trackers

**Files:**

- Modify: `docs/plan.md`, `docs/plans/phase-2-ipc.md`, `docs/plans/phase-3-vm.md`,
  `docs/plans/phase-4-servers.md`, `docs/plans/phase-5-musl-fs.md`, `docs/plans/phase-5-prep.md`

**Interfaces:**

- Consumes: `docs/conventions/git-and-prs.md` (Task 4) — the note links to it for the full rule.

- [ ] **Step 1: Add the status-convention note**

At the top of `docs/plan.md` and each `docs/plans/*.md`, replacing any existing marker-convention
sentence:

```markdown
> **Status convention.** A slice's status is a GFM checkbox — `- [ ]` not started, `- [x]` done —
> checked by the PR that does the work, in that same PR. The first unchecked box in plan order is
> the next slice. Lines in the older form, `✓ shipped (PR #N, merged YYYY-MM-DD)`, are retired-form
> history; never write a new one. Full rule:
> [`docs/conventions/git-and-prs.md`](../conventions/git-and-prs.md).
```

Adjust the relative link depth per file (`docs/plan.md` uses `conventions/git-and-prs.md`).

- [ ] **Step 2: Close slice 5.11 in the retired form**

It merged as PR #57 on 2026-09-06 without checking its own box, so it is the boundary: retired form
ends here.

```bash
cd /Users/kevinbarnard/src/minixrs
grep -n '5\.11' docs/plan.md docs/plans/phase-5-musl-fs.md | grep -i 'ready\|pending'
```

Replace `◀ ready (branch feature/slice-5.11-dev-null-zero, pending merge)` with `✓ shipped (PR #57,
merged 2026-09-06)` in both files.

- [ ] **Step 3: Convert the Phase 6 entries to checkboxes**

In `docs/plan.md`'s Phase 6 section, rewrite the five bullets as unchecked boxes:

```markdown
- [ ] `drivers/driver-rt/`: VirtIO MMIO transport (aarch64), virtqueue management, BDEV/CDEV
      protocol
- [ ] `drivers/virtio-blk/`: Block device
- [ ] `drivers/virtio-console/`: TTY
- [ ] `drivers/virtio-net/`: Network (packet I/O only; TCP/IP stack is later)
- [ ] Root filesystem on VirtIO disk
```

- [ ] **Step 4: Verify no stale markers remain**

```bash
cd /Users/kevinbarnard/src/minixrs
grep -rn '◀ next\|◀ ready\|pending merge' docs/ || echo "NO STALE MARKERS"
grep -c 'Status convention' docs/plan.md docs/plans/*.md
grep -n '5\.11' docs/plan.md | head -2
```

Expected: `NO STALE MARKERS`; every plan file reports 1; the 5.11 line shows the retired form.

- [ ] **Step 5: Format and commit**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt docs/plan.md docs/plans/
dprint check docs/plan.md docs/plans/ && echo "FORMAT OK"
git add docs/plan.md docs/plans/
git commit --signoff -m "docs(plans): checkbox status convention; close 5.11

Adds the status-convention note to every tracker, closes slice 5.11 in the
retired form as the boundary, converts Phase 6 to unchecked boxes, and
retires the last '◀ next' and '◀ ready (pending merge)' markers. From here a
slice's own PR checks its own box.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 15: Tree-wide reformat

One isolated commit containing no content changes, so `git blame` can skip it.

**Files:**

- Modify: every `.md` not already dprint-clean (chiefly `book/src/**`, `docs/superpowers/**`,
  `README.md`, `RELEASING.md`)
- Create: `.git-blame-ignore-revs`

- [ ] **Step 1: Confirm the working tree is clean**

```bash
cd /Users/kevinbarnard/src/minixrs
git status --porcelain     # expect empty except untracked PRE6-RECOMMEND.md
```

A dirty tree here would mix content into the reformat commit, defeating its purpose.

- [ ] **Step 2: Reformat everything**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint fmt
git diff --stat | tail -3
```

- [ ] **Step 3: Prove the diff is whitespace-only**

```bash
cd /Users/kevinbarnard/src/minixrs
git diff --ignore-all-space --stat | tail -5
```

Expected: empty, or only files where dprint changed emphasis/list markers. Inspect any file that
still shows a diff under `--ignore-all-space` and confirm it is a marker normalisation, not lost
words. If words changed, stop — the config is wrong.

- [ ] **Step 4: Commit the reformat alone**

```bash
cd /Users/kevinbarnard/src/minixrs
git add -u
git commit --signoff -m "style: reflow all markdown to 100 columns with dprint

Mechanical reformat, no content changes. Listed in .git-blame-ignore-revs by
the following commit.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
REFORMAT_SHA=$(git rev-parse HEAD)
echo "$REFORMAT_SHA"
```

- [ ] **Step 5: Record the SHA and commit**

```bash
cd /Users/kevinbarnard/src/minixrs
cat > .git-blame-ignore-revs <<EOF
# Revisions to skip in \`git blame\`. Opt in locally with:
#   git config blame.ignoreRevsFile .git-blame-ignore-revs

# style: reflow all markdown to 100 columns with dprint
$REFORMAT_SHA
EOF
git add .git-blame-ignore-revs
git commit --signoff -m "chore: ignore the markdown reformat in git blame

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
git config blame.ignoreRevsFile .git-blame-ignore-revs
git blame -L1,3 CLAUDE.md | cut -c1-60
```

Expected: blame attributes the lines to a content commit, not the reformat.

---

### Task 16: Final verification

The gate the whole branch exists for. Every earlier task saw one file; this is the only step
positioned to notice a rule that vanished.

**Files:**

- Delete: `docs/superpowers/plans/2026-09-17-routing-checklist.md`
- Modify: whatever the sweeps find

- [ ] **Step 1: Assert every checklist row is ticked**

```bash
cd /Users/kevinbarnard/src/minixrs
python3 - <<'PY'
import re
rows = [l for l in open('docs/superpowers/plans/2026-09-17-routing-checklist.md',
                        encoding='utf-8') if re.match(r'\|\s*R\d+\s*\|', l)]
unticked = [l for l in rows if not re.search(r'\|\s*(x|X|✓)\s*\|\s*$', l.rstrip())]
print(f"rows={len(rows)} unticked={len(unticked)}")
for l in unticked[:20]:
    print("  UNTICKED:", l.strip()[:110])
PY
```

Expected: `unticked=0`. Any unticked row is a dropped rule — restore it before continuing.

- [ ] **Step 2: Assert each rule appears exactly once in the tree**

```bash
cd /Users/kevinbarnard/src/minixrs
python3 - <<'PY'
import re, subprocess
rows = [l for l in open('docs/superpowers/plans/2026-09-17-routing-checklist.md',
                        encoding='utf-8') if re.match(r'\|\s*R\d+\s*\|', l)]
for l in rows:
    c = l.split('|')
    rid, rule = c[1].strip(), c[3].strip()
    probe = max(re.findall(r'`[^`]+`', rule) or [''], key=len).strip('`')
    if len(probe) < 4:
        continue
    out = subprocess.run(['grep', '-rlF', probe, 'CLAUDE.md', 'docs/conventions', 'docs/plans'],
                         capture_output=True, text=True).stdout.split()
    if len(out) == 0:
        print(f"{rid}: MISSING  '{probe}'")
PY
```

Expected: no `MISSING` lines. This probes on the rule's most distinctive backticked token, so it
catches deletions rather than rewordings.

- [ ] **Step 3: Sweep for cross-references to moved content**

```bash
cd /Users/kevinbarnard/src/minixrs
grep -rn 'CLAUDE.md' --include='*.md' --include='*.rs' . \
  | grep -v '^./docs/superpowers/' | grep -v '^./CLAUDE.md'
```

Every hit must still be true. A comment saying "see CLAUDE.md's Code Conventions" is now false —
re-point it at the area file.

- [ ] **Step 4: Sweep for falsified claims**

```bash
cd /Users/kevinbarnard/src/minixrs
grep -rniE 'until slice [0-9]|there is no .* yet|nothing reaches this|unreachable at boot' \
  --include='*.md' CLAUDE.md docs/conventions docs/plans | head -30
```

Check each against what this branch actually did. This branch relocates the prose these claims live
in, which is the condition under which they go stale unnoticed.

- [ ] **Step 5: Whole-tree format and link check**

```bash
cd /Users/kevinbarnard/src/minixrs
dprint check && echo "FORMAT CLEAN"
python3 - <<'PY'
import re, os, glob
bad = 0
for p in ['CLAUDE.md'] + glob.glob('docs/**/*.md', recursive=True):
    base = os.path.dirname(p)
    for m in re.finditer(r'\]\((\.{0,2}[^)#\s:]+\.md)', open(p, encoding='utf-8').read()):
        t = os.path.normpath(os.path.join(base, m.group(1)))
        if not os.path.exists(t):
            print(f"BROKEN {p} -> {m.group(1)}"); bad += 1
print("broken links:", bad)
PY
```

Expected: `FORMAT CLEAN` and `broken links: 0`.

- [ ] **Step 6: Confirm no code changed**

```bash
cd /Users/kevinbarnard/src/minixrs
git diff --stat main...HEAD -- '*.rs' '*.S' '*.toml' '.github/**' | tail -3
```

Expected: empty except `dprint.json` if your `*.toml` glob catches it (it should not — dprint.json
is JSON). Any `.rs` or workflow change is out of scope and must be reverted.

- [ ] **Step 7: Delete the checklist and commit**

```bash
cd /Users/kevinbarnard/src/minixrs
git rm docs/superpowers/plans/2026-09-17-routing-checklist.md
git add -A
git commit --signoff -m "docs: retire the routing checklist

Every row ticked and every rule confirmed present in exactly one destination.
The checklist was scaffolding for the split; the split is done.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
git log --oneline main..HEAD
```

- [ ] **Step 8: Report, do not push**

Summarise for review: final `CLAUDE.md` line count, the ten `docs/conventions/` files with sizes,
which rules were deliberately dropped and why, and anything found by the Step 3/4 sweeps. **Do not
push and do not open a PR** — that needs explicit approval.

---

## Notes for the executor

- **The pristine copy is the source of truth.** Every task reads
  `/tmp/claude-md-split/CLAUDE.md.orig`, never the live `CLAUDE.md`, which is being emptied as tasks
  land. If that file is missing, restore it with `git show 74df1a7:CLAUDE.md`.
- **Tasks 3–10 are independent** once Tasks 1–2 land; they touch disjoint files apart from the
  shared checklist. Tasks 11–14 are strictly ordered after them. Tasks 15–16 are strictly last.
- **The checklist is the only shared mutable file** among the parallel tasks. If two tasks run
  concurrently, expect a conflict there and resolve it by taking both sets of ticks.
- **When a rule is ambiguous between two area files, put it in one and link from the other.**
  Duplicating it guarantees the two copies drift, which is the failure mode this whole branch exists
  to fix.
