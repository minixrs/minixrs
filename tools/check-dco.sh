#!/usr/bin/env bash
#
# tools/check-dco.sh -- verify every non-merge commit in a range carries a
# Developer Certificate of Origin sign-off matching its author.
#
#   usage: tools/check-dco.sh [<range>]
#
# Default range is <merge-base with the default branch>..HEAD, i.e. the commits
# this branch adds. CI passes an explicit <base-sha>..<head-sha> for the PR.
#
# A commit passes when it carries a `Signed-off-by:` trailer whose email
# matches the commit author's, case-insensitively. Matching on *email* and not
# the display name is deliberate: the email is the identity the DCO attests to,
# while names get typo'd, transliterated, and reformatted by tooling.
#
# Merge commits are skipped -- GitHub's UI writes them and cannot sign them
# off, which is expected and documented in CLAUDE.md's "Commits" section.
#
# An empty range is a FAILURE, not a vacuous pass: the only caller is a PR gate
# where zero authored commits means the range was computed wrong, and a gate
# that goes green on a broken range is worse than no gate. Every violation is
# reported (not just the first) before the non-zero exit, so one run shows the
# whole story.
#
# Kept to bash 3.2 (no `mapfile`, no `${var,,}`) so it runs on a stock macOS
# /bin/bash as well as CI's bash 5 -- a check you cannot run before pushing is
# a check you find out about from a red X.

set -euo pipefail

if [[ $# -gt 1 ]]; then
    echo "usage: $0 [<range>]" >&2
    exit 64
fi

RANGE=""
if [[ $# -eq 1 ]]; then
    RANGE="$1"
else
    # Prefer the remote-tracking tip so the range is right on a branch whose
    # local `main` is stale; fall back to a local `main` for offline use.
    for base in origin/main main; do
        if git rev-parse --verify --quiet "$base" >/dev/null; then
            RANGE="$(git merge-base "$base" HEAD)..HEAD"
            break
        fi
    done
    if [[ -z "$RANGE" ]]; then
        echo "error: no origin/main or main to diff against; pass a range" >&2
        exit 64
    fi
fi

if ! git rev-parse --verify --quiet "${RANGE%%..*}" >/dev/null; then
    echo "error: bad range (left side unknown): $RANGE" >&2
    exit 66
fi

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# Hex SHAs are safe to word-split on, which keeps this bash 3.2 clean (no
# `mapfile`) and keeps the loop body in this shell, where `fail` survives.
commits="$(git rev-list --no-merges "$RANGE")"

if [[ -z "$commits" ]]; then
    echo "FAIL: no non-merge commits in range $RANGE"
    echo "    A PR always adds at least one authored commit, so this almost"
    echo "    certainly means the range is wrong rather than that the branch"
    echo "    is clean. Check the base/head SHAs before trusting this gate."
    exit 1
fi

fail=0
checked=0

for sha in $commits; do
    checked=$((checked + 1))
    author="$(git log -1 --format='%ae' "$sha")"
    subject="$(git log -1 --format='%s' "$sha")"
    # One trailer per line; a commit may legitimately carry several (e.g. a
    # patch relayed by a maintainer), and any one matching the author suffices.
    signoffs="$(git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$sha")"

    matched=0
    while IFS= read -r trailer; do
        [[ -z "$trailer" ]] && continue
        # `Name <email>` -> `email`; a trailer with no angle brackets is
        # malformed and simply will not match.
        email="${trailer##*<}"
        email="${email%%>*}"
        if [[ "$(lower "$email")" == "$(lower "$author")" ]]; then
            matched=1
            break
        fi
    done <<< "$signoffs"

    if [[ "$matched" -eq 0 ]]; then
        echo "NO SIGN-OFF  ${sha:0:12}  $subject"
        echo "    author:   $author"
        if [[ -z "$signoffs" ]]; then
            echo "    trailers: (none) -- recommit with: git commit --amend --signoff"
        else
            echo "    trailers: $(echo "$signoffs" | tr '\n' ';')"
            echo "    none of those match the author; --signoff signs off as"
            echo "    the committer, so the author must add their own."
        fi
        fail=1
    fi
done

if [[ "$fail" -ne 0 ]]; then
    echo "FAIL: $RANGE has commits without a DCO sign-off (see above)"
    echo "    Fix the most recent one with:  git commit --amend --signoff"
    echo "    Fix a whole branch with:       git rebase --signoff <base>"
    exit 1
fi
echo "PASS: all $checked non-merge commits in $RANGE are signed off"
