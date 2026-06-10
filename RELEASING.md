# Releasing

minix.rs publishes its reusable libraries to crates.io under the `minixrs`
umbrella crate. The OS binaries (kernel, servers, drivers, userland) carry
`publish = false` and are never released to the registry.

## Published crates (publish order)

crates.io rejects `path`-only dependencies, so each crate must be on the
registry before anything that depends on it. The release workflow publishes them
bottom-up:

1. `minixrs-kernel-shared`
2. `minixrs-ipc`
3. `minixrs-server-rt`
4. `minixrs-driver-rt`
5. `minixrs` (facade — re-exports the four above)

All five share a single version via `[workspace.package]` in the root
`Cargo.toml`.

## One-time setup

- Create a [crates.io API token](https://crates.io/settings/tokens) and add it as
  the repo secret **`CARGO_REGISTRY_TOKEN`** (Settings → Secrets and variables →
  Actions). The release workflow no-ops without it.
- The first publish of `minixrs` claims the name on crates.io.

## Cutting a release

1. Bump `version` in `[workspace.package]` (root `Cargo.toml`) and commit on a
   branch; merge via PR as usual.
2. From `main`, tag and push:
   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
3. The tag triggers `.github/workflows/release.yml`, which runs
   `cargo publish --locked` for each crate in the order above. Modern cargo blocks
   until each new version is visible in the index, so dependents resolve cleanly.

## Dry run (local)

`cargo publish --dry-run` resolves dependencies against the **registry**, so only
the bottom crate (`minixrs-kernel-shared`) can be dry-run in isolation — the
dependents fail until their deps are actually on crates.io. To verify the whole
chain offline, package the five crates together: cargo unpacks each just-packaged
sibling into a temporary registry and verify-builds the dependents against it, in
order — exactly what the live publish does:

```sh
cargo package -p minixrs-kernel-shared -p minixrs-ipc \
              -p minixrs-server-rt -p minixrs-driver-rt -p minixrs
```

(Use `--allow-dirty` if you have uncommitted changes. Avoid `cargo package
--workspace` — it also tries to package the `publish = false` binary crates, whose
intra-workspace deps deliberately omit versions, and aborts.)
