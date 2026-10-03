# host-tools

Open source Idle contracts, native history tools and reusable host integrations.
Clients combine these packages with app-core and their platform adapters. This
workspace has no dependency on app-core, a renderer or the VS Code API.

| Package | Responsibility | Targets |
| --- | --- | --- |
| `idle-protocol` | Versioned requests, responses, events and JSON Schema shared by clients, Evo and managed/standalone services. | Native and WASM |
| `idle-history` | Portable history/source contracts, display classifications and pure peer/query helpers. | Native and WASM |
| `idle-peer-state` | WASM bindings for the shared peer connection helpers. | Native build and WASM |
| `idle-history-import` | Claude, Codex and human archive import, schema conversion and source reconciliation. | Native |
| `idle-history-collector` | Automatic source discovery, durable collection, Git reconciliation and change notifications. Library, framed service and standalone watch executable. | Native |
| `idle-history-tools` | Import, conversion and source-inspection commands. | Native |
| `packages/history-runtime` | Portable TypeScript peer coordinator with host-supplied transports, credentials and connection state. | Node |

EditChain supplies its engine through the sibling `../editchain` checkout.
Codex import runs an explicit `codex-session-exporter` executable. That exporter
stays with its Codex types in `codex/tools/codex-session-exporter`; consumers can
install the binary without a Codex source checkout.

## Build and verify

The toolchain is pinned in `rust-toolchain.toml`. Install Node 22, Git,
`cargo-deny` 0.20.2 and the WASM target, then run:

```sh
npm --prefix packages/history-runtime ci
./scripts/lint.sh
./scripts/check.sh
```

The lint suite checks all native packages and explicitly checks the three
portable packages on WASM. It retains import fixtures, schema drift checks,
durable recovery tests and Rust/Clippy/Rustdoc/dependency checks.

The full check also builds the portable peer coordinator and runs a standalone
collection process with the real exporter. By default it builds the exporter
from the sibling Codex checkout. Set `IDLE_CODEX_EXPORTER` to an installed absolute
executable path to use that binary instead. The check always runs the same
collection scenario: discovery, append, exclusive ownership, graceful shutdown,
restart and source replacement, without app-core or VS Code.

## Integration

- [Architecture](docs/architecture.md): package dependencies, effect execution and
  the planned public Offstage client.
- [Collection](docs/collection.md): Rust API, framed requests, standalone watch
  mode and process ownership.
- [Import API](docs/import-api.md), [commands](docs/import-cli.md),
  [conversion](docs/import-conversion.md) and [scaling](docs/import-scaling.md).
- [Coordination protocol](crates/idle-protocol/README.md): shared types, JSON
  Schema and wire compatibility.
- [Peer coordinator](packages/history-runtime/README.md): transport injection
  and native worker integration.

The `f47/host-tools` restructuring moves existing package ownership and makes
collection usable from native hosts. Production Offstage API adapters, full Evo
coordination and native/TUI applications retain their separate feature work.
