# Splitting CLAUDE.md into areas of concern

- **Date:** 2026-09-17
- **Status:** approved, pending implementation plan
- **Topic:** restructure the project instruction file, adopt a mechanical markdown format, and
  change the slice-completion convention

---

## Problem

`CLAUDE.md` is 650 lines and 138 KB. Three distinct problems have accumulated in it.

**It is too large to route.** An agent about to touch a driver must read past the errno ABI, the
boot-timing budget, and four phases of scheduler history to find the two rules that bind it. The
file is loaded into every session's context in full, so its size is a fixed tax on every task
regardless of what that task touches.

**It mixes rules with history.** `## Code Conventions` alone is 105 KB — 76% of the file — and holds
two different kinds of content in the same bullet list. Roughly 35 KB are durable rules that bind
future work ("offset arithmetic in a server crate must use `checked_add`"). Roughly 70 KB are
per-slice design records describing what a slice established and what it cost to learn ("5.10b moved
the last marker from 26.90% to 61.61% of a 240 s log"). The second kind is valuable, but it belongs
beside the slice it documents, not in the file every agent reads first.

**It is written in single lines.** Individual lines reach 7,158 characters. The file is effectively
unreadable in a terminal, in a diff, and in review.

Separately, the slice-completion convention has a structural flaw. A slice's plan entry is marked `✓
shipped (PR #N, merged YYYY-MM-DD)` **after** its PR merges, because the PR cannot know its own
number or merge date. That makes completion a follow-up edit — a separate PR, or a cleanup task
folded into the next slice — and the follow-up is routinely forgotten. The stale-marker footgun has
recurred at slices 4.8, 5.9, and 5.11; at the time of writing, `docs/plan.md:538` still labels 5.11
`◀ ready (branch …, pending merge)` although PR #57 merged on 2026-09-06. The recorded information —
PR number and merge date — is also already available from `git log` and the GitHub PR list.

---

## Decisions

Settled during brainstorming, in order:

1. **Per-slice design records merge into `docs/plans/phase-N-*.md`.** No new tree for them. `book/`
   stays canonical for how the system works; `docs/plans/` stays canonical for slice status and now
   also carries each slice's record.
2. **`CLAUDE.md` becomes an index plus non-negotiables**, roughly 150 lines. Everything else moves
   out behind links.
3. **Area files live in `docs/conventions/`** — a new tree parallel to `docs/plans/`. Not `.claude/`
   (human contributors would not find it) and not `book/` (build commands and review tooling are not
   teaching material, and `docs.yml` is path-filtered to `book/**`).
4. **Slice status uses GFM checkboxes**, `- [x]` and `- [ ]`.
5. **No "next" pointer.** The first unchecked box in plan order is the next slice. A pointer is a
   second thing to move in every PR and is exactly what has gone stale three times.
6. **History is not rewritten.** Existing `✓ shipped (PR #N, merged …)` lines stay as they are; the
   new form applies from the next slice onward.
7. **The 100-column rule covers all repo markdown, prose only.** Tables, fenced code, and bare URLs
   are exempt.
8. **`docs/conventions/` is split nine ways**, one file per area.
9. **Formatting is mechanical, via dprint.** The rule is a command, not a habit.

---

## Target tree

```
CLAUDE.md                          ~150 lines: overview, non-negotiables, link table
docs/conventions/
  README.md                        one-paragraph index; what each file is for
  build-and-boot.md                build commands, QEMU boot budget and how to re-measure it,
                                   the three hello flavors, ELF and stack-frame inspection
  ci.md                            the eleven gates, which block, the pre-push checklist,
                                   the SDK coverage gap
  git-and-prs.md                   branch and merge policy, GPG signing, DCO sign-off,
                                   auto-mode commit-yes/push-no, PR atomicity
  kernel.md                        UnsafeCell tables, NLL borrow-ending, IPC slice passing,
                                   rts_set/rts_unset, bare-metal-only crate, uaccess and the
                                   never-dereference-a-user-VA rule
  servers-and-drivers.md           user.ld and brand!(), ELF cfg_attr gating, SEF and DS,
                                   MXBI packing, EL0 has no console, boot privilege wiring,
                                   VM regions, one-page-stack discipline
  abi.md                           kernel-shared carries zero unsafe, errno bands, generated
                                   C headers, request-band allocation, the D8 freeze
  rust-style.md                    SPDX headers, checked_add, clippy traps, no_std-under-test
                                   configuration, doc_lazy_continuation
  testing-and-markers.md           marker files, mutation-test discipline, trace forensics,
                                   boot-ratio measurement
  docs-and-workflow.md             book/ vs docs/plans/ vs docs/superpowers/, the slice process,
                                   review habits, Hunk review tooling, the formatting rule
dprint.json                        markdown formatter configuration
.git-blame-ignore-revs             names the tree-wide reformat commit
```

Nine files rather than three or five because each is genuinely a distinct area an agent works
inside, and each lands between 1 KB and 12 KB — small enough to be read end to end, which is the
property that makes a split worth doing at all.

---

## Content routing

Moving `## Build`, `## CI`, `## Commits`, `## Documentation`, and `## Review tooling: Hunk` into
their area files is close to verbatim. The per-slice records are not, because most of them mix two
kinds of content inside one bullet. The routing rule:

- A sentence phrased as a standing instruction that binds future work — "take the granter from the
  kernel-stamped `m_source`, never from the payload", "the kernel never dereferences a user VA" — is
  **extracted into its area file as a rule**, stated imperatively, with provenance reduced to a
  parenthetical slice number.
- A sentence describing what a slice established, measured, or learned — "the mutation matrix
  confirmed both orderings are unreachable", "the budget went to 600 s because the last marker moved
  from 26.90% to 61.61%" — **moves to `docs/plans/phase-N-*.md`** beside that slice's entry.
- A bullet containing both is split. The area file then carries a one-line link back to the plan
  section, so a rule that looks arbitrary can be traced to the failure that produced it.

Cross-phase records — the kernel de-hosting, the `boot-stubs` feature, the `NR_SERVED_PROCS`
unification — go to `docs/plans/phase-5-prep.md`, where those chunks are already tracked.

This is a rewrite, not a `sed` invocation, and it is where the work can go wrong: a rule silently
dropped during distillation leaves no trace. See Verification below.

---

## Status convention

From the next slice onward, plan entries are GFM checkboxes:

```markdown
- [x] **5.11** /dev/null + /dev/zero on the memory driver + CDEV_READ
- [ ] **6.1** SYS_IRQCTL + HARDWARE NOTIFY
```

No PR number, no merge date, no `◀ next`, no `◀ ready (pending merge)`.

**The PR that does the work checks its own box.** Marking completion is never a follow-up commit, a
separate PR, or a cleanup task inherited by the next slice. A PR is atomic: it contains the work and
the record that the work is done. Which PR and when are questions `git log` and the GitHub PR list
answer better than a hand-maintained line ever did.

`◀ ready (branch …, pending merge)` is retired outright. That state existed only to describe work
finished but not yet recorded — the gap this change closes.

**Two conventions will be visible in the same files**, because history is not being rewritten. To
keep an agent from copying whichever form it sees first, `docs/plan.md` and each `docs/plans/*.md`
gets a short **Status convention** note at the top stating that checkboxes are current, that `✓
shipped (PR #N, merged …)` lines are retired-form history, and that no new line may use the retired
form.

**Slice 5.11 is the boundary.** Its PR merged without checking its own box, so it must be recorded
now regardless. It is closed in the *retired* form — `✓ shipped (PR #57, merged 2026-09-06)` — so
that the boundary is clean: retired form ends at 5.11, checkboxes begin at Phase 6.

---

## Formatting

`dprint.json` at the repo root, markdown plugin pinned by version and checksum in the repo's
existing SHA-pinning style:

```json
{
  "lineWidth": 100,
  "markdown": {
    "textWrap": "always",
    "emphasisKind": "asterisks",
    "strongKind": "asterisks",
    "unorderedListKind": "dashes"
  },
  "excludes": ["target/**", "external/**", "book/book/**"],
  "plugins": ["https://plugins.dprint.dev/markdown-0.24.0.wasm@cf7e65674b7eb5d91f85152ae9020dc8b3e73bd7944efcbfb05df71081217764"]
}
```

Verified empirically against real copies of `CLAUDE.md`, `PRE6-RECOMMEND.md`, and `docs/plan.md`
using dprint 0.57.4 with markdown plugin 0.24.0:

| Property          | Observed                                                                     |
| ----------------- | ---------------------------------------------------------------------------- |
| Prose width       | exactly 100 characters maximum; `CLAUDE.md` 650 → 1580 lines, 7158 → 100 max |
| Tables            | untouched — a 509-character row survived unchanged                           |
| Fenced code       | untouched — the `sh` block's long `#` comment lines kept verbatim            |
| `◀` `✓` em-dashes | preserved (39 glyphs intact in `docs/plan.md`)                               |
| Escape hatch      | `<!-- dprint-ignore -->` leaves the following block alone                    |
| CI mode           | `dprint check` passes silently, fails dirty                                  |

The three style keys are not cosmetic: the plugin defaults to `_underscore_` emphasis, which would
rewrite every `*word*` in the tree. With `emphasisKind: "asterisks"` the existing style is
preserved.

The rule in `docs-and-workflow.md` is therefore "run `dprint fmt`", not a width an agent has to
eyeball.

**One behavioural consequence the split work must respect:** `textWrap: "always"` reflows a whole
markdown *paragraph*, so consecutive lines separated only by a newline are joined before being
rewrapped. Anything that must keep its own line — a metadata header, a run of short labelled fields
— has to be a real markdown block: a list item, a table row, or a paragraph separated by a blank
line. Hand-broken lines inside one paragraph will not survive `dprint fmt`. This was found by
formatting this spec, whose own header had to become a list.

**The tree-wide reformat lands as one isolated commit** containing no content changes, and
`.git-blame-ignore-revs` names its SHA so `git blame` stays useful across roughly ten thousand
reflowed lines.

**A CI gate is deferred**, per the decision that dprint's value now is making the gate easy later.
The recipe is recorded so the follow-up is small: install the binary in the **existing** `fmt` job
(`curl -fsSL https://dprint.dev/install.sh | sh`) and add `dprint check` to it. Adding a twelfth
blocking gate is avoidable, and `dprint/check-action` would be another third-party action to
SHA-pin.

---

## Verification

The mechanical moves verify themselves — a section is present in its new file or it is not. The
distillation does not, so it gets an explicit check rather than a reviewer's memory:

1. Before any content moves, extract every imperative sentence from the current `CLAUDE.md` into a
   checklist file in the scratchpad.
2. After the split, confirm each entry appears in exactly one destination — an area file or a plan
   file. Zero occurrences is a dropped rule; two is a duplicate that will drift.
3. That checklist is the acceptance criterion for the whole-branch review, not a task-level one:
   each task sees one area file, so no task is in a position to notice a rule that vanished.

Three sweeps the repo's own conventions already require, applied here:

- `grep -rn 'CLAUDE.md' --include='*.md' .` — cross-references that now point at moved content.
- The falsified-claim sweep for `until slice N`, `there is no X yet`, and similar tripwires, since
  this branch relocates the prose those claims live in.
- `dprint check` clean on the whole tree.

No boot is required: this branch changes no code. `cargo fmt --all --check` and the clippy gates
should nonetheless be run before pushing, because the branch touches `.git-blame-ignore-revs` and
`dprint.json` at the root and a typo there is cheap to catch.

---

## Out of scope

- Rewriting `book/` content. Only its wrapping changes.
- Rewriting historical `✓ shipped` lines into checkboxes.
- Adding the dprint CI gate (recipe recorded above; deferred deliberately).
- Extending the 100-column rule to Rust doc comments. rustfmt does not reflow them, so the rule
  would be hand-maintained tree-wide with no gate — large diff, high drift.
- `PRE6-RECOMMEND.md`, currently untracked at the repo root. It is Phase 6 planning material and
  should land in `docs/plans/`, but that is its own change.
