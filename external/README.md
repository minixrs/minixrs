# external/ — vendored third-party code

Two different vendoring mechanisms live here, for two different reasons.

| Directory | Mechanism | Upstream | Licence |
|---|---|---|---|
| `limine/` | `make` fetch of a pinned commit (artifacts gitignored) | [limine-bootloader/limine](https://github.com/limine-bootloader/limine) | BSD Zero Clause |
| `musl/` | git submodule, pinned commit on branch `minixrs` | [minixrs/musl-minixrs](https://github.com/minixrs/musl-minixrs) | MIT |

See `limine/README.md` for the bootloader. The rest of this file covers musl.

## `musl/` — the libc fork (slice 5.6)

`external/musl` is a submodule of **`minixrs/musl-minixrs`**, our fork of
[musl libc](https://musl.libc.org/), tracking its **`minixrs`** branch — *not*
`main`, which is a pristine upstream mirror with no port on it. The fork is
based on upstream tag `v1.2.6`.

musl is **MIT licensed**; the fork adds nothing under a different licence. This
table is the only record of that fact in this repo: `cargo-deny` and
`cargo-audit` walk the cargo dependency graph, which a C source tree is invisible
to, so nothing automated can notice the licence here.

Initialise it with:

```sh
git submodule update --init --recursive
```

### Building it

`tools/build-musl.sh` drives configure/make into a cached sysroot at
`target/musl-sysroot`. Everything it writes lives under `target/`; the submodule
work tree is left **pristine**, because the `c-headers` CI job asserts a clean
`git status` and a build that dirtied a submodule would fail it.

Nothing builds musl automatically. `kernel/build.rs` presence-checks
`target/musl-sysroot/.stamp` and, when it is missing or stale, packs the
`worker` ELF under the name `hello` and emits a `cargo::warning` — so a fresh
clone still boots green, it just loses the slice-5.6 C markers. Kicking off a
multi-minute libc build from inside a cargo build script would be worse.

### Bumping the pin

The port branch is **force-pushed** when it is rebased onto a new upstream tag,
which orphans whatever commit this submodule pins. So a fork rebase and the
submodule bump here must land in the **same PR** — otherwise this repo points at
a commit that no longer exists and the submodule fails to clone.

The fork's own `MINIXRS.md` carries the branch contract, the delta contract, and
a Linux-dependency inventory re-derived against v1.2.6.
