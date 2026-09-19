# Git, commits, and pull requests

Branching, signing, sign-off, and what a PR owes the plan trackers.

## Branching and pushing

- Always work on a feature branch — never commit directly to `main` or `master`. Prefix branch names
  by kind: `feature/`, `bugfix/`, `chore/`, `release/`.
- PRs land by **regular merge commit**, never squash — squashing destroys the per-commit history.
  Rebase merge is reserved for a solo series where every commit is yours *and* a linear history is
  specifically wanted; it is not the default, for the same signing reason the Commits section below
  covers in detail: a merge preserves every commit object byte-for-byte, so each author's GPG
  signature reaches `main` intact, while a rebase replays commits under new SHAs and cannot carry a
  third party's signature.
- In auto-accept (autonomous) mode, committing on a feature branch is fine — signed and signed-off
  as always. **Pushing, opening a PR, or triggering CI requires explicit human approval.** Nothing
  that leaves the machine happens without that approval first; local commits are cheap and
  reversible, a push is not.

## Commits

- **Every commit must carry a DCO sign-off** — always `git commit --signoff` (`-s`), which appends
  `Signed-off-by: Kevin Barnard <kevin.barnard@gmail.com>` from `user.name`/`user.email`. It is the
  Developer Certificate of Origin attestation, not a stylistic trailer: it asserts the author has
  the right to contribute the code under BSD-3-Clause. Never hand-write the trailer for someone
  else, and never add one on their behalf — `--signoff` signs off *as the committer*, which is the
  whole point. Every non-merge commit in this repo's history has one; keep it that way
- Sign-off is **orthogonal to GPG signing** — `--signoff` is the DCO trailer, `--gpg-sign` (on by
  default here) is the cryptographic signature. Both are required, and neither substitutes for the
  other. The global rules still hold: never `--no-gpg-sign`, never `--no-verify`
- Trailer **presence** is what matters, not order: `--signoff` appends the sign-off last, but the
  `Entire-Checkpoint:` trailer is inserted by a hook that may run either side of the commit, so it
  lands before *or* after `Signed-off-by:` depending on timing — both orderings are in the history
  and neither is wrong. Verify with `git log -1 --format='%(trailers:key=Signed-off-by)'` before
  pushing, especially after a `git commit --amend` that omitted `-s`
- PRs land by **regular merge commit** (never squash; rebase merge only for a solo series where a
  linear history is specifically wanted). A merge preserves every commit object byte-for-byte, so
  both the `Signed-off-by:` trailer **and** the author's GPG signature reach `main` intact — the
  signature being the half a rebase cannot carry, since replayed commits are new objects re-signed
  by whoever ran the rebase (your key locally, GitHub's web-flow key through the button). On a
  public repo expecting outside contributions that is a false attestation rather than a cosmetic
  loss, which is why the earlier rebase-by-default rule was reversed on 2026-08-30. Merging through
  **GitHub's merge button is fine** here precisely because it touches only the merge commit and
  never rewrites the authored commits beneath it — so there is no reason to push to `main` locally
- History note: PRs up to **#48** landed as merge commits, **#49–#53** under the brief
  rebase-by-default rule (2026-07-28 `1132e62` … 2026-08-30), and everything after as merge commits
  again. So `main` reads as merge bubbles, then one linear run, then merge bubbles again — three
  deliberate stretches rather than a broken history
- A merge commit has **no** sign-off; that is expected and not worth fixing, and
  `tools/check-dco.sh` skips merge commits for exactly that reason. Only authored commits need one
- This is **enforced, not just documented**: the blocking `dco` CI gate runs `tools/check-dco.sh`
  over every non-merge commit a PR adds. Run it locally before pushing — bare `tools/check-dco.sh`
  defaults to `<merge-base with origin/main>..HEAD`. It matches the sign-off's **email** against the
  commit author's (case-insensitively), not the display name, so a trailer naming someone else
  fails: `--signoff` signs off as the committer, and a relayed patch needs its *author's* sign-off.
  Fixes are `git commit --amend --signoff` for the tip, `git rebase --signoff <base>` for a branch

## A PR marks its own work complete

A PR is atomic: it contains the work **and** the record that the work is done. The PR that
implements a slice checks that slice's box in `docs/plan.md` and, **when that file exists**, in the
matching `docs/plans/phase-N-*.md`, in the same PR, as part of the same change. Not every phase has
a slice tracker. `docs/plans/phase-6-prep.md` is the pre-Phase-6 chunk tracker (like
`phase-5-prep.md`); a Phase 6 *slice* still checks its box in `docs/plan.md` only until chunk 4
lands `docs/plans/phase-6-virtio.md`. Do not treat `phase-6-prep.md` as the Phase 6 slice file, and
do not invent a `phase-6-*.md` just to hold crate-path boxes.

Marking completion is never a follow-up commit, never a separate PR, and never a cleanup task
inherited by the next slice. Those are the shapes that go stale — the tracker carried a wrong status
at slices 4.8, 5.9, and 5.11 for exactly this reason.

No PR number, no merge date, no "next" pointer, no "pending merge" state. **The first unchecked box
in plan order is the next slice.** Which PR and when are questions `git log` and the GitHub PR list
answer better than a hand-maintained line.

Lines in the older form — `✓ shipped (PR #N, merged YYYY-MM-DD)` — are retired-form history, kept
because rewriting them would churn four files to no benefit. Never write a new one.

Status is a GFM checkbox and nothing else — here are `docs/plan.md`'s first two Phase 6 rows, as
they will read once the first of them has shipped:

```markdown
- [x] `drivers/driver-rt/`: VirtIO MMIO transport (aarch64), virtqueue management, BDEV/CDEV
      protocol
- [ ] `drivers/virtio-blk/`: Block device
```

Check the box the tracker already carries; never invent a row, and never invent a slice number that
is not on it.
