# host-tools

Open source Idle contracts, native history tools and reusable host integrations.
Clients combine these packages with app-core and their platform adapters. This
workspace has no dependency on app-core, a renderer or the VS Code API.

| Package | Responsibility | Targets |
| --- | --- | --- |
| `idle-host` | One supervised process with independent capture, history, collection, repository and coordination channels. | Native |
| `idle-protocol` | Versioned requests, responses, events and JSON Schema shared by clients, Evo and managed/standalone services. | Native and WASM |
| `idle-coordination` | Repository metadata, settings/rules, views, peer activity, grants, controller ownership and native peer coordination. Library and framed service. | Native |
| `idle-history` | Portable history queries/results, repository bindings, projection inputs, source contracts and pure peer helpers. | Native and WASM |
| `idle-editor-capture` | Editor observation validation, conversion, archive replay and durable writer; `idle-editor-service` executable. | Native |
| `idle-history-native` | Shared history queries, exact record/file/diff reads and author/exposure projections; `idle-history-service` executable. | Native |
| `idle-history-import` | Claude, Codex and human archive import, schema conversion and source reconciliation. | Native |
| `idle-history-collector` | Automatic source discovery, durable collection, Git reconciliation and change notifications. Library, framed service and standalone watch executable. | Native |
| `idle-history-tools` | Import, conversion and source-inspection commands. | Native |
| `idle-host-io` | Shared bounded little-endian framing for capture, collection, repository and history services. | Native |
| `packages/history-runtime` | TypeScript native peer bridge and invitation support for wire-interoperability tests. VS Code sharing uses `idle-coordination`. | Node |

EditChain supplies versioned engine crates through its GitHub-hosted Cargo index.
Cargo records the selected versions and archive checksums in `Cargo.lock`.
Codex import runs an explicit `codex-session-exporter` executable. That exporter
stays with its Codex types in `codex/tools/codex-session-exporter`; consumers can
install the binary without a Codex source checkout.

## Build and verify

The toolchain is pinned in `rust-toolchain.toml`. Install Node 22, Python 3.12 or
newer, Git, OpenSSL development libraries, `cargo-deny` 0.20.2 and the WASM target,
then run:

```sh
npm --prefix packages/history-runtime ci
./scripts/lint.sh
./scripts/check.sh
```

The lint suite checks all native packages and explicitly checks the two
portable packages on WASM. It retains import fixtures, schema drift checks,
durable recovery tests and Rust/Clippy/Rustdoc/dependency checks.
It also tests two native peers, interoperability with the existing TypeScript
peer bridge, service restart and managed handoff through an injected adapter.
The full check runs the pinned Microsoft Dev Tunnels SDK's unit tests with the
workspace SSH patches. These checks need no cloud account; the optional
[live relay smoke test](docs/coordination.md#verification) creates temporary relays.

Native coordination consumers need no Node installation, app-core or VS Code:

```sh
cargo build --locked -p idle-coordination --bin idle-coordination
target/debug/idle-coordination --config /absolute/path/service.json
```

The executable serves framed JSON on stdin/stdout. See the
[configuration and API guide](docs/coordination.md) before starting it.

The full check builds the native peer coordinator and runs a standalone
collection process with the released exporter. `native-dependencies.json` records
engine/exporter tags and archive checksums; `scripts/install-artifacts.py` installs
them under ignored `.artifacts/`. The check runs the same
collection scenario: discovery, append, exclusive ownership, graceful shutdown,
restart and source replacement, without app-core or VS Code.

## Integration

- [Architecture](docs/architecture.md): package dependencies, effect execution and
  the planned public Offstage client.
- [Shared native host](docs/native-host.md): workspace channels, framing, credentials,
  queue limits, cancellation and process recovery.
- [Collection](docs/collection.md): Rust API, framed requests, standalone watch
  mode and process ownership.
- [Import API](docs/import-api.md), [commands](docs/import-cli.md),
  [conversion](docs/import-conversion.md) and [scaling](docs/import-scaling.md).
- [Coordination protocol](crates/idle-protocol/README.md): shared types, JSON
  Schema and wire compatibility.
- [Native coordination](docs/coordination.md): authority, native service, peer
  lifecycle, managed adoption and coordinated upgrades.
- [Peer coordinator](packages/history-runtime/README.md): transport injection
  and native worker integration.

The `f47/host-tools` restructuring moves existing package ownership and makes
collection usable from native hosts. Production Offstage API adapters, full Evo
coordination and native/TUI applications retain their separate feature work.

The [standalone repository reader](docs/repository.md) supplies Git/GitHub data,
exact projection sources and recorded-session discovery through a native library
and framed service. It has no app-core or renderer dependency.

Editor capture and history services build independently of app-core and VS Code:

```sh
cargo build --locked -p idle-editor-capture -p idle-history-native --bins
```

`idle-history-native` supports `default-features = false` for applications that
only need its query/projection library. The default `service` feature adds the
editor activity, content-preview and repository service endpoints. Applications
keep their effect/result adapters. The extension links these service libraries
through `idle-host` and provides the editor UI. Standalone binaries remain in the
producer bundle for other consumers and compatibility tests.

## Package releases

Our reusable crates are stored as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch contains the Cargo sparse index; its entries
include immutable archive checksums. `.cargo/config.toml` registers the indexes.
Normal checks use the committed lockfile and need only this repository's source.

A successful `main` CI run starts the Release workflow. Release-plz calculates
versions and changelogs, and automation commits that metadata to `main`. The
entire CI workflow checks the version commit before any package is published.
Package archives, indexes and native bundles then publish from that exact commit;
there is no separate release PR. Concurrent changes to `main` are never overwritten.

Declare breaking changes in the feature PR, including the required minimum
versions in consumers. Release-plz uses commit messages and Rust API checks to
calculate the next version. To recover a failed publication, rerun **Release**
on `main`; existing versions and public archives remain immutable, and unfinished
drafts can resume. A documentation-only change that does not alter packaged
contents does not create another package version.

Dependabot requires a secret reference for custom Cargo registries, including
public ones. Set the repository's Dependabot secret `PUBLIC_CARGO_REGISTRY_TOKEN`
to the literal value `anonymous`. This is a public marker, not an access token;
the GitHub indexes remain anonymously readable.

The native release workflow builds Linux x64, macOS x64/arm64 and Windows x64
bundles when releasing the native tools. It publishes the draft only after all
platform builds complete. `native-release.json` defines the binaries and test
support owned by this producer.

The **Update released artifacts** workflow checks for compatible internal
packages and complete native/consumer archives every 15 minutes, or on manual
request. It groups lockfile versions and archive checksums in one generated PR
and starts the full CI workflow. Successful CI for the current bot commit allows
a fast-forward into `main`, preserving the exact tested commit. If `main` has
advanced, the updater refreshes the PR and CI runs again. Failed or incompatible
updates stay open for review. Ordinary feature PRs and third-party Dependabot PRs
retain their normal review process. CI files contain no sibling checkout commits
to advance after each producer change.


## Coordinated development

For ordinary local Rust work, add a temporary Cargo patch for the relevant
registry and pass it with `cargo --config /absolute/path/local.toml ...`.
Keep these overrides out of committed manifests and lockfiles. Full checks with
an unpublished producer can use `memos/scripts/check-integration.py` with
explicit `--producer` and `--consumer` checkout paths. It temporarily patches
Cargo, builds candidate native bundles when needed, runs the consumer's normal
check script and restores its dependency files. The manual **Unpublished package
integration** workflow in memos runs the same check for selected branches.
