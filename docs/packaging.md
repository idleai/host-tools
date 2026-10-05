# Packaging and releases

Reusable crates are published as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch serves the sparse Cargo index with immutable
archive checksums; [Cargo configuration](../.cargo/config.toml) registers it.

## Dependency selection

Checks and builds need only this repository's source. The canonical
`scripts/lint.sh` and `scripts/check.sh` select the latest compatible internal
Cargo releases at the start. The committed `Cargo.lock` supplies the initial
third-party selection. The
[native dependency manifest](../native-dependencies.json) declares compatible
engine and exporter ranges; resolution requires complete platform bundles.

The resolver verifies published checksums and writes the selected versions and
Cargo lockfile to ignored `target/released-dependencies.json`. CI jobs, release
verification and packaging reuse that selection. CI retains the record as an
artifact, and releases attach it. Native bundles also include the record.
A later build of the same commit can select newer dependencies.

Use an authenticated GitHub CLI (`gh`) for release discovery. For an individual
local command, run this from the repository root:

```sh
python3 scripts/release_dependencies.py run -- cargo build --workspace --locked
```

`--locked` keeps later commands within that build on the resolved selection.
Keep consumer minimum versions accurate when adopting a new API: `^0.1.2`
accepts `0.1.3`; adopting `0.2.0` requires a manifest change.

The [Check latest released dependencies workflow](../.github/workflows/update-releases.yml)
checks every 15 minutes, or on manual request, and starts main CI when inputs
change. PR builds resolve immediately. No dependency-update branch or PR is
created. Retry a failed selection through CI or publish a fix for the next check.
Release preparation incorporates the selected Cargo dependencies in its version
commit, allowing dependency changes to produce new packages.

[The artifact installer](../scripts/install-artifacts.py) downloads the selected
native bundles into ignored `.artifacts/`. Full checks exercise the released
exporter, peer coordinator and standalone collection lifecycle.

## Automatic releases

The [Release workflow](../.github/workflows/release.yml) starts after successful
`main` CI:

1. Release-plz calculates versions and changelogs from commit messages and Rust
   API checks, then commits the release metadata to `main`.
2. The full CI workflow checks that version commit.
3. Crate archives, Cargo index entries and native bundles publish from that
   verified commit using its tested dependency selection.

Release preparation uses a normal push, so a concurrent change to `main` cannot
be overwritten. Declare breaking changes and consumer minimum versions in the
feature PR. Documentation changes with no effect on packaged contents do not
create a new package version.

Use **Re-run failed jobs** on the original Release run to finish a failed
publication with its verified commit, even if `main` has advanced. Dispatch
**Release** on `main` to prepare current changes or resume its version commit.
Retries can finish draft releases; published versions and archives are immutable.

The [native release workflow](../.github/workflows/native-release.yml) builds
Linux x64, macOS x64/arm64 and Windows x64 bundles. It publishes the native draft
after all platform builds complete. [native-release.json](../native-release.json)
defines the producer's binaries and test support.

Both `idle-host` and `idle-coordination` ship in the `idle-coordination-v*`
bundle. `idle-host` keeps its version calculation and Git tags, so a change to
that binary alone still updates the shared bundle version. It does not create
a separate GitHub release; the combined bundle owns the downloadable assets.

## Unpublished integration

For local Rust work, pass a temporary registry patch with
`cargo --config /absolute/path/local.toml ...`. Keep these overrides out of
committed manifests and lockfiles.

For a full check, use
[memos/scripts/check-integration.py](https://github.com/idleai/memos/blob/v1/scripts/check-integration.py)
with explicit `--producer` and `--consumer` checkout paths. It patches Cargo,
builds candidate native bundles when needed, runs the consumer's normal check
script and restores dependency files. The manual
[Unpublished package integration workflow](https://github.com/idleai/memos/blob/v1/.github/workflows/integration.yml)
runs the same check for selected branches.

## Registry maintenance

Dependabot version updates are paused. Its custom Cargo registry configuration
still requires the Dependabot secret `PUBLIC_CARGO_REGISTRY_TOKEN` set to the
literal value `anonymous`. This public marker is not an access token; the Cargo
indexes remain anonymously readable.
