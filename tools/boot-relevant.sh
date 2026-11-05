#!/usr/bin/env bash
#
# tools/boot-relevant.sh -- decide whether a change can affect the QEMU boot,
# i.e. whether CI's `qemu-smoke` job has anything to prove.
#
#   usage: tools/boot-relevant.sh <range>      # e.g. <base-sha>...<head-sha>
#          tools/boot-relevant.sh --self-test  # prove the classifier both ways
#
# Prints `true` (boot it) or `false` (docs-only, skip it) on stdout. CI feeds
# the PR's three-dot range, so the diff is measured from the merge base and a
# `main` that moved on since the branch was cut does not leak into it.
#
# The list below names what is SAFE TO SKIP; everything else boots. That is
# the fail-safe direction: a path nobody thought about costs one boot rather
# than slipping through untested. Keep it to files the kernel build provably
# never reads -- docs, mdBook sources, formatter config, tool settings.
#
# Renames are split (`--no-renames`) so a file moved from kernel/ into docs/
# shows its old path too, and still boots. An empty change set also boots: the
# only way to get one here is a range computed wrong, and a gate that skips on
# a broken input is worse than one that spends a boot.
#
# Kept to bash 3.2 so it runs on a stock macOS /bin/bash before you push, the
# same rule as tools/check-dco.sh.

set -euo pipefail

# 0 when the path is docs-only, 1 when it may affect the boot.
#
# `case` globs let `*` match `/`, so `*.md` covers Markdown at any depth and
# `book/*` everything under book/.
docs_only() {
    case "$1" in
        *.md | book/* | docs/* | .entire/* | .claude/* | dprint.json) return 0 ;;
        LICENSE | LICENSE.* | .github/workflows/docs.yml) return 0 ;;
        *) return 1 ;;
    esac
}

# Reads one path per line on stdin; prints true/false.
classify() {
    local path seen=0
    while IFS= read -r path; do
        [[ -z "$path" ]] && continue
        seen=1
        if ! docs_only "$path"; then
            echo true
            return
        fi
    done
    if [[ $seen -eq 0 ]]; then
        echo true
    else
        echo false
    fi
}

self_test() {
    local failed=0 total=0 want got name paths
    # name | expected | newline-separated paths
    while IFS='|' read -r name want paths; do
        total=$((total + 1))
        got=$(printf '%b\n' "$paths" | classify)
        if [[ "$got" != "$want" ]]; then
            echo "boot-relevant: self-test FAIL: $name (want $want, got $got)" >&2
            failed=$((failed + 1))
        fi
    done <<'EOF'
docs only|false|README.md\nbook/src/SUMMARY.md\ndocs/plan.md
nested markdown|false|servers/vm/NOTES.md
tool settings|false|.entire/settings.json\n.claude/settings.json
formatter, license, docs workflow|false|dprint.json\nLICENSE\n.github/workflows/docs.yml
kernel source|true|kernel/src/main.rs
one code file among docs|true|docs/plan.md\nservers/vm/src/main.rs\nbook/src/SUMMARY.md
CI workflow itself|true|.github/workflows/ci.yml
boot expectations|true|tests/qemu-boot.expected
musl submodule pointer|true|external/musl
toolchain pin|true|rust-toolchain.toml
lockfile|true|Cargo.lock
lookalike outside docs/|true|tools/docs-gen.sh
empty change set|true|
EOF
    echo "boot-relevant: self-test $((total - failed))/$total cases passed" >&2
    [[ $failed -eq 0 ]]
}

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <range> | --self-test" >&2
    exit 64
fi

if [[ "$1" == "--self-test" ]]; then
    self_test
    exit
fi

git diff --no-renames --name-only "$1" | classify
