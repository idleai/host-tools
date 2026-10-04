# Native repository coordination

`idle-coordination` is a Rust library and executable that runs without Node,
app-core, VS Code or a managed backend. EditChain handles peer-v5 authentication,
device membership and durable record/blob replication. The coordinator manages
repository metadata, consent, relay connections, retries and discovery.

## Authority and authentication

One `Authority` owns a repository's metadata, settings/rules, view definitions,
memberships, resource grants and controller lease. `FilePersistence` locks its
private directory and commits through atomic, synchronized compare-and-swap.
Injected `Persistence` adapters must provide the same guarantees.

All collaborators' metadata operations must reach this single writer; history
replication neither copies authority state nor elects controllers. Embedding
hosts authenticate each `Principal` outside request JSON. The native stdin
service binds the process owner's principal at startup and opens no network
listener. Socket or remote adapters supply authentication and connection isolation.

`Request<Mutation>` carries the contributor, workspace, immutable request key,
deadline and optional control fence. Writes require `Absent` or the exact current
revision. Settings and agent rules have separate revisions and preserve unknown
JSON fields. Saved views contain definitions; their results are derived elsewhere.

Successful writes and domain refusals retain their original results. Reusing a
key with different content is a conflict. Requests expire within 24 hours, with
at most 4,096 retained outcomes and 16 MiB of saved state. Capacity refusals leave
existing state and retries available. A storage failure may have committed:
reopen the faulted authority and use `request_status` with the original key.

Membership permits metadata participation; host execution, session input and
provider use require separate grants. Revoking membership overrides those grants.
Snapshots filter resources for the current contributor. Recovery keeps 256
ordered invalidations; membership/grant changes or missing history require a new
snapshot. Presence is attributed to the authenticated contributor, limited to the
repository and visible hosts, and expires within 120 seconds.

Controller leases check the authenticated runtime, host, Control session and
grants. Epochs increase durably; leases last at most 60 seconds. Validation time
never moves backward within an open authority. Restart retires the lease while
retaining its epoch. Call `validate_control` immediately before using a fence.

## Native process

For a library consumer, add a path dependency on
`../host-tools/crates/idle-coordination`. Cargo applies patches only from the
consuming workspace root: copy both `[patch]` tables from this repository's
[Cargo.toml](../Cargo.toml), adjusting the compatibility crate's path to
`../host-tools/crates/idle-ssh-buffer-compat`. Keep their exact Git revisions and
commit the resulting lockfile. Instantiate `Authority` and `PeerCoordinator`
directly, or construct a `Service` with your authenticated principal and adapters.

Build from this checkout with the sibling EditChain checkout:

```sh
cargo build --locked -p idle-coordination --bin idle-coordination
```

The executable requires the platform's OpenSSL and system TLS libraries. It embeds
the engine worker; `--peer-worker` also exposes EditChain's existing worker IPC.
Node is needed only for development interoperability tests.

Create a trusted local configuration, replacing paths and stable IDs:

```json
{
  "state_directory": "/home/alice/.local/state/idle/repository",
  "chain_directory": "/home/alice/project/.editchain",
  "device_directory": "/home/alice/.local/share/idle/device",
  "workspace": {
    "id": "workspace-project",
    "chain": "chain-project",
    "name": "Project",
    "mode": {
      "kind": "standalone",
      "repository": {
        "id": "repository-project",
        "name": "Project",
        "remote": "https://github.com/example/project"
      }
    }
  },
  "contributor": {
    "contributor_id": "alice",
    "authenticated_as": { "issuer": "local-process", "subject": "alice" }
  },
  "runtime": null,
  "credential_variable": "IDLE_TUNNELS_GITHUB_TOKEN",
  "host_credentials": false,
  "discovery_repository": null,
  "resume_sharing": false
}
```

Existing Unix state directories must have owner-only permissions. Preserve the
state, chain and device directories and their IDs across restarts and adoption;
startup checks the saved workspace/chain/repository binding. `resume_sharing`
resumes retained approvals but leaves a completed Stop disabled.

Run `idle-coordination --config /absolute/path/service.json` with pipes connected
to your native client. Hosting reads the named credential variable for each
management request; guest connections use only their invitation grant. Embedding
hosts can implement `Credentials::renew`. The executable cannot renew guest grants
itself, so an expired connection needs a fresh invitation.

Editor hosts can set `host_credentials: true` and call `serve_configuration`.
The native service then requests fresh authorization on its private process pipe,
including during startup cleanup, command execution and background renewal:

```json
{"kind":"credential","data":{"id":"1","purpose":"management"}}
```

The host replies on stdin with the same ID and a token, or `null` for denial:

```json
{"kind":"credential","data":{"id":"1","token":null}}
```

The two purposes are `management` and `discovery`; the host selects and rechecks
the required account and permissions separately. Tokens are limited to 64 KiB;
at most eight callbacks can be pending, with a twenty-second deadline. EOF and
cancellation retire their slots. Hosts must service these callbacks while waiting
for ordinary responses. The standard `service::Client` is for connections using
injected or environment credentials; an editor provides the multiplexed host
adapter. Tokens stay out of configuration files, process arguments and status.

## Framed API

`service::Client<R, W>` works with asynchronous pipes or authenticated socket
halves. `service::serve` is available to embedding hosts. Each frame is a four-byte
big-endian unsigned byte length followed by UTF-8 JSON, limited to 16 MiB. The
service accepts up to eight outstanding calls and serializes their execution.
`timeout_ms` bounds client request writes and gives the server a cooperative
execution budget of 1 to 60,000 milliseconds. Cancellation includes queued calls;
the client allows fifteen seconds to send cancellation and receive cleanup results.
An interrupted write invalidates the client connection.

The JSON payload of a version query is:

```json
{
  "kind": "call",
  "data": {
    "version": 1,
    "id": "query-1",
    "timeout_ms": 30000,
    "command": { "kind": "versions" }
  }
}
```

Responses have `version`, the original `id`, and `result: {"Ok": value}` or
`result: {"Err": "fixed_error_code"}`. Cancel a call with
`{"kind":"cancel","data":"query-1"}`. It has no separate response: wait for
the original call's outcome after cleanup. A mutation's durable request key is
independent of this connection-local ID. EOF, Ctrl-C and SIGTERM suspend sharing,
drain transports/workers and preserve consent. Use `stop` for durable shutdown
and deletion of owned relays. On a broken pipe, reopen and reconcile mutations
through their original keys.

| Command kinds | Payload / result |
| --- | --- |
| `snapshot`, `catch_up`, `request_status` | Authorized state, invalidations or original outcome. Catch-up takes `{after, limit}`. |
| `mutate` | `Request<Mutation>` to `Response<MutationResult>`; inspect its typed domain result as well as the service result. |
| `check_access`, `validate_control` | Exact resource scope or controller fence; current authorization only. |
| `presence`, `publish_presence`, `remove_presence` | Fresh entries, one attributed entry, or its connection ID. |
| `sharing_status`, `join_request` | Credential-free state or an encoded device join request. |
| `inspect_request`, `inspect_invitation` | Validate encoded text before presenting approval. Invitation results remain private. |
| `sharing_scope`, `devices` | Read the existing outgoing boundary and approved devices. |
| `import_sharing`, `import_cleanup`, `cleanup` | Import a prior private session, retain cleanup markers, or retry pending owned-resource deletion. |
| `host` | `{request, scope}` to a private encoded invitation. |
| `join` | `{invitation, scope}`; explicitly accept the invitation for this device. |
| `scope` | `"keep"`, `"all"` or `"from_now"`; drain workers before changing consent. |
| `resume`, `reconnect`, `suspend`, `stop`, `revoke` | Existing approval lifecycle; revoke takes an exact device fingerprint. |
| `configure_directory`, `discover` | Select an explicitly approved `owner/repository` (or `null` to disable), then refresh it. |
| `prepare_adoption`, `pending_adoption`, `finish_adoption` | Freeze for a named destination, inspect the package, or reconcile through an injected managed adapter. |

See [Command](../crates/idle-coordination/src/service/mod.rs), the
[repository schema](../crates/idle-protocol/schemas/standalone-v1.json) and
[configuration fixture](../crates/idle-protocol/tests/fixtures/configuration_write.json).
Invitations and successful host responses contain bearer grants; store them
privately. Debug output redacts tokens and service response payloads.

## Sharing and discovery

`PeerOptions` injects relay, credentials, persistence and clock adapters.
`DevTunnels` uses Microsoft's SDK directly. The bridge keeps reads moving during
blocked writes, bounds buffered output and allows at most eight active edges.
It enforces authentication deadlines, backs off transient failures with jitter,
and collapses duplicate routes by authenticated device identity. Shared
`idle-history` status reaches Live after durable inventory checks and reports
record/blob progress.

Invitation parsing preserves `editchain:` base64url version-one fields and
camelCase saved-state fields. It checks certificate fingerprints, the addressed
guest, expiration and Microsoft relay endpoints before enrollment. `keep`
retains an active scope exactly, including legacy exclusions; reconnect never
reselects a cutoff. `from_now` uses the engine's durable arrival boundary, not a
timestamp-only filter. Changes drain old workers first. Revocation drains known
connections and the engine rechecks approvals on every native turn.

Token renewal can only retain the same space, host certificate, guest, tunnel
and cluster. Host route and keys may rotate within that approved tunnel. Expired
tokens cannot trigger account-level guest access. Hosts renew their management
session before expiry and unregister old relay endpoints before reconnecting.
Version or authorization failures stop retrying; transient failures back off.

Cloud creation records an owner marker before contacting the service. Cleanup
verifies the exact resource and marker before deletion. Stop drains connections
even if its journal fails, and clears saved sharing only after recording the stop.
Failed deletion is retried on reopen; suspension retains the relay for resumption.
Uncertain creates remain recorded through the expiry window. Cleanup failures are
reported; a crash may leave a relay until restart or server-side expiration.

A host migration passes version-one saved sharing over the private pipe with
`import_sharing`. The coordinator checks the existing engine identity, approved
devices and exact active scope before persisting it. Import itself leaves sharing
disabled. A durable receipt makes retries idempotent even after revocation or
Stop, so a lost acknowledgement cannot restore older grants. The previous host
removes its copy only after acknowledgement. `import_cleanup` durably transfers
validated resource markers; a retained host is excluded from cleanup even before
its marker has an exact resource locator. Ambiguous marker lookup is refused.

`sharing_status` includes native connection IDs, durable-change counters and the
last configured directory outcome. These public fields support host status views
without exposing invitations or credentials. A suspended owner performs no new
directory reads.

Optional `discovery_repository` uses GitHub Actions repository variables, with
suitable GitHub permissions, refreshed every minute. Protocol-3 `EDITCHAIN_PEER_`
entries contain no token and refresh only approved routes within the same tunnel
and cluster. Discovery cannot enroll devices, change consent or renew grants.

## Managed adoption and f15/f17

Preparation suspends peers, freezes mutations, retires the controller lease and
advances its epoch. Transfer preserves workspace/repository/chain/session IDs,
metadata revisions, grants, views and request outcomes. The consent summary
records scope and approved certificates; the complete scope ledger and exclusions
must stay in the same engine directory. Adoption preserves absent or inactive
consent and never creates a sharing approval.

`ManagedAdoption::import` must authenticate the destination, import atomically,
deduplicate the transfer ID and refuse conflicting identity bindings. The
acknowledgement must match the exact package hash, destination, workspace and
chain, retaining at least the transferred epoch. Lost acknowledgements retry the
same frozen package after restart. A bad acknowledgement leaves it frozen.
Completed adoption reopens as managed and leaves local mutations disabled.
Embedding hosts supply the managed adapter and credentials; the standalone
executable has neither.

The f15/f17 integration binds Evo's authenticated runtime, rechecks `check_access`
and `validate_control` at execution, and keeps resource grants separate. It must
preserve input request identity and report acceptance/order/completion from Evo.
Runtime execution, sandbox policy and live Evo integration remain in f15/f17.

## Coordinated upgrades

| Boundary | Current version / source |
| --- | --- |
| Repository requests and additive schema | API `"1"` |
| Native service framing | 1 |
| Invitations and saved sharing | 1, unchanged TypeScript shapes |
| Public discovery / TypeScript outer protocol | 3 |
| EditChain peer protocol | 5 |
| Managed transfer | 1 |
| Microsoft Dev Tunnels | `bb2a7dbdc56312b01b86be6eb8ce9cda7bb932a2` |
| Microsoft SSH fork | `0bced23016d869dc847b25840268896fd0969b69` |

Query `versions` before opening sharing. Unknown versions fail closed. Preserve
existing invitation, saved-state and engine scope encodings; a future incompatible
change requires coordinated Rust/TypeScript rollout and explicit migration, never
deleting saved consent to reconnect. Regenerate schemas and review fixture changes
with every public contract change.

The root Cargo patches follow Microsoft's OpenSSL SSH fork. The fork's old secret
buffer is replaced by the small private [compatibility crate](../crates/idle-ssh-buffer-compat/README.md),
which delegates allocation, checked growth and zeroization to fixed upstream
`russh-cryptovec` 0.60.3. The fork still has the older SSH-agent framing path
described by RUSTSEC-2026-0154; these adapters never open agent sockets or expose
agent forwarding. They also use uncompressed SSH channels. Do not enable those
paths without updating the fork to include the upstream fixes. No advisory is ignored.
The SDK's repository MIT license is recorded by a content-hashed clarification.

Upgrade SDK, SSH fork, compatibility wrapper and lockfile together. Review host
key verification, endpoint cleanup and connection backpressure; then run the
whole check and live smoke below. CI uses the same lint entrypoint and pinned
EditChain worker checkout as local checks.

## Verification

`./scripts/lint.sh` runs repository quality gates, protocol fixture/schema tests,
Rust peer replication with blobs and live appends, TypeScript interoperation,
scope/revocation/renewal/cancellation cases, authority retries and fencing,
native process restart, and managed handoff with a lost acknowledgement. The
managed tests use a deterministic injected adapter; they do not claim a deployed
managed service accepted a transfer.

`./scripts/check.sh` adds builds, existing collector/package smoke checks and all
18 pinned SDK unit tests via `scripts/check-tunnels.py`. The SDK's Ed25519 test
requires its optional `rs-crypto` feature, enabled only in a temporary test build;
the shipped dependency graph uses OpenSSL. No SDK tests are removed or skipped.

The optional live test creates temporary private Microsoft relays, checks native
authentication/inventory, reconnects with a changed route, restarts the guest
service, and deletes owned relays:

```sh
cargo build --locked -p idle-coordination --bin idle-coordination
python3 scripts/smoke-tunnels.py --github-auth
```

Alternatively set `IDLE_TUNNELS_GITHUB_TOKEN` privately and omit `--github-auth`.
The script never prints the token or invitation. If deletion fails it retains
private recovery state and reports its path; reopen that configuration and issue
`stop` to finish cleanup. The live test checks the Microsoft transport; durable
record/blob replication is separately tested through injected transports against
real Rust and TypeScript engine workers.
