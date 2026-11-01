# Packaging and releases

Reusable crates are published as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch serves the sparse Cargo index with immutable
archive checksums; [Cargo configuration](../.cargo/config.toml) registers it.

## Dependency selection

Checks and builds need only this repository's source. The canonical
`scripts/lint.sh` and `scripts/check.sh` select the latest internal Cargo
releases within the declared major version at the start. The committed
`Cargo.lock` supplies the initial third-party selection. The
[native dependency manifest](../native-dependencies.json) declares compatible
engine and exporter ranges; automatic resolution requires a Linux x64 bundle and checksum. Other published
platform checksums are retained when available.

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
Internal dependencies use explicit ranges such as `>=0.5.1, <1.0.0`. Fresh builds
can adopt `0.6.0` and later `0.x` releases automatically; `1.0.0` requires a
manual major-version change. Keep the lower bound accurate when code needs a
new API. Apply the same bounds to Cargo packages and release artifacts.
Workspace path dependencies continue to use their local source, and third-party
requirements retain their existing policy.

The [Check latest released dependencies workflow](../.github/workflows/update-releases.yml)
checks every six hours, or on manual request, and starts main CI when inputs
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
2. The Linux CI workflow checks that version commit.
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

The [native release workflow](../.github/workflows/native-release.yml) automatically
builds Linux x64 in an Ubuntu 22.04 container on the local runner pool, then
publishes the native draft. [native-release.json](../native-release.json) defines
the producer's binaries and test support. macOS and Windows bundles are added
through the manual workflow below.

Both `idle-host` and `idle-coordination` ship in the `idle-coordination-v*`
bundle. `idle-host` keeps its version calculation and Git tags, so a change to
that binary alone still updates the shared bundle version. It does not create
a separate GitHub release; the combined bundle owns the downloadable assets.

## CI platforms and manual builds

Automatic pull-request, main, scheduled, and release runs select Linux only.
Trusted PR jobs use the local PR pool; other Linux release work uses
`IDLE_BACKGROUND_RUNNER` (default `idle-background-linux-x64`). Fork PRs use
GitHub-hosted Ubuntu. Windows dependency checks run only from the manual entrypoint.

Run [Full platform CI](../.github/workflows/full-platform.yml) from Actions with an
existing release tag, or use:

```sh
gh workflow run full-platform.yml --repo idleai/host-tools --ref main -f tag=EXISTING_RELEASE_TAG
```

The run checks out that tag and reuses its recorded dependency versions. It does
not prepare a new version commit. Native dependency tags must already have the
required platform bundles: run their manual workflows first, starting with
`editchain`, then `host-tools`, then consumers. The workflow reports any missing
pinned dependency instead of choosing a different version.

The manual run builds Linux x64, macOS x64/arm64, and Windows x64 bundles, then
attaches missing archives and checksums to the same release. Existing assets are
never overwritten. Set `publish` to `false` for validation with CI artifacts only.

## Unpublished integration

For local Rust work, pass a temporary registry patch with
`cargo --config /absolute/path/local.toml ...`. Keep these overrides out of
committed manifests and lockfiles.

For a full check, use
[memos/scripts/check-integration.py](https://github.com/idleai/memos/blob/v1/scripts/check-integration.py)
with explicit `--producer` and `--consumer` checkout paths. Repeat `--producer`
for paired changes, listing native producers in dependency order. The helper
selects candidate Cargo packages, builds compatible native bundles, runs the
consumer's normal check script and restores dependency files on success or
failure. `--output /path/to/results` retains candidate bundles and dependency
records with their checksums. Temporary registry overrides stay outside the
normal manifests; declared minimum versions must match the APIs being consumed. The manual
[Unpublished package integration workflow](https://github.com/idleai/memos/blob/v1/.github/workflows/integration.yml)
runs the same check for selected branches.

## Registry maintenance

Dependabot version updates are paused. Its custom Cargo registry configuration
still requires the Dependabot secret `PUBLIC_CARGO_REGISTRY_TOKEN` set to the
literal value `anonymous`. This public marker is not an access token; the Cargo
indexes remain anonymously readable.
