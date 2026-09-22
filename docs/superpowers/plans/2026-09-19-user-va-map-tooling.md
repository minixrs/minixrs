# User VA Map — the tooling half (hand-off to `~/src/tooling`)

> **This plan is executed in a different repository, in a different session.** Nothing here is
> edited or applied from the minixrs session that wrote it — the cross-repo rule forbids it, and the
> tooling repo signs its own commits. This file is the specification that session works from.

**Companion plan:** [`2026-09-19-user-va-map.md`](./2026-09-19-user-va-map.md) (the minixrs half,
tasks 1–12).\
**Spec:** [`../specs/2026-09-19-user-va-map-design.md`](../specs/2026-09-19-user-va-map-design.md) —
§5 carries the change table, `V11` the reasoning for dropping the pin, `V12` the ordering.\
**Tracker:** [`../../plans/phase-6-prep.md`](../../plans/phase-6-prep.md), chunk 2 — its box is
checked for the OS-side work; this plan is what the "tooling hand-off — outstanding" line points at.

## Ordering — minixrs lands FIRST

```
1. minixrs PR lands             — the stack leaves 0x0020_0000; both image bases become safe
2. tooling PR lands + SDK rebuild — the --image-base pin is dropped; SDK images move to 0x0020_0000
3. minixrs three-boot matrix re-run against the rebuilt SDK
```

**This ordering is load-bearing, not tidiness.** LLVM patch 0006 pins `--image-base=0x100000` for
exactly one reason: lld's aarch64 default is `0x200000`, which is where minixrs used to map the
initial stack page. Drop the pin before step 1 and every SDK-built image links straight onto the
stack page — `load_exec_image` maps the `PT_LOAD`s first and the stack second, so the stack's
`map_page` fails `AlreadyMapped`, the load unwinds, and the program answers `ENOEXEC`/`ENOMEM`
rather than misbehaving visibly. A silent non-boot is the failure mode, which is why the order is
written down rather than assumed.

The minixrs change is safe in isolation in the other direction: moving the stack to `0x3FFF_0000`
makes `0x200000` an ordinary image address, so the SDK flavour keeps linking at `0x100000` until
step 2 and passes either way. `$MINIXRS_SDK` is never written to by minixrs (`build-musl.sh` still
does its own `rm -rf $SDK/sysroot`).

## The new VA map, as the tooling repo must mirror it

Defined in `kernel-shared/src/uspace.rs` (minixrs); duplicated in `check-image.sh` by necessity,
since that script runs on a host with no minixrs checkout.

| Constant                 | Value         | Meaning                                           |
| ------------------------ | ------------- | ------------------------------------------------- |
| `USER_STACK_TOP`         | `0x4000_0000` | `== USER_DEVICE_WINDOW_BASE`; stack grows down    |
| `USER_STACK_BYTES`       | `0x1_0000`    | 64 KiB = 16 pages                                 |
| `USER_STACK_BASE`        | `0x3FFF_0000` | lowest mapped stack page                          |
| `USER_STACK_GUARD_BYTES` | `0x1000`      | one unmapped guard page below the stack           |
| `USER_REGION_LIMIT`      | `0x3FFE_F000` | the guard page's base; the real ceiling for a map |

`SERVER_STACK_VA` and `SERVER_STACK_BYTES` no longer exist under those names — the stack constants
moved to `kernel-shared::uspace` so tooling and kernel can share one definition. Cite the new file
in the mirrored comments, not `kernel/src/arch/aarch64/userland.rs`.

## The edits

### 1. `verify/check-image.sh` — the mirrored constants (currently lines 59–61)

Today:

```bash
STACK_VA=$((0x200000))              # kernel/src/arch/aarch64/userland.rs SERVER_STACK_VA
STACK_PAGES=1                       # userland.rs maps exactly one page there
DEVICE_WINDOW_BASE=$((0x40000000))  # kernel-shared/src/uspace.rs USER_DEVICE_WINDOW_BASE
```

Becomes:

```bash
USER_STACK_BASE=$((0x3FFF0000))     # kernel-shared/src/uspace.rs USER_STACK_BASE
STACK_PAGES=16                      # USER_STACK_BYTES / USER_PAGE_SIZE
DEVICE_WINDOW_BASE=$((0x40000000))  # kernel-shared/src/uspace.rs USER_DEVICE_WINDOW_BASE
```

Keep the existing "no `_` digit separators" note above the block — bash arithmetic parses
`0x3FFF_0000` as a variable reference, so the separators the Rust sources carry must not be copied
across.

`DEVICE_WINDOW_BASE` stays defined even though the check below folds into the stack rule: it is
still the thing `USER_STACK_TOP` is equal to, and keeping the name makes the equality visible.

### 2. `verify/check-image.sh` — the overlap rule (currently lines 179–184)

Today two separate checks, in this order:

```bash
if (( pvaddr < STACK_VA + STACK_PAGES * PAGE_SIZE && vend > STACK_VA )); then
    bad "PT_LOAD #$i [...] overlaps the stack page at $(hx "$STACK_VA") (SERVER_STACK_VA)"
fi
if (( vend > DEVICE_WINDOW_BASE )); then
    bad "PT_LOAD #$i [...] reaches the device window at $(hx "$DEVICE_WINDOW_BASE")"
fi
```

The first is an interval-overlap test because the old stack was a single page *inside* the usable
range, with valid VA on both sides. That is no longer true: the stack now runs to the top of usable
process VA, so everything from `USER_STACK_BASE` upward is off-limits and the test collapses to a
one-sided comparison. The device-window check is then redundant — `USER_STACK_TOP ==
USER_DEVICE_WINDOW_BASE`, so any `vend` that reaches the window has already passed the stack base.

Becomes one check:

```bash
if (( vend > USER_STACK_BASE )); then
    bad "PT_LOAD #$i [$(hx "$pvaddr"),$(hx "$vend")) reaches the stack at $(hx "$USER_STACK_BASE") (USER_STACK_BASE)"
fi
```

Leave the `vend > USER_VA_TOP` check alone; it is a different claim and still reachable in
principle.

**Open question for the implementing session, flagged rather than decided here.** Spec §5 says
`vend > USER_STACK_BASE`, and the table above is written to match it. The *strictly* correct ceiling
is `USER_REGION_LIMIT` (`0x3FFE_F000`): an image whose last page lands on the guard page is not
overlapping the stack, but it defeats the guard, and VM will not place a region there either. The
difference is exactly one page and no real image is anywhere near it. Recommendation: assert `vend >
USER_REGION_LIMIT` and say so in the message, which is a superset of the spec's rule and cannot
reject anything the spec's rule accepts for a good reason. If that is taken, mirror
`USER_REGION_LIMIT` in the constants block too, and derive it (`USER_STACK_BASE - PAGE_SIZE`) rather
than writing a second literal.

### 3. `verify/check-driver.sh:94` — remove the `--image-base` assertion

The block at lines 89–96 (the comment plus the `grep -q -- '--image-base=0x100000'` / `pass` / `bad`
chain) goes away entirely. Its whole subject is the pin, and after edit 4 the driver does not emit
the flag, so the assertion would fail on a correctly built SDK.

Delete the comment with it — it explains a compensation that no longer has a subject. Do not replace
it with an assertion that the base is *absent*: clang not emitting a flag is not a property worth
pinning, and a user's own `-Wl,--image-base` is still legal.

### 4. LLVM patch 0006 — drop the pin (`V11`)

`patches/llvm/0006-minixrs-Driver-pin-the-image-base-at-0x100000.patch` is dropped, not amended: its
entire content is the pin. That means removing three things that ship together:

- `CmdArgs.push_back("--image-base=0x100000");` and its comment block in
  `clang/lib/Driver/ToolChains/MinixRS.cpp` (`minixrs::Linker::ConstructJob`, after the two `-z`
  flags);
- the `// CHECK-SAME: "--image-base=0x100000"` line in `clang/test/Driver/minixrs.c`;
- the whole `CHECK-BASE-OVERRIDE` run at the end of that test — it exists only to prove the driver's
  own flag is outranked by a user's, and with no driver flag there is nothing to outrank.

Renumbering: the series is `0001`–`0006` and `0006` is last, so dropping it needs no rebase of the
others. Whatever applies the series (`scripts/`, the M-ladder docs) must stop naming it.

After this, SDK-built images link at lld's aarch64 default `0x200000`; repo-built images keep the
explicit `0x100000` in `servers/*/user.ld` and `userland/*/user.ld`. **Two different load bases are
correct, not a discrepancy** — each process has its own `TTBR0`, which is the same reasoning that
already lets every server share one base.

`check-image.sh` keeps asserting what actually matters — segments clear of the stack, the program
headers covered by a `PT_LOAD` (the `AT_PHDR` condition musl's `__init_tls` dereferences), 4 KiB
alignment, `ET_EXEC`, the identity note — and stops asserting any particular base.

### 5. `verify/selftest.sh:142` — the `image-base-1m` fixture inverts

Today the two image fixtures encode "the pin is what makes an image loadable":

```bash
expect_image image-base-1m    0 "LOADABLE"                  branded --image-base=0x100000
expect_image image-default    1 "overlaps the stack page"   branded
```

`image-default` is the one that has to change: linking at lld's default `0x200000` is now perfectly
loadable, so a fixture asserting exit 1 and the string `overlaps the stack page` will fail against
the corrected checker. Both rows become pass rows:

```bash
expect_image image-base-1m    0 "LOADABLE"  branded --image-base=0x100000
expect_image image-base-2m    0 "LOADABLE"  branded --image-base=0x200000
```

Keep a *negative* fixture for the rule — a green checker that can no longer say no is worth nothing.
`expect_image`'s contract already demands the reason string, not just the exit code, so the new row
needs an image that genuinely reaches the stack. Link one at the ceiling:

```bash
expect_image image-on-stack 1 "reaches the stack" branded --image-base=0x3FFF0000
```

(Confirm the reason string matches whatever wording edit 2 lands on, and confirm `link`'s fixture
source produces a `p_memsz` large enough that `vend` crosses the base rather than sitting exactly on
it — a zero-`memsz` segment is skipped by the `npages > 0` guard.)

### 6. Documentation that names the pin

- `docs/sysroot-layout.md:57` states the driver emits `--image-base=0x100000` and why. Rewrite it
  for the new map: the driver emits no base, lld's default `0x200000` is used, and the reason the
  old text gave (the stack page at `0x200000`) is historical.
- `docs/plans/llvm-m2.md` and `docs/plans/musl-m3.md` describe the pin at length. **Those are
  shipped plan history — do not rewrite them.** If the repo's convention allows it, add a one-line
  forward pointer to this change; otherwise leave them.

## Verification (in the tooling repo, after step 1 of the ordering)

1. `verify/selftest.sh` — all fixtures pass, including the new negative one. This is the fixture
   re-mutation-test spec §6 asks for: comment out the overlap `bad` line and confirm
   `image-on-stack` flips to FAIL for the reason under test, not merely on exit code.
2. Rebuild the SDK (clang without patch 0006, then the musl sysroot).
3. `verify/check-driver.sh` — green, with no `--image-base` row.
4. `verify/check-image.sh` over every image the SDK and the minixrs tree build — the SDK ones now at
   `0x200000`, the repo ones still at `0x100000`, all green.
5. Hand back to minixrs for step 3 of the ordering: the mandatory three-boot matrix (SDK flavour,
   forced in-tree musl, moved-aside sysroot with `MINIXRS_SDK=/nonexistent` on *both* of the last
   two rows) against the rebuilt SDK.

## What this plan does not change

- `check-brand.sh` and the identity note — untouched by the VA map.
- The `-z max-page-size=4096` / `-z separate-loadable-segments` flags — unrelated to the base, and
  still required by the loader.
- Anything under `$MINIXRS_SDK` from the minixrs session. The SDK is rebuilt by the tooling repo's
  own scripts, which is the only thing that writes there.
