# idle-protocol

Versioned Rust types and JSON Schema for requests, responses and events exchanged
between Idle clients, Evo, and standalone or managed services.

Covers membership, Runner/Control sessions, resources, grants, Control leases and
recovery cursors, plus shared projection inputs. Requests carry contributor identity and retry keys; backend
receipt, runtime acceptance, ordering and completion are separate states.

Consumers depend directly on this crate. It has no dependency on the `app-core`
application, EditChain, private backends or cloud SDKs. Serde is the only default
dependency.

## Use

From a sibling repository's workspace:

```toml
[workspace.dependencies]
idle-protocol = { path = "../host-tools/crates/idle-protocol", version = "0.1.0" }
```

In the consuming crate:

```toml
[dependencies]
idle-protocol.workspace = true
```

Import types from `idle_protocol::v1`. `SessionKind` is `Runner` or `Control`,
serialized as `"runner"` or `"control"`. Messages carry `api_version: "1"`.

`v1::projections::ProjectionSnapshot` separately carries `version: "1"`, a
workspace/chain scope and all five projection destinations. Its rows preserve
full observation/item references, optional HTTPS source pages, supplied labels/status, counts, freshness and
explicit partial/unavailable results. Call `validate()` before admitting decoded
inputs. Evo owns controller payload meanings and maps them into this contract;
this crate does not interpret Note/Link payloads or persist controller state.

`v1::standalone` adds repository mutations, authorized snapshots, transient
presence and bounded invalidation recovery. Mutations use the existing
`Request<T>` / `Response<T>` envelope. `v1::configuration` retains the f27
`Settings` / `AgentRules`, `schema_version` and complete `json` text fields;
each document has an independent conditional revision. Existing command/event
enums and their schema remain unchanged.

## Schema and verification

The optional `schema` feature enables JSON Schema generation. From the repo root:

```sh
cargo run --locked -p idle-protocol --features schema \
  --example export_schema -- crates/idle-protocol/schemas/v1.json
cargo run --locked -p idle-protocol --features schema \
  --example export_projections_schema -- crates/idle-protocol/schemas/projections-v1.json
cargo run --locked -p idle-protocol --features schema \
  --example export_standalone_schema -- crates/idle-protocol/schemas/standalone-v1.json
cargo run --locked -p idle-protocol --features schema \
  --example export_repository_schema -- crates/idle-protocol/schemas/repository-v1.json
./scripts/lint.sh
```

The lint suite checks wire fixture round trips and schema drift alongside the
workspace's Rust checks.

See the [v1 protocol reference](docs/v1.md) for compatibility, retries, authority,
fencing and recovery rules; [JSON Schema](schemas/v1.json) and
[fixtures](tests/fixtures) describe the wire format.
The separate [projection schema](schemas/projections-v1.json) and
[projection fixture](tests/fixtures/projection_snapshot.json) define the shared
input shape. See app-core's [integration guide](../../../app-core/docs/projections.md) for
engine queries, client interactions and the f11 mapper boundary.

The additive [repository schema](schemas/standalone-v1.json) covers
`RepositoryMessage`. The [native service guide](../../docs/coordination.md)
describes its endpoint framing, authorization and f15/f17 runtime boundary.

The [repository snapshot schema](schemas/repository-v1.json) and
[fixture](tests/fixtures/repository_snapshot.json) define read-only Git/GitHub
facts and recorded sessions. These are separate from live session/control grants.
The optional `reflection` feature supplies shared native-shell type metadata.
