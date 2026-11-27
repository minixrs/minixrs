# CI gates

What runs on every PR, which gates block, and what to run locally before pushing. See
[`build-and-boot.md`](./build-and-boot.md) for the commands themselves.

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. It has thirteen jobs: eleven
gates, listed here, plus two that are not gates of their own — `boot-scope`, the docs-only
classifier `qemu-smoke` waits on (below), and `sonar`. The eleven gates run in parallel — `fmt`,
`clippy`, `clippy-kernel` (aarch64 kernel lint + compile), `c-headers` (regenerate + `clang
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
  branch protection requires — rename it only together with the protection rule.
- **dprint is installed by checksum-verified download, never `curl … | sh`.** The workflow pins the
  CLI version *and* the SHA-256 of the release asset, alongside the plugin checksum already pinned
  in `dprint.json` — the plugin checksum does not constrain how a floating CLI resolves config.
  `DPRINT_SHA256` was computed from the downloaded bytes, not copied from the publisher's
  `SHASUMS256.txt`: taking the digest from the same server as the artifact verifies nothing. Bump
  the two together, and keep `--proto '=https' --proto-redir '=https'` on the `curl` — a release
  asset redirects to an object store, and only `--proto-redir` constrains the redirect
- **The link checker self-tests before it runs.** `tools/check-md-links.py --self-test` asserts the
  check *fails* on each breakage it exists to catch — missing file, missing anchor,
  underscore-bearing anchors, a link reflowed across two lines, headings inside fences. A gate
  nobody has watched fail reads as coverage without being any, so the assertion is that it breaks,
  not that it passes
- Only `geiger` and `miri` are **advisory** (`continue-on-error`); the other nine block. miri only
  covers the host-testable crates — the `-p` list in the `miri` job of `.github/workflows/ci.yml` is
  the authority, and `minixrs-driver-rt` joined it in 6.2 because its MMIO and shared-memory code
  (`mmio.rs`, and `dma.rs` since 6.3, driven by the virtqueue's tests) are the `unsafe` Miri is for
  — while `minixrs-ipc` stays out, having inline asm. Since slice 6.6 `driver-rt` also holds the
  CDEV validators and read plan that `minixrs-tty` used to own, so those tests run under Miri there;
  `minixrs-tty` stays on the list for its ring and its line's phases. `minixrs-virtio-console` is
  not on it, like `minixrs-virtio-blk`: the tested half of each is safe code. `geiger`'s per-package
  sweep filters out `minixrs-kernel` (it can't host-build)
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
  than a fallback, since `tests/qemu-boot.expected` requires the C programs' markers.
  `clippy-kernel` does not, so it builds **`worker`**. No job builds the SDK flavor at all — see
  [The SDK flavor has zero CI coverage](#the-sdk-flavor-has-zero-ci-coverage) below for what that
  costs and the local mitigation
- **`qemu-smoke` skips on docs-only PRs.** A `boot-scope` job runs `tools/boot-relevant.sh` on the
  PR's three-dot range and the boot runs only if something outside the skip list changed — the
  script names what is *safe to skip* (`*.md`, `book/`, `docs/`, `.entire/`, `.claude/`,
  `dprint.json`, `LICENSE`, `docs.yml`) and everything else boots, renames split so a file moved
  into `docs/` still counts its old path. Pushes to `main` always boot. Two traps the design avoids,
  so keep them avoided when editing: a workflow-level `paths-ignore` would leave the *required*
  "QEMU boot smoke" check never reporting, so docs PRs would hang pending — a job skipped by its
  `if:` reports success instead; and since a skipped required check is a pass, `qemu-smoke` boots
  whenever `boot-scope` did not *succeed*, so a broken classifier costs a boot rather than waving
  every PR through. Add a path to the skip list only if the kernel build provably never reads it,
  and add a self-test case with it
- `qemu-smoke` runs on the free `ubuntu-24.04-arm` runner and boots the kernel **three times**
  through the cargo runner, each boot asserting exit 124, the timeout status a healthy run must
  produce:

  | Boot    | Command prefix                                         | Timeout | Fed | Log                |
  | ------- | ------------------------------------------------------ | ------- | --- | ------------------ |
  | disk    | `MINIXRS_ROOT=disk`                                    | 1200 s  | yes | `qemu-disk.log`    |
  | reboot  | `MINIXRS_ROOT=disk MINIXRS_VBLK_IMAGE=target/disk.img` | 180 s   | no  | `qemu-reboot.log`  |
  | ramdisk | `MINIXRS_ROOT=ramdisk`                                 | 1200 s  | yes | `qemu-ramdisk.log` |

  The reboot attaches the image the disk boot just wrote, as-is, which is why the steps are in this
  order: the reboot-ordering rule, and the once-per-disk-boot rule beside it, are in
  [`build-and-boot.md`](./build-and-boot.md) under "THE REBOOT", with what each boot attaches.
  `tools/check-boot-log.sh <disk|reboot|ramdisk> <log>` then greps each log — the two full boots
  against the common marker files plus their backend's own pair, the reboot against its own pair
  only — and all three are checked even if an earlier one fails, so one run shows the whole story. A
  third step runs `tools/diff-backends.sh --self-test` followed by the comparison of the two
  full-boot logs. All three logs are uploaded in the `qemu-boot-log` artifact, with the screen
  check's files beside them (below). **Blocking** as of phase-5-prep chunk 7. The marker-file
  contract itself — which file a marker belongs in, what the scripts match, and why expectations
  must be first-occurrence-only — lives in
  [`testing-and-markers.md`](./testing-and-markers.md#markers)
- **The two full boots are fed their console input, on both consoles; the reboot is not** (slices
  6.5 and 6.6). Each full boot's stdin is `< <(FEED_CONSOLE_CEILING=1300 tools/feed-console.sh <its
  own log>)` — the form, the feeder, the second console's FIFO pair and the measurements behind them
  are under [Fed boots](./build-and-boot.md#fed-boots), and which marker lines depend on it under
  [Markers](./testing-and-markers.md#markers). The invocation did not change for the second console:
  the same script feeds `/dev/hvc0` through `target/hvc0.{in,out}` first and the serial console
  after. **Both full boots use the default FIFO pair, which is safe only because they run one after
  the other** — a job that ever ran two boots at once would have to give each its own
  `MINIXRS_HVC0`. Two more things are CI's own. The ceiling is **1300 s** against the boot's 1200 s
  timeout, and the only thing it has to do is exceed that timeout: the build runs *inside* `timeout
  1200 cargo run`, so nothing is in front of the boot for the ceiling to allow for, and the feeder's
  clock starts with the boot's. The margin is so that the feeder never gives up on a boot that is
  still running; the cap is so that a boot that never parks a read does not leave a poller behind
  for long. Whoever changes a full boot's timeout changes its ceiling in the same edit. And the
  redirection reads and writes one log file on one command line, which `actionlint`'s shellcheck
  reports as an informational SC2094 — that is the design, not a finding. **None of this has run on
  CI**: the feeder was exercised only on a dev machine (macOS, QEMU 11, Homebrew's edk2), so whether
  the runner's bash, QEMU and firmware carry the bytes the same way — deliver each probe's two
  chunks as two interrupts, and behave as macOS does with a `pipe` chardev over FIFOs nobody else
  holds — is unmeasured until the branch's first CI run. `cons.other ok`, `hvc0 ok` and `stdin ok`
  all missing there with `cons.beside ok` present is the feeder path or the FIFOs, not a driver,
  until shown otherwise: the feeder's stderr says which stage it reached
- **Both full boots are screen-checked, and the check blocks** (slice 6.7). It is steps inside
  `qemu-smoke`, not a job, so the gate count does not move. Before each full boot the boot step
  starts `MINIXRS_FBCHECK=target/release/minixrs-fbcheck tools/screendump.sh qemu-<boot>.log
  fb-<boot>.ppm > fb-<boot>.verdict 2>&1 &` in the background, runs the boot as before, and then
  settles the script with a small shell function, `finish_screen`: a script that has exited leaves
  its exit status in `fb-<boot>.rc`; **one still running when the boot's `timeout` has fired is
  killed and counted as failed** (`1` in the `.rc`, and a line saying so appended to the
  `.verdict`), because the script cannot see QEMU die and one that is still looking never saw its
  gate line, never saw the screen scroll, or never got a scrolled dump to pass. **A pass in CI is a
  pass of a scrolled screen**: the script asks `fbcheck` for one and keeps looking, every 10 s,
  through any number of `early` verdicts — the first cut took the first dump that passed, which in
  the default boot is an unscrolled one, and let a TTY whose scroll painted nothing through. Nothing
  fails there — all three boots still run. The check step then prints each `fb-<boot>.verdict` and
  fails, with an `::error::` naming the `.ppm`, unless the `.rc` holds `0`; it does so after the
  three `check-boot-log.sh` calls and whatever they said, so one run still shows the whole story.
  **The reboot is not screen-checked.** What the check proves, what it forgives and how to read its
  verdict line are under [The screen check](./testing-and-markers.md#the-screen-check)
- **The artifact carries the dumps.** `qemu-boot-log` uploads `fb-*.ppm` and `fb-*.verdict` with the
  three logs, on every run (`if: always()`), pass or fail. Each `.ppm` is the **last** dump its
  script took — the passing one on a pass, the last one looked at otherwise — and each `.verdict`
  holds the script's whole stderr: an `early … (not scrolled yet)` line per change in the number of
  rows on the screen, every failed look's reason, and the final `ok … scrolled=yes`. `early` lines
  in a passing verdict are normal. `fb-*.rc` is the step's own bookkeeping and is not uploaded. A
  boot whose script never reached a dump (no gate line, no socket) has a `.verdict` and no `.ppm`
- **`qemu-smoke` builds `tools/fbcheck` in a step of its own before the first boot**, for
  `mkimage`'s reason below and one more: handed to the script through `MINIXRS_FBCHECK`, the
  pre-built binary means **no cargo runs beside a boot at all**. Left to build it itself the script
  would start a `cargo build` at the same moment as the boot's `cargo run`, the two would contend
  for `target/`'s lock, and whichever the boot lost would come out of its timeout
- **The screen check uses the default QMP socket, `target/qmp.sock`, for both boots** — safe for the
  reason the default FIFO pair is: the boots run one after the other. A job that ran two at once
  would give each its own `MINIXRS_QMP` as well as its own `MINIXRS_HVC0`. Like the feeder, **none
  of this has run on CI**: `ramfb` under the runner's QEMU and `qemu-efi-aarch64` firmware — whether
  Limine reports a framebuffer there at all, in what mode, and whether `screendump` over a unix QMP
  socket behaves as on macOS — is unmeasured until the branch's first CI run. A different default
  mode moves three expected markers together ([Markers](./testing-and-markers.md#markers)); `[fb]
  none` on both boots is the firmware or the device, not the driver
- **The boots run in sequence in one job, not as a matrix.** A matrix would rename the required
  "QEMU boot smoke" check, which needs a branch-protection edit, and two QEMUs on one runner would
  starve each other. The price is the job's `timeout-minutes`, **90** since slice 6.4 (50 before
  6.3, 65 after it): every boot burns its full timeout, so the boots cost 1200 + 180 + 1200 s = 43
  minutes, and 90 keeps the 45 minutes of build headroom the 65 had. Whoever changes any of the
  three timeouts changes `timeout-minutes` in the same edit, by the sum of the differences
- **`qemu-smoke` builds `tools/mkimage` in a step of its own before the first boot.** The runner
  builds the disk image through a nested `cargo run --release -p minixrs-mkimage`, which happens
  *inside* the boot's `timeout`; on a cold cache that compile would be charged to the first boot and
  skew the fraction the budget is derived from. Pre-built, the nested cargo only runs it
- **`fbcheck`'s and the TTY renderer's host tests run in `coverage`**, like `mkimage`'s, since
  `coverage` runs every host test in the workspace. `minixrs-tty` was already on the advisory `miri`
  job's `-p` list, so the renderer's tests now run under Miri as well, although the library half is
  `#![forbid(unsafe_code)]` and gives Miri nothing new to find; several of them paint whole 800×600
  images, and **how long that takes under Miri is unmeasured** — the job is advisory, so a slow run
  cannot fail a PR, but look at its duration on the branch's first CI run. `minixrs-fbcheck` is not
  on the list: it has no `unsafe` either, and its tests open unix sockets
- **`mkimage`'s host tests run in `coverage`, and nowhere else.** There is no `cargo test` job:
  `coverage` runs `cargo llvm-cov --workspace --exclude minixrs-kernel`, which executes every host
  test in the workspace, the image writer's included. `minixrs-mkimage` is deliberately not in the
  `miri` list — it has no `unsafe` for Miri to check, and its tests build whole 37 MiB images.
  `minixrs-driver-rt` is in the advisory `miri` job's `-p` list, so the GPT *parser* is among what
  that job is configured to check. As of slice 6.4 that is a configuration and a local result, not a
  CI one: `cargo miri test -p minixrs-driver-rt` was run locally on the parser's commit and again on
  the slice's final tree (2026-11-22: 113 passed, 0 failed), and the branch had not run on CI
- **Two things about the 6.4 boot are not validated until a CI run says so**, because both were
  measured only against Homebrew's edk2 under QEMU 11.0.3: that Debian's `QEMU_EFI.fd` boots from a
  modern (version 2) virtio-mmio disk and reads the FAT32 and GPT `tools/mkimage` writes, and that
  1200 s / 180 s clear the runner's real TCG slowdown. If the firmware cannot, the disk boot's log
  holds no kernel banner at all and the checker reports every marker missing. The job also still
  installs `ipxe-qemu`, and why QEMU on the runner wants its `efi-virtio.rom` has not been
  established there: the workflow's comment used to attribute it to the virtio-blk-PCI ESP drive,
  which 6.4 removed, and that looks like a mis-attribution — on a dev machine's QEMU 11.0.3 `info
  qtree` names `efi-virtio.rom` as the `romfile` of `virtio-net-pci`, the NIC `-M virt` creates by
  default (the runner passes neither `-nodefaults` nor `-nic none`), and gives `virtio-blk-pci`
  none. If the runner's QEMU agrees, the need did not go away with the ESP drive. It is untested
  without the package, so it stays until a run without it is made on purpose
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
  and the budget went to 600 s. 6.3 is the first measurement that did **not** raise it, and the
  first taken per backend: at a fixed 300 s the last required marker sits at **66.8%** of the disk
  boot's log and **48.1%** of the ramdisk boot's, against 48.1% and 55.7% for two boots of the merge
  base (the spread between two boots of one tree is that wide, so compare against both). The disk
  boot is the slower one because every block request is completed by a real interrupt instead of a
  memcpy; ~200 s local under a 600 s budget is a 3.0x factor, so the per-boot budget stayed and only
  `timeout-minutes` grew. Measure **both** backends from now on and budget for the slower. 6.4
  raised it again, **600 → 1200 s** for each full boot: the machine boots from one GPT disk, every
  write to it is followed by a flush (a second interrupt round trip), and the ramdisk boot is no
  longer device-free — it brings the rootless disk up and falls back. The measurement is two
  musl-flavour disk boots of the final code (2026-11-22, same dev machine, QEMU 11.0.3,
  `MINIXRS_SDK=/nonexistent` on the build and on the boot, `len=234280` in each log's `[ksys
  SYS_EXEC] … name=hello` line), compared in *seconds* because the logs differ in length — a
  fraction of a 450 s log is not comparable with a fraction of a 600 s one. The last required marker
  (`[diag vfs] pm-exit reset closed=1`) sits at 88.03% of a 450 s log, **~396 s**, and at 65.57% of
  a 600 s log (byte 41 876 691 of 63 868 433), **~393 s**; the host's load average fell from 11.6 to
  2.7 during the second boot and the two still agree within 3 s. The ramdisk boot was read once:
  57.87% of a 450 s log, **~260 s**. Under 600 s that is 1.5x for the disk boot, so the two full
  boots went to 1200 s — **3.03x** for the disk boot (1200 / ~396), the factor that let 6.3 keep its
  budget, and **4.6x** for the ramdisk boot. Each full boot burns its whole timeout, so the raise
  costs every booting CI run 20 minutes over 6.3; if the runner turns out faster than feared, the
  remedy is to lower the two numbers again. **The slice first set this budget from the wrong
  flavour**, by the trap the end of this bullet describes: every boot before its review round ran
  with the SDK installed and `MINIXRS_SDK` unset, so its `hello` was the SDK's 46 696 bytes
  (`len=46696`), not the 234 280-byte in-tree-musl one CI builds. Those SDK-flavour boots put the
  last required marker at 82.0–82.7% of a 300 s disk log across three boots of the final code
  (82.00%, 82.71%, 82.68%: ~246–248 s) and 59.3% of the ramdisk log (~178 s), with a spread across
  the slice's tasks of 68.4%–82.7% and 47.7%–59.3%; 900 s was chosen from them, read as 3.63x.
  Against the musl readings 900 s is 2.27x — under the ~2.5x at which every earlier slice raised —
  so it was corrected to 1200 s before the branch ever ran on CI. Up to `fs.leak` the two flavours'
  logs agree to within a few seconds; the whole difference is the C programs' phase, where `hello`
  and `fileio` are read through MFS and VFS a round at a time. The SDK figures remain true of an
  SDK-flavour boot and are fine for knowing how long an iteration boot takes; they are not a budget
  measurement. **Take the budget measurement in the musl flavour** — `MINIXRS_SDK=/nonexistent` on
  the build *and* on the timed boot — and confirm it from the log's `len=` before using the number.
  6.3's 66.8% / 48.1% predate this discovery and their flavour could not be re-checked, so do not
  compare a new reading against them without re-taking the merge base. **Still unmeasured: the
  runner's own TCG factor.** Nothing of 6.4 had run on CI when 1200 s was chosen; the branch's first
  CI run is that measurement (what remains of 6.4's budget debt in
  [`phase-6-virtio.md`](../plans/phase-6-virtio.md)). The reboot's 180 s comes from a different
  number: it needs one line, `persist gen=1`, which lands about 34 s of local wall clock after
  `cargo run` starts. CI's TCG is slower than local, so raise with real headroom rather than
  trimming to what passes here — think in the *ratio*, not the wall-clock seconds, which are a
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

**This is a deliberate, tracked decision, not an oversight.** Pre-Phase-6 chunk 6 kept the gap and
deferred an SDK CI job until tooling builds the SDK on a Linux host; the reasons, the job's sketched
shape and the revisit trigger are in
[`phase-6-prep.md`](../plans/phase-6-prep.md#chunk-6-sdk-flavor-ci-coverage). Until then the matrix
below is the only thing that exercises the SDK flavour, so skipping it is shipping untested.

The mitigation is local. Run the **three-boot matrix** when touching `build_c_*`:

| Row | How to force it                                                      | Flavour reached       |
| --- | -------------------------------------------------------------------- | --------------------- |
| 1   | `$MINIXRS_SDK` pointing at a real SDK prefix                         | `HelloFlavor::Sdk`    |
| 2   | `MINIXRS_SDK=/nonexistent`, `target/musl-sysroot` present            | `HelloFlavor::Musl`   |
| 3   | `MINIXRS_SDK=/nonexistent` **and** `target/musl-sysroot` moved aside | `HelloFlavor::Worker` |

- Run at least one `clippy-kernel` invocation with `MINIXRS_SDK=/nonexistent`, so the path CI
  compiles is the one you linted.
- **Row 3's sysroot is `target/musl-sysroot`, inside the repo** — not `~/toolchains/minixrs/sysroot`
  or anything else under `$MINIXRS_SDK`. `kernel/build.rs`'s `build_c_musl` looks there and nowhere
  else. This file used to say only "moved-aside sysroot", which reads as the SDK's, and sent the
  user-VA-map slice's dispatcher to the wrong directory — with `MINIXRS_SDK=/nonexistent` the
  selector never consults the SDK prefix at all, so moving *that* aside silently re-runs the musl
  row and tests nothing. Name the path when you record the row.
- **Row 3 needs `MINIXRS_SDK=/nonexistent` too.** On a machine that has a usable SDK the flavor
  selector never reaches the sysroot, so moving it aside alone re-runs the SDK row — 5.10a nearly
  recorded that as a passing fourth row.
- **A correct row 3 FAILS `check-boot-log.sh`, by design.** The fallback packs `worker` under the
  names `hello` and `fileio`, so the C programs' own markers cannot appear. Exactly sixteen expected
  markers go missing and nothing else (on a [fed](./build-and-boot.md#fed-boots) boot — unfed,
  `cons.other ok`, `hvc0 ok` and `stdin ok` make nineteen) — `hello`'s five (`hello: Hello from C!`,
  `hello: stderr works`, `hello: errno ok`, `iov-end`, `hello: iov ok match=1`), `fileio`'s ten
  (`fileio: read`, `lseek ok`, `lseek deny ok`, `readv ok`, `fresh-buf ok`, `seek-hole ok`, `brk
  ok`, `stdio ok`, `mmap ok`, `malloc ok`), and VFS's `[diag vfs] pm-exit reset closed=1`, since
  `worker` leaves no descriptor open for the exit reset to close. The two exec-trace markers still
  match, because `worker` is exec'd under those names. Any *other* missing marker is a real
  regression — and a **full `PASS` on row 3 (every check of the common and backend sets, none
  missing) means the fallback did not engage**, so what you measured was the musl row again. Rows 1
  and 2 must be fully clean.

Identify the flavour from the **build-script warnings**, never from the boot log: the C markers are
byte-identical across the SDK and musl flavours, so the log physically cannot tell them apart.

| Flavour               | Warning on the build                                                                |
| --------------------- | ----------------------------------------------------------------------------------- |
| `HelloFlavor::Sdk`    | `hello, fileio: built with the minix.rs SDK at <prefix> (<stamp>)`                  |
| `HelloFlavor::Musl`   | **none** — silent by design                                                         |
| `HelloFlavor::Worker` | "no C toolchain for hello or fileio; packing worker under both names" (grep for it) |

`Musl` is silent on purpose (`kernel/build.rs:321`): it is what every CI job builds, and warning on
the norm is how people learn to ignore build-script warnings, which would cost us the two that mean
something. **An empty flavour line is the musl result, not a failed grep** — do not go hunting for a
warning that was never meant to exist.

Treat an image-base or stack move as a **mandatory** matrix run. `$MINIXRS_SDK` does not persist
across separate shell invocations, which is how a matrix row silently measures the wrong flavour;
the table above is how you confirm the flavour you actually built, and the boot-timing bullet above
states the same trap for timing runs — which slice 6.4 then fell into for every budget reading it
took: an installed SDK is found without `MINIXRS_SDK` being set at all, so "I did not set it" is not
"musl". Read `len=` off the log's `[ksys SYS_EXEC] … name=hello` line.
