# Native repository coordination

`idle-coordination` provides a Rust library and `idle-coordination` executable.
They run without Node, app-core, VS Code or a managed backend. EditChain handles
peer-v5 TLS authentication, approved device membership, inventories, durable
records and blobs. The coordinator handles consent, relay lifetime, retries,
discovery and shared `idle-history` connection status.

## Authority and authentication

One `Authority` owns a repository's metadata, settings/rules, view definitions,
memberships, resource grants and controller lease. `FilePersistence` exclusively
locks its private directory and commits with compare-and-swap, atomic replacement
and filesystem synchronization. A second local owner is rejected. Injected
`Persistence` adapters must provide the same atomic, durable guarantees.

The authority is a single writer. Peer history replication does not replicate
metadata authority state or elect controllers independently on each peer. An
embedding host must route collaborators' metadata operations to this authority
and authenticate every `Principal` outside the supplied JSON. The native stdin
service trusts the process owner and binds one principal at startup. It does not
open a network listener. An adapter exposing it over a socket or remote API must
supply authentication and connection isolation itself.

`Request<Mutation>` retains the existing protocol envelope, exact contributor,
workspace, immutable request key, deadline and optional control fence. Conditional
writes require `Absent` or the exact current revision. Settings and agent rules
have separate revisions and preserve complete JSON text, including unknown fields.
Saved views contain definitions; projection results are still derived elsewhere.

Successful writes and definite refusals retain their original results. Retrying
the same key with a changed envelope or operation is a conflict. Requests expire
within 24 hours; up to 4,096 unexpired outcomes are retained, and capacity is
refused instead of dropping active retry identities. A storage failure may have
committed: the authority faults until reopened, after which `request_status`
resolves the original result. Do not mint a new key for an uncertain request.

Membership grants metadata participation. It does not grant host execution,
session input or provider use. Those resource permissions are checked separately;
membership revocation overrides them. Snapshots filter sessions, hosts, providers
and grants for the current contributor. Recovery retains 256 ordered invalidations;
membership/grant changes retire the visibility generation, and missing history
requires a replacement snapshot. Presence is transient, attributed to the
authenticated contributor, validated against the repository and visible hosts,
and expires within 120 seconds.

Controller acquisition/renewal checks the authenticated runtime, host, Control
session and relevant grants. Epochs increase durably; leases last at most 60
seconds. Restart retires the old lease while retaining its watermark. Consumers
must revalidate the exact fence with `validate_control` immediately before use.

## Native process

For a library consumer, add a path dependency on
`../host-tools/crates/idle-coordination`. Cargo applies patches only from the
consuming workspace root: copy both `[patch]` tables from this repository's
[Cargo.toml](../Cargo.toml), adjusting the compatibility crate's path to
`../host-tools/crates/idle-ssh-buffer-compat`. Keep their exact Git revisions and
commit the resulting lockfile. Depending on the library alone does not propagate
those workspace patches. Instantiate `Authority` and `PeerCoordinator` directly,
or construct a `Service` with your authenticated principal and adapters.

Build from this checkout with the sibling EditChain checkout:

```sh
cargo build --locked -p idle-coordination --bin idle-coordination
```

Distribute the resulting executable with the platform's OpenSSL and system TLS
requirements. Node is used only by development interoperability tests. The native
library embeds the engine worker; it needs no separate worker executable. The
same binary's `--peer-worker` entrypoint exposes EditChain's existing worker IPC
for hosts that already use that interface.

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
  "discovery_repository": null,
  "resume_sharing": false
}
```

The service creates its state directory privately; existing Unix directories
must already have owner-only permissions. Preserve that directory, the chain and
the device directory across restarts. The workspace/chain/repository binding is
validated against saved metadata. Sharing scope and approvals remain in the
existing engine store. Do not generate new IDs while retrying or adopting it.

Run `idle-coordination --config /absolute/path/service.json` with pipes connected
to your native client. No token is stored in the startup configuration. Hosting
reads the named environment variable for each management request. Guest
connections use their invitation grant and never fall back to owner credentials.
The executable's environment adapter cannot obtain a renewed guest grant itself;
an embedding host may implement `Credentials::renew` against its approved grant
issuer. An unavailable renewal leaves that connection expired.

## Framed API

`service::Client<R, W>` works with asynchronous pipes or authenticated socket
halves. `service::serve` is available to embedding hosts. Each frame is a four-byte
big-endian unsigned byte length followed by UTF-8 JSON, limited to 16 MiB. The
service accepts up to eight outstanding calls and serializes their execution.
`timeout_ms` is a cooperative execution budget, from 1 to 60,000 milliseconds;
cleanup may extend response time. Cancellation also applies to queued calls.

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
| `host` | `{request, scope}` to a private encoded invitation. |
| `join` | `{invitation, scope}`; explicitly accept the invitation for this device. |
| `scope` | `"keep"`, `"all"` or `"from_now"`; drain workers before changing consent. |
| `resume`, `reconnect`, `suspend`, `stop`, `revoke` | Existing approval lifecycle; revoke takes an exact device fingerprint. |
| `discover` | Refresh the explicitly configured directory. |
| `prepare_adoption`, `pending_adoption`, `finish_adoption` | Freeze for a named destination, inspect the package, or reconcile through an injected managed adapter. |

See [Command](../crates/idle-coordination/src/service/mod.rs), the
[repository schema](../crates/idle-protocol/schemas/standalone-v1.json) and
[configuration fixture](../crates/idle-protocol/tests/fixtures/configuration_write.json).
Invitations and successful host responses contain bearer grants; store them
privately. Debug output redacts tokens and service response payloads.

## Sharing and discovery

`PeerOptions` injects relay, credentials, persistence and clock adapters.
`DevTunnels` uses Microsoft's SDK for management, host and client connections;
it does not invoke Node, a tunnel CLI or a local TCP forwarding listener. The
native bridge uses bounded I/O, authentication deadlines and at most eight
active edges, then backs off transient failures with jitter. Simultaneous routes
collapse by authenticated device identity. Status advances to Live only after
the engine's durable inventory checks, and exposes record/blob progress.

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

Cloud creation is journaled by an unguessable owner marker before contacting the
service. Cleanup reconciles uncertain creation and verifies the exact saved
resource identity plus marker before deletion. Stop is durable before saved
sharing is cleared; a failed deletion remains pending and is retried on reopen.
Suspension retains an owned relay for resumption. Unknown creates are retained
for reconciliation through the resource's expiry window. Explicit teardown
reports cleanup failures; a process crash may leave a relay until restart or
its server-side expiration.

Optional `discovery_repository` enables GitHub Actions repository variables and
requires suitable GitHub permissions. A configured service refreshes them every
minute. Entries retain `EDITCHAIN_PEER_` naming and protocol 3. They contain no
connect token. Discovery only refreshes an existing approved certificate's route
within the same tunnel and cluster; it cannot enroll a device, broaden consent or
renew a grant. Presence and directory freshness never imply execution authority.

## Managed adoption and f15/f17

Preparation suspends local peers, freezes new mutations, retires the controller
lease and advances its epoch. The durable transfer retains workspace, repository,
chain and session IDs, metadata revisions, grants, view definitions and original
request outcomes. Its consent summary records the engine's current scope and
approved certificates. The complete scope ledger and record exclusions remain
in the same engine directory: the summary cannot initialize an equivalent new
store. Adoption never reconfigures sharing or rewrites that ledger.
Absent or inactive consent is retained as such; adopting metadata never creates
a history-sharing approval.

`ManagedAdoption::import` must authenticate the destination, import atomically,
deduplicate the transfer ID and refuse conflicting identity bindings. The
acknowledgement must match the exact package hash, destination, workspace and
chain, retaining at least the transferred epoch. Lost acknowledgements retry the
same frozen package after restart. A bad acknowledgement leaves it frozen.
Completed adoption reopens as managed; the old local authority cannot accept new
mutations. The standalone executable has no managed adapter or implicit backend
credentials. Embedding hosts supply the real adapter when endpoints exist.

The f15/f17 integration must bind Evo's authenticated runtime and route execution
through current `check_access` and `validate_control` calls. Recheck expiry,
revocation and epoch at execution, keep session/compute/provider grants separate,
retain input request identity, and report runtime acceptance/order/completion
from Evo. This service implements no runtime execution, sandbox policy or fake
acceptance. Integration with live Evo grants and leases remains in f15/f17.

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
