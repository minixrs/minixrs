# Testing and markers

How to verify a server that cannot print, what the two kernel trace samplers do and do not catch,
the boot-marker-file contract, and the mutation-testing discipline. See
[`build-and-boot.md`](./build-and-boot.md) for the boot commands themselves (how to produce and time
a log) — this file covers how to read one.

## Verifying server behaviour

User-space servers run at EL0 with no console access — they cannot print. Verify server behaviour
through kernel-side traces (`[pf]` from `do_page_fault`, `[ksys …]` from `do_vmctl`/`system`, `[ipc
N]` from `ipc::dispatch`), never server-side logging.

Trace sampling is asymmetric between the two: `[ipc N]` head-traces the first ~12 calls *plus* every
100th, but `[ksys N]` samples only every 100th (no head carve-out) — a server's first/rare kernel
call (e.g. a startup `SYS_GETINFO`) shows on `[ipc]`, not `[ksys]`.

Since slice 5.1 a server can also print through `SYS_DIAGCTL` via `server-rt::diag_print`; see
[`kernel.md`](./kernel.md) for the debug channel's kernel half. That is a channel for deliberate
diagnostics, not a replacement for the traces above — nothing routes a server's ordinary execution
through it.

The two `EFAULT` traces — `[efault] proc=… nr=… call=… va=…` and `[efault deliver] proc=… nr=… va=…`
— are **uncounted**, unlike the sampled `[ipc {n}]` form, which is what makes them stable boot
markers instead of ones that can fall out of a fixed-size sample window. The `do_ipc` one keys on
`result == EFAULT`, which is unambiguous because nothing else in the kernel produces that errno.
Keep that invariant in mind before changing `do_ipc`'s tracing — it's the thing that makes the
`[efault]` marker unambiguous.

## Trace forensics

The `[ipc]` modulo sampler almost never catches low-rate callers — a blocking SENDREC client (e.g. a
fork/wait loop) round-trips once per rotation, thousands of times rarer than a synchronous
kernel-call flood from a busier caller — and a server packed late in the MXBI archive boots after
that flood starts, so its SEF handshake never lands in `TRACE_HEAD` either.

**Zero sampled lines for such a caller is NOT evidence it's stuck.** Verify via its downstream
head-carved `[ksys …]` traces instead (e.g. `[ksys SYS_FORK]`/`SYS_EXIT` are head-carved at 6, so
raise the head const temporarily to count real cycles), or add a temporary unconditional `[DBG]`
trace in `ipc::do_ipc` keyed on the caller nr — remove it before committing.

## Exit-status probes

A process that cannot print at all — not even through a driver, e.g. `worker`'s exec-frame validator
running before a console exists — reports its verdict through its own exit status. **Encode the
pass, never the absence of a failure.** `execstack::EXEC_STACK_PROBE_PASS` (`0x5A`) is the positive
sentinel a passing probe returns; status `0` prints `FAIL no-verdict` rather than being read as
success, because two other paths can produce a `0` that means nothing:

- `mproc::handle_kill_in` zombifies a signalled process without touching `exit_status`, so a probe
  that died before it could report is reaped with status `0`.
- A frame bad enough to fault the probe lands it in VM's SIGSEGV arm, which prints no `!!! EL0 data
  abort` for the forbidden-marker list to catch — a silent, verdict-free death.

Companion rule: key any reap-derived marker on the child's **pid**, not on "the first reap" —
`alloc_pid` never returns `0`, so `0` is the correct "not yet" sentinel, and pid-keying is what
stops an unrelated zombie (reaped first, by the same parent) from being mistaken for the probe's own
child.

## Markers

`tools/check-boot-log.sh <log>` greps a captured boot log against `tests/qemu-boot.expected` /
`tests/qemu-boot.forbidden` (see [`build-and-boot.md`](./build-and-boot.md) for how to produce the
log and how long to budget). Update those marker files in the same PR when trace formats or the boot
roster change, or the `qemu-smoke` CI job goes red.

A few properties of the script matter when you're reading its output rather than just running it:

- **The marker files test first occurrences only**, never counts — keep expectations timing-robust,
  since CI's TCG is slower than local and a marker that only ever appears once is the only kind that
  survives that difference.
- **It works on a partial log.** Once it reports PASS on a still-growing log, the verdict is final
  and the boot can be stopped — the 5.10b review round got all 97 markers well inside the 600 s
  budget. Only `qemu-smoke`'s exit-124 assertion needs the timeout to actually elapse. Copy the log
  aside before checking; the live file grows underneath the script.
- **Its PASS total counts expected AND forbidden lines** (5.11: 86 + 20 = 106), so a slice that adds
  N expected and M forbidden lines moves the total by N + M — don't be surprised when the total
  moves by more than the expected-marker count alone.
- **Never copy a count into a marker from a plan** — recompute it from the constant. 5.8's plan said
  `n=30` for what was actually a 31-byte `ROOTFS_MOTD`.

## A starved boot is not a regression

A boot on a loaded host fails markers that are not broken, and the failure mimics a real regression
in an unrelated subsystem. In the user-VA-map slice alone that produced three wrong conclusions —
"the budget must rise to 1800 s", "the filesystem write path hangs", and "the line I just added
broke the boot". Every one of them was host contention.

The discriminator costs one command. The IPC counter is the throughput proxy: QEMU under TCG
advances guest time at whatever share of the host it gets, so a starved run simply does less work in
the same wall-clock budget.

```bash
grep -ao '\[ipc [0-9]*' <log> | tail -1     # final IPC counter, the throughput proxy
uptime; ps -eo pcpu,comm -r | head -5       # what was competing for the machine
```

A clean 300 s run on this branch reaches **~18.8 M**; a passing 1200 s run reached **114 M**.
**Under ~10 M at the timeout means the run was starved, not broken.** Re-run before believing any
marker failure, and never change code on the strength of a starved boot — a "fix" that passes on the
re-run has proved nothing about the code.

The control that settles it in one step: restore the merge base's copy of the file you changed,
rebuild, and boot again. If HEAD fails identically, the host is the cause and the file is innocent.
Restore via the scratchpad-snapshot discipline below, not `git checkout`.

## Mutation testing

The standard (established slices 5.1–5.3): apply a mutation, observe the named marker move, revert.
This runs against an **uncommitted** working tree, so the restore step has real hazards:

- **Never revert a mutation with `git checkout <file>`.** Copy the files to the scratchpad first and
  restore from there, then `diff -q` each back against the snapshot and `grep -rn MUTATION` to prove
  nothing leaked into the PR.
- **The scratchpad snapshot must cover files a slice ADDS, not just the ones it edits.** `git
  checkout -- <untracked file>` does not restore — it *errors* — and behind a `|| true` it silently
  leaves the mutation in the tree (slice 5.9 did exactly this to a new file). Snapshot every file
  you will mutate before the first run, and let the final `grep -rn MUTATION` sweep — never the
  restore command's exit status — be what proves the tree clean.
- **A mutation that fails to compile is indistinguishable from one that worked.** `kernel/build.rs`
  panics on the nested server build, the log holds no kernel output at all, and `check-boot-log.sh`
  reports every marker MISSING. Before recording any observation, run `grep -a 'error\[E' <log>` or
  confirm unrelated markers still PASS — slice 5.4 nearly recorded a false result this way.
- **Iterate and mutation-test in the stub-free config** (`--no-default-features`): 5.10b measured
  the `fs.*` markers landing at ~0.14% of a 60 s stub-free log, so `timeout 30` sufficed for nine
  boots that would each have cost 240 s otherwise. Judge such a run by grepping the specific marker,
  since `check-boot-log.sh` FAILs the stub A–D markers there by design.
- **Not every mutation moves its predicted marker.** Reordering the MXBI packing does not reliably
  break a DS lookup, because publish-before-retrieve is scheduler-dependent and often still resolves
  — to exercise a DS *fallback* branch, remove the peer entirely or break the key (slice 5.7 got
  that observation from the drop-the-server mutation instead, and slice 5.8 reconfirmed both halves:
  a driver swap moved nothing, while breaking a DS key gave a clean fallback FAIL).
- **Some correct invariants have no mutation that moves anything, and the honest record says so
  rather than omitting the row.** 5.10b found two orderings whose violation needs a failure
  *between* two steps, where the only available failure is `EIO` from a fixed RAM ramdisk whose
  block numbers are bounds-checked before they leave the filesystem server — no probe can induce
  one. A third case, an off-by-one in a bitmap-clear helper, moved no marker either but **is**
  caught by host unit tests — a different situation worth distinguishing. Before recording a row as
  uncovered, run `cargo test` under the mutation too, not just a boot.
- **A guard's mutation moves the marker of the probe that exists for that guard, not a marker on a
  healthy path** — a healthy path never trips it. Slice 5.8's plan predicted that deleting a
  not-a-directory guard would break the filesystem self-check; resolving a real file only ever walks
  real directories, so what actually moved was the deny-probe's FAIL line instead.

Two mechanical notes from 5.11: rust-analyzer diagnostics (`dead_code`, unused imports) that appear
mid-run are usually residue of a mutated or half-edited file — trust `cargo clippy … -D warnings` on
the committed tree, not the IDE. And the boot-ratio measurement's `git checkout --detach
<merge-base>` needs a clean tree: commit marker edits first, detach, boot, then check out the branch
again. Stage by explicit path (`git add $(git diff --name-only)`) — a bare `git add -A` sweeps in
unrelated local changes.
