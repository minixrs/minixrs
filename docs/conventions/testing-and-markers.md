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

`tools/check-boot-log.sh <disk|ramdisk|reboot> <log>` greps a captured boot log against the marker
files (see [`build-and-boot.md`](./build-and-boot.md) for how to produce the log, which backend a
boot ran with, and how long to budget). Update those marker files in the same PR when trace formats
or the boot roster change, or the `qemu-smoke` CI job goes red.

There are four pairs of files — eight files since slice 6.4 — and the checker reads two pairs for a
full boot and one for the reboot:

| Pair                                      | Checked for      | Holds                                                                                    |
| ----------------------------------------- | ---------------- | ---------------------------------------------------------------------------------------- |
| `tests/qemu-boot.expected` / `.forbidden` | every full boot  | everything that must hold whichever driver serves the root                               |
| `tests/qemu-boot.disk.*`                  | a `disk` boot    | the device's bring-up, `root ok`, the flush, `name=vblk`; forbids the fallback           |
| `tests/qemu-boot.ramdisk.*`               | a `ramdisk` boot | the same bring-up, `root FAIL no root partition`, the fallback line, `name=memory`       |
| `tests/qemu-boot.reboot.*`                | the reboot only  | `persist gen=1` and the two lines saying the root is the disk's; forbids `persist gen=0` |

- **Both full boots attach the boot disk.** Since slice 6.4 the machine boots from its disk, so the
  `ramdisk` boot is the **no-root fallback** boot: the same image without the root partition, which
  `vblk` brings up and declines and MFS falls back from. The device's four bring-up lines are
  therefore required by *both* per-backend files, and they are not in the common file only because
  the hand-run `MINIXRS_ROOT=nodisk` boot has none of them and must still pass the common set.
- **The no-device state has no checked boot.** `[diag vblk] no device` and a DS miss in MFS are
  reached only with `MINIXRS_ROOT=nodisk`, by hand; both per-backend files forbid `no device`, so
  that log passes the common pair and fails both backends' by design.
- **The backend argument is required, and there is no way to check the common pair alone.** The
  common files cannot tell a disk boot that silently fell back to the ramdisk from a healthy one:
  every filesystem marker passes either way. What closes that is the **forbidden** half of the
  per-backend pair — a `disk` boot forbids `bdev.ds ok name=memory`, `bdev.root vblk`, `[diag vblk]
  no device`, `[diag vblk] root FAIL` and `bdev.minor absent`; a `ramdisk` boot forbids `bdev.ds ok
  name=vblk`, `[diag vblk] root ok`, `[diag vblk] no device`, `[diag vblk] flush ok` and `bdev.minor
  ok`. Each requires a line the other forbids, so each boot fails the other backend's set.
- **The reboot is checked against its own pair and nothing else** — not the common pair, not the
  disk pair. It is the second boot of the image a disk boot wrote, and init's write battery was
  designed against a fresh image: it creates files that must not exist, fills a file that ships
  empty, grows a directory that ships exactly full. What it does on its own leftovers is not
  defined, so a full check of a used image would judge behaviour nobody specified. The persistence
  probe therefore runs *before* the battery and the reboot is stopped soon after its line. Two
  consequences: `persist gen=0` is required of every full boot and forbidden on a reboot, so neither
  log can pass as the other; and the common forbidden list is not applied to a reboot, so a panic
  there is caught only by `persist gen=1` never appearing.
- **The full boots are fed boots, and exactly one required line needs the feeding.** Slice 6.5 put
  five console lines in the common expected file — `[diag tty] rx on`, `[diag vfs] cons.notify ok`,
  `[diag vfs] cons.park`, `[diag vfs] cons.beside ok` and `minix.rs init: stdin ok n=15` — and only
  the last can print solely when `tools/feed-console.sh` supplied the boot's stdin (the command
  form, and what the feeder waits for, are under [Fed boots](./build-and-boot.md#fed-boots)). The
  other four depend on no input and every boot prints them, fed or not: the receive masks were set,
  TTY's notify probe arrived at VFS, a read was parked, a file read was served beside it. An
  **unfed** full boot therefore fails on `stdin ok` alone, by design; that is a missing feeder, not
  a regression, and it is the first thing to rule out when `stdin ok` is the only line missing. The
  same goes for the forbidden `stdin FAIL`: it describes a fed boot, and a boot somebody typed at
  prints it correctly. The **reboot is not fed** and loses nothing by it — it is checked against its
  own pair only, which holds none of the five.
- **`stdin ok` proves two interrupts, not one, and only because of how the boot is fed.** The feeder
  sends the probe in two chunks a few seconds apart ([Fed boots](./build-and-boot.md#fed-boots) has
  the why), so the line needs the receive interrupt, the drain, the notification, VFS's retry — and
  then TTY's **re-arm** of its line, without which the second chunk never interrupts. Fed in one
  write, the same line proves everything but the re-arm.
- **`cdev.short ok n=256` has carried a second proof since slice 6.5**, and `cons.notify ok` exists
  for the same one: a kernel rule about notifications and `SENDREC`, stated with its failure line in
  [`kernel.md`](./kernel.md#a-pending-notification-is-never-the-reply-to-a-sendrec). A `cdev.short
  FAIL rc=4096` is that rule broken, not a console-write bug.
- **A new marker goes in the common pair unless it exists because of one backend.** Everything the
  filesystem proves — mount, read, write, create/truncate, the hole probe, exec from the root — is
  common and therefore required of both drivers. A marker moved into a per-backend file stops being
  required of the other backend, so that is a decision, not a tidy-up.
- **`tools/diff-backends.sh <disk-log> <ramdisk-log>` checks what the marker files cannot: that
  nothing *else* differs.** It takes each log's `[diag mfs]` lines and `[diag vfs]` lines as two
  separate streams, keeps the first occurrence of each line in order, and fails unless the VFS
  streams are identical and the MFS streams differ in exactly the `bdev.ds ok name=` line — with two
  further allowances since slice 6.4, each for one exact line: the ramdisk side may carry `bdev.root
  vblk rc=-6 fallback=memory` (at most once; allowed, not required — requiring it is the marker
  file's job), and the `bdev.minor` verdict is each backend's own (`ok minor=1 mbr=1` on the disk
  side, `absent minor=1 rc=-6` on the ramdisk side, at the same place in the two streams, at most
  once each). Another `rc`, a longer line, a `FAIL`, or either line on the wrong side still fails.
  The shape was measured, not chosen: two boots of the *same* backend differ in the raw interleaved
  stream (the servers interleave differently from boot to boot, and VFS prints one `pm-exit reset`
  line per exit for as long as the boot runs), and agree per server by first occurrence. What it
  therefore **cannot** see is a backend-dependent change in how often a line repeats, or in the
  order of lines between the two servers. `--self-test` proves it still fails on a difference; CI
  runs that before the comparison.

A few properties of the checker matter when you're reading its output rather than just running it:

- **The marker files test first occurrences only**, never counts — keep expectations timing-robust,
  since CI's TCG is slower than local and a marker that only ever appears once is the only kind that
  survives that difference.
- **It works on a partial log.** Once it reports PASS on a still-growing log, the verdict is final
  and the boot can be stopped — the 5.10b review round got all 97 markers well inside the 600 s
  budget. Only `qemu-smoke`'s exit-124 assertion needs the timeout to actually elapse. Copy the log
  aside before checking; the live file grows underneath the script.
- **Its PASS total counts expected AND forbidden lines** (5.11: 86 + 20 = 106), so a slice that adds
  N expected and M forbidden lines moves the total by N + M — don't be surprised when the total
  moves by more than the expected-marker count alone. The total is per boot kind, because each adds
  its own pair to the common one (6.4: 165 for each full boot, 7 for the reboot; 6.5 added six
  expected and three forbidden lines to the common pair, so 174 and still 7 — counted from the files
  as committed: 113 + 48 common, 8 + 5 per backend, 3 + 4 for the reboot); count the marker files
  rather than quoting a number from a plan.
- **Never copy a count into a marker from a plan** — recompute it from the constant. 5.8's plan said
  `n=30` for what was actually a 31-byte `ROOTFS_MOTD`.
- **A marker must be able to fail on the thing it proves.** The script matches with `grep -F`, so a
  prefix is a substring: 6.2's planned `[devmap] vblk va=0x4000` also matched TTY's page at
  `0x40000000`, the exact off-by-one-page mapping it existed to catch. Assert the whole line when
  every field is deterministic. When one field varies by run, print the stable fields first — 6.2
  moved `pages=` ahead of the firmware-dependent `pa=` so that `pages=16` could be asserted.

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

A clean 300 s run on this branch reaches **~18.8 M**; a passing 1200 s run reached **114 M**. (Slice
6.3's baseline, two 300 s boots of its merge base, reached 32.3 M and 31.9 M — the floor below is
deliberately far under any clean run.) **Under ~10 M at the timeout means the run was starved, not
broken.** Re-run before believing any marker failure, and never change code on the strength of a
starved boot — a "fix" that passes on the re-run has proved nothing about the code.

**The shape of the missing set is the better tell, and the threshold alone will fool you.** One run
reached 10.59 M — over the floor — and was plainly starved. Read the whole `MISSING` list:

- **Contiguous from some point to the end of the boot** → the run stopped early. Starvation.
- **A specific subsystem's markers gone while later ones still land** → a real regression.

The first shape is unmistakable once seen: a starved run loses `SYS_FORK`, then every exec, then
everything downstream, because it never got there. A regression in the exec path would let fork
through and take out exec alone.

**Never pipe `check-boot-log.sh` through `tail`.** It prints one `MISSING` line per marker and the
verdict last, so `tail -2` shows the final miss and hides the rest — which is exactly how a
contiguous tail-of-boot failure gets mistaken for a single broken marker. Since the last missing
marker is usually the newest feature's, a truncated verdict points straight at whatever you just
changed. Use `grep -c '^MISSING'` and read the full list.

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
  since `check-boot-log.sh` FAILs the stub A–D markers there by design. A mutation whose marker
  depends on console input needs the boot [fed](./build-and-boot.md#fed-boots) like any other; an
  unfed one never prints `stdin ok` whatever the code does.
- **Feed a receive-path mutation with the standard feeder, never with a `printf` of your own.**
  `tools/feed-console.sh` sends the probe in two chunks; a hand-rolled feed that writes it at once
  delivers one interrupt, and every mutation of what TTY does *after* its first drain then passes.
  6.5's feeder itself first wrote the probe in one piece, and "drop the `IRQ_ENABLE` after the
  drain" passed under it, which is why it now has two chunks. If a mutation needs different input,
  keep it in at least two writes with a gap between them.
- **A probe proves as many events as it causes.** One write was one interrupt, so it proved one
  interrupt's worth of the receive path and nothing about the steady state. Before trusting a probe
  for a loop — re-arm, re-queue, retry — count how many times the probe makes the loop go round; if
  the answer is once, the code that sets up the *second* turn is unproved, and the mutation that
  deletes it is the way to find out.
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
- **A mutation predicted to be caught by "a host test" must name the test.** 6.3's spec predicted
  exactly that for skipping the read in `virtio-blk`'s partial write. The host tests cover the
  *plan* (`write_plan` returns `Partial`), not the step in `main.rs` that acts on it, which no host
  test calls; the mutation moved nothing at all until a boot probe was written for it
  (`bdev.partial`, see
  [`servers-and-drivers.md`](./servers-and-drivers.md#writing-probes-and-denial-batteries)). A
  prediction that cannot name its test is a guess that the coverage exists.
- **A refusal path needs a boot that provokes the refusal.** CI's boots prove a disk with a root and
  a disk without one (the fallback, since 6.4). Every other refusal — a read-only disk, a damaged
  partition table, a disk with no table at all — and the no-device state are boots run by hand with
  `MINIXRS_VBLK_IMAGE` / `MINIXRS_VBLK_DRIVE_OPTS` / `MINIXRS_ROOT=nodisk` and recorded in the phase
  file, because CI runs none of them.
- **The firmware writes to a writable boot disk, so boot a damaged-table image read-only.** edk2
  repairs a damaged primary GPT header or entry array from the backup copy *before the kernel runs*
  (6.4: one bit flipped in either, and the driver printed `root ok` with the full disk set passing).
  A mutation or refusal boot that depends on the damage reaching the driver needs `readonly=on`, and
  where the firmware then refuses to boot the disk at all (a damaged header), the second-drive
  recipe in [`build-and-boot.md`](./build-and-boot.md). The same fact limits "the image is unchanged
  afterwards" as evidence: a byte comparison of a boot disk can differ because of a write no
  minix.rs code made.
- **A mutation booted off the standard path needs a control boot beside it.** 6.4's deleted
  header-CRC check had to be booted on a read-only second drive; there the mutated driver printed
  `root FAIL readonly` — the next gate — and only the unmutated control (`root FAIL gpt crc`) shows
  that the line moved.
- **A mutation at a call site and the same mutation inside the pure helper are two rows.** Skipping
  the flush where `do_write` calls it failed no host test and moved one marker; the same omission
  inside `blk::flush_after` fails a host test. Record where the mutation was applied, and when a
  decision is host-tested but its caller is not, run the call-site form.
- **A guard's mutation moves the marker of the probe that exists for that guard, not a marker on a
  healthy path** — a healthy path never trips it. Slice 5.8's plan predicted that deleting a
  not-a-directory guard would break the filesystem self-check; resolving a real file only ever walks
  real directories, so what actually moved was the deny-probe's FAIL line instead.
- **Mutate a guard only once something exercises it.** 6.2's stub/driver priv-id collision moved no
  marker when its task landed, because no driver ran in the colliding slots yet; with virtio-blk
  live the same mutation hung the default boot with 47 markers missing. A "nothing moves" row
  recorded before the consumer exists is not a result — re-run the mutation when the consumer lands.
- **QEMU is more permissive than the specification, so observe which check fires rather than
  predicting it.** 6.2 predicted that a wrong register page would fail the virtio magic check; an
  empty virtio-mmio slot still reads the magic, and the DeviceID check caught it instead. It also
  predicted that the status read-back would refuse a driver omitting `VIRTIO_F_VERSION_1`; QEMU
  keeps `FEATURES_OK`, and only the exact `features=` marker text and the host tests catch that
  (re-observed in 6.4 with optional features requested: `virtio ok features=0x200`, and the device
  kept working fully).

Two mechanical notes from 5.11: rust-analyzer diagnostics (`dead_code`, unused imports) that appear
mid-run are usually residue of a mutated or half-edited file — trust `cargo clippy … -D warnings` on
the committed tree, not the IDE. And the boot-ratio measurement's `git checkout --detach
<merge-base>` needs a clean tree: commit marker edits first, detach, boot, then check out the branch
again. Stage by explicit path (`git add $(git diff --name-only)`) — a bare `git add -A` sweeps in
unrelated local changes.
