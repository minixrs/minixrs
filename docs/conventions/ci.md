# CI gates

What runs on every PR, which gates block, and what to run locally before pushing. See
[`build-and-boot.md`](./build-and-boot.md) for the commands themselves.

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. Eleven gates run in parallel —
`fmt`, `clippy`, `clippy-kernel` (aarch64 kernel lint + compile), `c-headers` (regenerate + `clang
-fsyntax-only` the generated C ABI headers), `audit` (cargo-audit), `deny` (cargo-deny, config in
`deny.toml`), `dco` (DCO sign-off, PR-only), `geiger`, `miri`, `qemu-smoke` (aarch64 QEMU boot
smoke), `coverage` (cargo-llvm-cov → `lcov.info`) — then a `sonar` job feeds the LCOV report to
SonarQube Cloud (org `minixrs`, project `minixrs_minixrs`, config in `sonar-project.properties`).
The Sonar scan auto-detects PR vs branch: PRs get decoration, `main` pushes refresh the
whole-project picture.

- The `fmt` job is **three** gates, not one: `cargo fmt --all --check`, `dprint check` (markdown
  formatting — see [`docs-and-workflow.md`](./docs-and-workflow.md#markdown-formatting)), and
  `tools/check-md-links.py` (every relative markdown link resolves to a file, and every `#fragment`
  to a heading that really renders to that anchor). All three block. They live in one job because
  none of them needs a build, and the job keeps the CI name `rustfmt` because that is the name
  branch protection requires — rename it only together with the protection rule. dprint's **CLI
  version is pinned** in the workflow as well as the plugin's checksum in `dprint.json`: the plugin
  checksum does not constrain how a floating CLI resolves config
- Only `geiger` and `miri` are **advisory** (`continue-on-error`); the other nine block. miri only
  covers the host-testable crates (`-p minixrs-kernel-shared -p minixrs-vm -p minixrs-pm`) —
  `minixrs-ipc` has inline asm. `geiger`'s per-package sweep filters out `minixrs-kernel` (it can't
  host-build)
- The `clippy` job runs a **second `run:` step**, `cargo clippy -p minixrs-mfs --features server
  --all-targets -- -D warnings`, and it blocks like the first. It exists because `fs/mfs`'s
  `[[bin]]` carries `required-features = ["server"]`, so `main.rs` is **invisible to every other CI
  job** — clippy `--all-targets`, miri and llvm-cov all skip it without that feature. This step is
  the mitigation: without it the only thing in CI that compiles `fs/mfs/src/main.rs` at all is the
  `qemu-smoke` boot, which reports a lint failure as a mysterious build-script panic
- `dco` runs `tools/check-dco.sh <base>..<head>` and is **PR-only** (`if: github.event_name ==
  'pull_request'`): a push to `main` lands a GitHub merge commit, which by design has no sign-off,
  and the authored commits under it were already checked on their own PR. It needs `fetch-depth: 0`
  — the default shallow clone has neither endpoint of the range — and passes the two SHAs through
  `env:` rather than `${{ }}`-interpolating them into the `run:` (the standard expression-injection
  guard, kept even though these fields are hex). An **empty range is a failure, not a pass**: zero
  authored commits means the range was computed wrong, and a gate that greens on a broken range is
  worse than none. The script is bash 3.2-clean (no `mapfile`, no `${var,,}`) so it runs on a stock
  macOS `/bin/bash` before you push, not just on CI's bash 5
- `qemu-smoke` checks out **submodules recursively** and runs `tools/build-musl.sh` before `cargo
  kernel-aarch64` (slice 5.6), with `target/musl-sysroot` cached on the submodule's resolved
  commit + `rust-toolchain.toml`/`build-musl.sh` — keyed on the *commit*, not a tracked file,
  because the port branch is force-pushed on rebase and `external/musl/VERSION` would not move.
  Without the recursive checkout the job silently tests the `worker`-as-`hello` fallback
- **Which `hello` flavor a job builds is decided by whether it builds the musl sysroot**:
  `qemu-smoke` does, so it builds **`musl`** — which is why that flavor is a real dependency rather
  than a fallback, since `tests/qemu-boot.expected` requires the five C markers. `clippy-kernel`
  does not, so it builds **`worker`**. No job builds the SDK flavor at all — see
  [The SDK flavor has zero CI coverage](#the-sdk-flavor-has-zero-ci-coverage) below for what that
  costs and the local mitigation
- `qemu-smoke` runs on the free `ubuntu-24.04-arm` runner: boots the kernel for 600 s wall clock via
  the cargo runner (asserting exit 124, the timeout status a healthy run must produce), then
  `tools/check-boot-log.sh` greps the serial log against the two marker files. **Blocking** as of
  phase-5-prep chunk 7. The marker-file contract itself — what the script matches, and why
  expectations must be first-occurrence-only — lives in
  [`testing-and-markers.md`](./testing-and-markers.md#markers)
- **A slice can break the boot-timing budget, and "it passes locally" is not the check.** The budget
  was 45 s until slice 5.9, 120 s until 5.10a, and 240 s until 5.10b; **every raise has had the same
  cause and the same evidence**, and the number will move again. 5.9: `hello` stopped being a memcpy
  out of the boot archive and became ~200 KB read off the ramdisk through MFS and VFS, one
  `FS_MAX_IO` round at a time. 5.10a: init writes 32 KiB to `/etc/scratch` and reads every byte
  back, ~130 more device round trips plus the zone-allocation and inode write-back traffic under
  them. 5.10b: init's leak battery issues 256 doomed writes before one good one, each a grant plus a
  BDEV round trip. Measure a marker's *position as a fraction of a fixed-timeout log* (`grep -abo`
  the marker, divide by `wc -c`) on the **musl** flavour — the one CI builds — and compare it
  against the same number at the merge base. 5.9 moved the last C marker from ~27% to ~71% of a 45 s
  run; 5.10a moved it from **27.98% to 56.20% of a 120 s run**, i.e. the boot roughly *doubled*,
  which cut the safety factor over local from 3.6x to 1.8x and is why the budget went to 240 s;
  5.10b moved it again, **26.90% to 61.61% of a 240 s run**, a 2.29x jump leaving a 1.62x factor,
  and the budget went to 600 s. CI's TCG is slower than local, so raise with real headroom rather
  than trimming to what passes here — think in the *ratio*, not the wall-clock seconds, which are a
  property of the dev machine. Two mechanical notes: `git stash` does **not** give you the "before"
  once the slice is committed on a branch — detach to the merge base, and stash only the doc edits
  so `target/` and `target/musl-sysroot` survive and the two boots differ in nothing but the code.
  And build (`cargo build`) before the timed `cargo run`, or the rebuild lands inside the timeout
  and skews the fraction. A third, which has already cost one silently-wrong measurement:
  **`$MINIXRS_SDK` does not persist across separate shell invocations**, so setting it in one
  command and booting in another measures the **SDK** flavour, not the musl one the number is
  defined against. Confirm the flavour from the embedded `hello` size — ~200 KB musl, ~47 KB SDK,
  ~15 KB fallback — rather than trusting the build warning. A boot-time selftest that reads a
  **configuration-dependent** file is the usual culprit: prefer `/etc/pattern` (40 KiB in every
  config, so its count is assertable too) over `/bin/hello`, which is ~46 KB with the SDK, ~200 KB
  with in-tree musl, and ~15 KB in the sysroot-absent fallback
- Before pushing, the blocking gates must be green: `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings` (locally this lints the kernel too), `cargo clippy -p
  minixrs-mfs --features server --all-targets -- -D warnings` (nothing else lints MFS's `[[bin]]`,
  whose `required-features = ["server"]` hides it from every other job), and `cargo clippy -p
  minixrs-kernel --target aarch64-unknown-none -- -D warnings` plus the same with
  `--no-default-features`. Run `cargo fmt --all` to fix formatting
- CI's `clippy` and `coverage` still pass `--exclude minixrs-kernel`, but for **runner cost, not
  correctness**: `forced-target` means they *could* build the kernel, at the price of clang
  cross-assembling the `.S` files plus 8 nested server builds on an x86 blocking gate. So a local
  `cargo clippy --workspace --all-targets` does lint kernel code, while CI delegates that to the
  **blocking** `clippy-kernel` job (`ubuntu-24.04-arm`), which runs twice — default features and
  `--no-default-features` (the stub-free config, whose two `#[allow]`s live inside `cfg(feature =
  "boot-stubs")` code). No `--all-targets` there: the kernel is `no_std`/`no_main`, so there is no
  test harness to build. It is **clean as of the chunk-5 bump** (former lints fixed or carrying a
  per-item `#[allow(clippy::…)]` + rationale: the `nomem`-asm pointer in `sched::set_tpidr_to` is a
  value-not-deref false positive, `Proc::EMPTY` must stay `const` for its array-repeat init, the two
  `mem::forget(aspace)` are defensively future-`Drop`-safe, and the many-arg boot helpers mirror the
  `elf.rs` precedent). Keep it clean when touching kernel code — it now blocks
- `cargo fmt --all` still covers the kernel: cargo-fmt enumerates all workspace *members* (not just
  default ones), and rustfmt follows `mod` declarations without evaluating `cfg`. Likewise
  `audit`/`deny` still see the kernel's deps, because it remains a member sharing one `Cargo.lock`
- The toolchain is **pinned to a dated nightly** in `rust-toolchain.toml` (bare `nightly` let new
  lints/fmt rules break CI with no code change); bump it deliberately, not incidentally
- `Cargo.lock` **is committed** (so audit/deny are reproducible) — do not re-add it to `.gitignore`
- Third-party actions are pinned to full commit SHAs with `# vN` comments; keep that when editing
- SonarCloud needs the `SONAR_TOKEN` repo secret and Automatic Analysis disabled (CI-based instead)
- Add every new `userland/**/src/main.rs` to `sonar.coverage.exclusions` — they are freestanding
  entry points with no host-testable logic (slice 4.7)
- **Publishing:** see `RELEASING.md` — `release.yml` publishes the five library crates to crates.io
  on a `v*` tag push, bottom-up dependency order mandatory; verify locally with the five-crate
  `cargo package -p …` command documented there

## The SDK flavor has zero CI coverage

No CI job installs the minix.rs SDK (an LLVM build is hours), so `$MINIXRS_SDK` is never set on a
runner and the SDK `hello` flavor is never exercised — a regression in the patched clang driver
ships green.

The mitigation is local, and has three parts:

- Run the **three-boot matrix** (SDK, forced musl, moved-aside sysroot) when touching
  `build_hello*`.
- Run at least one `clippy-kernel` invocation with `MINIXRS_SDK=/nonexistent`, so the path CI
  compiles is the one you linted.
- **The moved-aside-sysroot row needs `MINIXRS_SDK=/nonexistent` as well.** On a machine that has a
  usable SDK the flavor selector never reaches the sysroot, so moving it aside alone re-runs the SDK
  row and tests nothing — 5.10a nearly recorded that as a passing fourth row.

Treat an image-base or stack move as a **mandatory** matrix run. `$MINIXRS_SDK` does not persist
across separate shell invocations, which is how a matrix row silently measures the wrong flavour —
the boot-timing bullet above states that trap and how to confirm the flavour you actually built.
