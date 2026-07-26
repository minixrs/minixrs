#!/bin/sh
# SPDX-License-Identifier: BSD-3-Clause
# Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
#
# Build the musl fork (external/musl) into a cached sysroot at
# target/musl-sysroot (phase-5 decision D10).
#
# Everything this writes lives under target/. The submodule work tree is left
# PRISTINE -- the c-headers CI job asserts a clean `git status`, and a build
# that dirties a submodule would fail it. musl supports out-of-tree builds, so
# the object tree goes to target/musl-build.
#
# Rebuilds are skipped when target/musl-sysroot/.stamp already matches the
# submodule SHA and the toolchain. Pass --force to rebuild anyway.
#
# Environment:
#   MUSL_SRC   musl source tree (default: external/musl). Point this at a local
#              fork checkout to test a port change before bumping the submodule.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MUSL_SRC=${MUSL_SRC:-$ROOT/external/musl}
SYSROOT=$ROOT/target/musl-sysroot
BUILD=$ROOT/target/musl-build
HDRS=$ROOT/target/gen-c-headers
STAMP=$SYSROOT/.stamp

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

if [ ! -f "$MUSL_SRC/configure" ]; then
	echo "error: no musl source at $MUSL_SRC" >&2
	echo "       run: git submodule update --init --recursive" >&2
	exit 1
fi

# ---------------------------------------------------------------------------
# Toolchain. Both binutils replacements ship with the pinned Rust nightly, so
# there is no Homebrew LLVM dependency.
#
# RANLIB is `llvm-ar s`, not llvm-ranlib: the toolchain ships llvm-ar but NOT
# llvm-ranlib, and `llvm-ar s` writes the archive index, which is all ranlib
# does here.
# ---------------------------------------------------------------------------
SR=$(rustc --print sysroot)
AR=$(echo "$SR"/lib/rustlib/*/bin/llvm-ar)
if [ ! -x "$AR" ]; then
	echo "error: llvm-ar not found under $SR/lib/rustlib/*/bin" >&2
	exit 1
fi
CC="clang --target=aarch64-unknown-linux-musl"

# ---------------------------------------------------------------------------
# Freshness. The stamp keys on the exact musl commit plus the toolchain, so a
# submodule bump or a nightly bump both invalidate it. CI uses the same value
# as its cache key.
# ---------------------------------------------------------------------------
MUSL_SHA=$(git -C "$MUSL_SRC" rev-parse HEAD 2>/dev/null || echo unknown)

# Hash portably. macOS ships `shasum` (perl) but no `sha256sum`; Linux ships
# `sha256sum` (coreutils) and usually but not always `shasum`. `cksum` is in
# POSIX and is the last resort -- it is a weak checksum, but this only has to
# detect a toolchain CHANGE, not resist an adversary.
#
# Failing loudly matters: a hash command that printed nothing would make every
# stamp compare equal, and the sysroot would silently never rebuild.
hash_stdin() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256
	elif command -v openssl >/dev/null 2>&1; then
		openssl dgst -sha256 | sed 's/.*= //'
	else
		cksum
	fi
}

TOOLCHAIN_ID=$(
	{ cat "$ROOT/rust-toolchain.toml" 2>/dev/null || true; clang --version | head -1; } \
		| hash_stdin | cut -d' ' -f1
)
if [ -z "$TOOLCHAIN_ID" ]; then
	echo "error: could not hash the toolchain identity (no sha256sum/shasum/openssl/cksum?)" >&2
	exit 1
fi
WANT="musl=$MUSL_SHA toolchain=$TOOLCHAIN_ID"

if [ "$FORCE" -eq 0 ] && [ -f "$STAMP" ] && [ -f "$SYSROOT/lib/libc.a" ]; then
	if [ "$(cat "$STAMP")" = "$WANT" ]; then
		echo "musl sysroot up to date ($SYSROOT)"
		exit 0
	fi
fi

echo "building musl fork -> $SYSROOT"
echo "  source:    $MUSL_SRC ($MUSL_SHA)"

# ---------------------------------------------------------------------------
# The generated ABI headers come first: src/minixrs/*.c includes <minixrs/ipc.h>
# and friends, and musl compiles its own sources with -nostdinc, so the only way
# in is an explicit -I in CFLAGS.
# ---------------------------------------------------------------------------
( cd "$ROOT" && cargo gen-c-headers >/dev/null )

rm -rf "$BUILD" "$SYSROOT"
mkdir -p "$BUILD"

( cd "$BUILD" && "$MUSL_SRC/configure" \
	--target=aarch64-linux-musl \
	--prefix="$SYSROOT" \
	--disable-shared \
	CC="$CC" \
	AR="$AR" \
	RANLIB="$AR s" \
	CFLAGS="-I$HDRS/include" >/dev/null )

# configure's linker probes all fail under a Darwin-hosted clang (its default
# linker is ld64). Harmless: --disable-shared means only libc.a and the crt
# objects are built, and consumers link with rust-lld.

make -C "$BUILD" -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" >/dev/null
make -C "$BUILD" install >/dev/null

# ---------------------------------------------------------------------------
# Install the generated minix.rs ABI headers alongside musl's own, so a C
# program compiles against one -isystem path. They are copied into the SYSROOT,
# never into the submodule work tree.
# ---------------------------------------------------------------------------
mkdir -p "$SYSROOT/include/minixrs"
cp "$HDRS"/include/minixrs/*.h "$SYSROOT/include/minixrs/"

# ---------------------------------------------------------------------------
# The D7 errno check, for real.
#
# Slice 5.0 deferred this to 5.6 because CI had no musl sysroot: until now the
# opt-in POSIX block was only ever compiled against a generated stand-in, which
# proves macro spellings but not values. Here it compiles against the FORK'S OWN
# bits/errno.h in the installed sysroot, so a POSIX errno whose value drifted
# from musl's is a build failure.
#
# -nostdinc + -isystem "$SYSROOT/include" is what makes it honest: the host libc
# cannot creep in (Darwin EDEADLK is 11, not 35).
# ---------------------------------------------------------------------------
echo "  checking:  POSIX errno values against the fork's bits/errno.h"
clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only \
	--target=aarch64-unknown-linux-musl \
	-nostdinc -isystem "$SYSROOT/include" \
	-DMINIXRS_ABI_CHECK_POSIX_ERRNO \
	"$HDRS/abi-selftest.c"

printf '%s' "$WANT" > "$STAMP"
echo "musl sysroot ready ($SYSROOT)"
