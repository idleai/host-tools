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
Codex import runs an explicit `codex-session-exporter` executable. That exporter
stays with its Codex types in `codex/tools/codex-session-exporter`; consumers can
install the binary without a Codex source checkout.

## Build and verify

The toolchain is pinned in `rust-toolchain.toml`. Install Node 22, Python 3.12 or
newer, Git, an authenticated GitHub CLI (`gh`), OpenSSL development libraries,
`cargo-deny` 0.20.2 and the WASM target, then run:

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

The full check also runs standalone collection with the released exporter,
covering discovery, append, exclusive ownership, shutdown, restart and source
replacement.

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

Checks select the latest compatible internal releases and reuse that selection
through testing and packaging. Rebuilding a commit can select newer versions.
Successful main CI starts automatic crate and native bundle publication. See
[packaging and releases](docs/packaging.md) for the workflow, retries and testing
unpublished dependencies.
