# Repository workspace configuration

Standalone clients can supply `workspace_root` when opening the native
coordination service. The coordinator then uses the selected checkout's
`.idle/workspace/` files as the source of authored configuration. The same
`RepositoryFiles` and `Authority::open_repository` APIs work without VS Code.
The service advertises `workspace_configuration: 1` in its version response.

```text
.idle/workspace/
  workspace.json
  settings.json
  agent-rules.json
  control.json
  hosts.json
  providers.json
  projections/
    review-readiness.json
```

`workspace.json` contains `schema_version: 1`, a stable `id` and a display `name`.
The first local metadata connection creates this manifest, empty controller and
resource definitions, and migrates any existing settings, rules and saved views.
Settings and rules remain absent until authored. Track these files in Git to
share their committed versions and restore them with a clone. A UI save writes
the working tree immediately; it does not stage, commit or push anything.

## File formats

Settings, agent rules and controller configuration are complete JSON objects.
Settings and rules retain their exact text, including unknown fields. Controller
configuration is stored for the runtime owner; reading it never starts an agent.
Host and provider documents contain `hosts` and `providers` arrays respectively:

```json
{
  "hosts": [{"id": "workstation", "name": "Development host", "routes": []}]
}
```

```json
{
  "providers": [{
    "id": "local",
    "name": "Local models",
    "host_id": "workstation",
    "credential_ref": "local-serving",
    "routes": []
  }]
}
```

Omit `host_id` for an external provider. `credential_ref` names host-owned
credentials; never put credential values in these files. Resource declarations
have no owner, permission, runtime or health fields. Native snapshots display
them as unknown/unconnected, with no executable capabilities. Actual authorized
runtime publications take priority over declarations with the same ID. Declared
resources never enter the authority's grant or controller tables.

A projection is a separate document with an object-valued definition:

```json
{
  "schema_version": 1,
  "id": "review-readiness",
  "title": "Review readiness",
  "kind": "task",
  "definition": {
    "filters": {"labels": ["review"]},
    "layout": {"group_by": "author"}
  }
}
```

Portable IDs use lowercase ASCII letters, digits, dots, underscores and hyphens,
with no leading dot or reserved Windows device stem. They use their literal
filename. Other IDs or IDs longer than 128 bytes retain their original identity
and use a `~<BLAKE3>.json`
filename, so migration never interprets an ID as a filesystem path. Projection
destinations retain the existing `activity`, `task`, `error`, `triage` and
`need_input` meanings. Definitions are preserved and returned through the
existing saved-view contract; evaluating additional query languages or building
a new projection authoring UI remains with the projection feature owners.
Unknown compatible manifest and projection document fields survive updates.

The schema bundle is
[`workspace-config-v1.json`](../crates/idle-protocol/schemas/workspace-config-v1.json).
Regenerate it with:

```sh
cargo run -p idle-protocol --features schema --example export_workspace_config_schema -- crates/idle-protocol/schemas/workspace-config-v1.json
```

## Reads, saves and recovery

`workspace_configuration` returns authored definitions, the stable manifest
identity and a content digest covering the observed files. `snapshot` retains
the existing settings/rules/view contracts. Clients continue to submit the same
conditional configuration and view mutations. The native adapter writes their
files and retains request results in private storage.

Before a snapshot, change-feed poll or new mutation, the coordinator rereads the
bounded files. External edits and deletions advance local revisions and generate
the existing domain invalidations. This uses the client's existing subscription
poll, including its one-second idle poll interval; it needs no renderer watcher.
Invalid JSON, merge-conflict markers, duplicate resource IDs, unknown versions,
symlinks and oversized input produce errors without replacing confirmed state.
The adapter reads at most 256 projection files and 4 MiB of authored content;
settings/rules/controller objects have a 256 KiB limit.

Native writers share a checkout-specific OS lock, compare the previously read
content, and replace one file atomically. A private journal spans the file write
and saved request result. Restart completes an interrupted write only when the
file still matches the expected or intended content. Divergent edits stop
recovery with a conflict and are never overwritten. Completed retries return
their original result, even when a later edit changed the file. Direct editor
and Git writes do not participate in the native writer lock; as with ordinary
editor saves, independently replacing the same file still requires conflict
resolution.

Existing repository files win over old private configuration. Migration runs
once, retains the private originals and request records, and does not recreate a
deleted definition on restart. A missing or changed manifest after attachment
requires explicit repair/rebinding. Unsupported older definitions stop migration
without deleting the originals.

## Checkout and runtime boundaries

The manifest ID travels with clones and worktrees. Existing checkout-local
workspace/repository/chain aliases remain unchanged, preserving recorded history,
drafts and request identities. The new authored identity does not implicitly
join two checkouts, remap a chain or authenticate a runtime.

Each worktree reads its own tracked definitions. A branch checkout reloads
definitions and advances local revisions, even when returning to older content.
It does not roll back controller epochs, grants or live sessions. Future Evo
integration must select an explicit configuration digest for a running controller
and apply changes through that runtime's reload policy.

Private coordination storage continues to hold contributor/device identity,
grants and revocations, controller leases, request receipts, migration recovery
and cached file observations. Account-scoped sharing channels omit
`workspace_root` and retain their existing authority. EditChain continues to hold
recorded work and controller results; runtime state and model authentication stay
with their runtime/credential owners.
