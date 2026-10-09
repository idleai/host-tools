# Shared native host

`idle-host` links capture, history, collection, repository and coordination
libraries into one supervised process. The VS Code extension owns one process
per extension host, including Remote SSH and container hosts. Each workspace
installs independent service channels with absolute storage paths. No service
uses a process-wide current directory. The collector can launch the separately
packaged Codex exporter; Git adapters can still invoke the system Git command.

The existing standalone executables remain available for command-line consumers
and compatibility tests. VS Code packages `idle-host` and the Codex exporter.
Library boundaries and saved storage formats remain the same.

## Private pipe protocol

The executable takes no configuration arguments. Its stdin/stdout connection
belongs to the local application that launched it. Configuration and credentials
must never be accepted from a webview or a network client as host installations.

Each frame starts with a four-byte unsigned little-endian length, excluding that
length prefix. The frame begins with this eight-byte routing header:

| Offset | Length | Field |
| --- | --- | --- |
| 0 | 1 | Protocol version, currently `1` |
| 1 | 1 | Message kind |
| 2 | 2 | Reserved; must be zero |
| 4 | 4 | Unsigned little-endian channel identifier |
| 8 | Remaining | Original UTF-8 service payload, or empty control payload |

| Kind | Direction | Meaning |
| --- | --- | --- |
| `0` Hello | Native → application | Channel zero; protocol and service versions |
| `1` Open | Application → native | Install an immutable workspace service binding |
| `2` Data | Both | Unchanged service request, reply or credential exchange |
| `3` Close | Application → native | Cancel and retire one channel |
| `4` Ready | Native → application | Empty acknowledgement that the channel was admitted |
| `5` Closed | Native → application | Worker has ended, or installation was rejected; fixed `code` only |
| `6` Shutdown | Application → native | Empty payload on channel zero; retire the host |

Hello advertises `version: 1` and version `1` for `capture`, `history`,
`collection`, `repository`, `coordination` and `runtime`. Clients validate the required services before
opening channels. The optional `features` array includes `repository.local`,
which permits repository reads with `local_only: true`. Clients must check this
feature before sending that field to an older host. Missing features are unsupported.
The `runtime.workspace` feature enables the optional runtime channel described below.
The `coordination.runtime-owner` feature adds transfer from a local coordinator
to a daemon-owned coordinator through that channel.
Channel IDs are positive and increase for the process lifetime;
closed IDs cannot be reused. Open contains:

```json
{
  "workspace": "/absolute/workspace",
  "service": {
    "kind": "capture",
    "binding": {
      "workspace_path": "/absolute/workspace",
      "chain_dir": "/absolute/workspace/.editchain"
    }
  }
}
```

Each service uses its existing Rust binding and request/reply JSON. Capture,
history, collection and repository keep their `{id, body}` envelopes.
Coordination keeps its versioned call, cancel, reply and credential messages;
only its standalone stream length prefix is omitted inside Data. This is a
custom framed protocol, not JSON-RPC 2.0. The router never parses or rewrites
service JSON, preserving exact captured records and full Rust integer values.

The history channel also accepts `{ "capabilities": true }`. Its response body
is `{ "Ok": { "timeline": 2, "operation_json": true } }`. Consumers check this
before sending `QueryAction::Timeline` or an `OperationJson` native action. The
outer history channel version and existing raw queries remain supported.

Timeline version 2 distinguishes recorded operation addresses from Git commit
destinations. The host may install a trusted `repository_directory` with the
history binding. This enables read-only HEAD history and immutable commit/patch
documents; requests carry the repository identity and full OID, never a path.
Existing history bindings without that directory continue serving recorded data.

## Ownership and limits

Requests execute in order within a channel. Filesystem services have independent
blocking workers; repository and coordination work uses asynchronous workers.
An invalid service request closes its channel. Invalid routing headers terminate
the connection. EOF, Shutdown and process termination cancel all workers.
Close acknowledgement follows worker teardown, so clients can safely reopen the
same storage binding after receiving it. Collection explicitly releases its
ownership lock on pause and drop, including when a child inherited a file handle.

The host admits at most 64 channels. Open is limited to 64 KiB; routed payloads
to 160 MiB. Service request/reply limits still apply. Input and output each have
a shared 192 MiB byte budget, with 16 queued inputs per channel and 64 queued
outputs. Full input queues retire the affected channel. A full output byte budget
fails the sender; a stalled pipe applies backpressure. Large frames can delay
other channels on the same pipe. Service shutdown has an eight-second grace
period, followed by a two-second output drain.

Local contributor metadata and account-authenticated sharing retain separate
coordinator bindings, credentials, state directories and consent. Combining
processes does not combine those authorities. Credentials remain channel scoped,
are requested only when needed, and are never put into command arguments or
host diagnostics. A host crash affects all its channels. The extension rejects
pending requests, waits for the old OS process to exit, then reopens requested
channels using fresh identifiers and their existing durable state.

## Codex compute connections

The optional `runtime` service takes a private `idle-runtime:` invitation and
the expected `workspace_id`, `repository_id`, `chain_id` and `client_id`. It
checks those identities before opening a Dev Tunnels connection on runtime
port 43189. History sharing continues using its existing port. Runtime calls
use the coordination call/cancel envelope with `command: {"kind":"status"}`.
Successful replies contain the daemon's status for the approved binding. A
version 2 invitation with `coordinationOwner: true` also permits
`command: {"kind":"coordination","request":{"kind":"status"}}`. The
application passes raw command JSON, and the native adapter embeds the request
as a JSON string in `idle/coordination/call`, preserving native integer values.
Version 1 invitations continue to permit attachment and status only.

The service authenticates the daemon-issued grant, initializes the native
Codex app-server protocol, selects the approved attachment and reads status.
It verifies the host identity and exact workspace binding on replies.
Invitations stay in the application host's secret storage. The runtime service
cannot obtain management credentials or use an invitation for another local
workspace. Cancellation retires an in-flight exchange before reconnecting.

On the compute machine, Codex owns a separate `idle-host --runtime-relay`
process. That mode uses one-MiB bounded, big-endian JSON frames on private
stdin/stdout pipes. A local `start` command supplies the private state directory
and the absolute path to an owner-selected GitHub CLI. The helper hosts and
renews its Dev Tunnel, forwards opaque frames, and journals its owned tunnel
for restart and cleanup. Codex authenticates each incoming runtime grant and
applies the workspace and method restrictions.

EOF suspends hosting without deleting the tunnel; `stop` with `remove: true`
removes it. Closing an editor only closes its client channel. It does not stop
the daemon or its helper. See the companion Codex CLI's `app-server idle`
commands for setup, invitations and revocation.

## Daemon-owned coordination

Codex starts one `idle-host --runtime-authority` child per workspace. Its private
stdin/stdout channel uses one-MiB bounded, big-endian JSON frames. The local
daemon supplies the trusted workspace identity, checkout root, chain and private
state directory. Remote requests cannot choose those paths. The helper holds an
exclusive state lock until the daemon closes its pipe and restores accepted
coordination state when it starts again.

The original authenticated contributor can transfer a local coordinator using
`prepare_runtime_transfer`, `runtime_transfer_chunk` and
`complete_runtime_transfer`. Preparation requires matching tracked definitions,
retires the active control lease and durably freezes local writes. The package
retains the contributor identity, access grants, revisions, cursors and write
results. Transfer packages are limited to 16 MiB and uploaded in chunks of at
most 64 KiB. The destination validates the entire package and installs its state
and receipt atomically. Repeating a commit returns the original receipt without
overwriting later writes. Source state uses format version 2 so older helpers
reject it instead of resuming local ownership.

After transfer, the editor routes coordination calls to the daemon. The original
contributor is bound to the owner invitation's client ID; another client cannot
claim the imported coordinator. The gateway supports workspace configuration,
metadata, access grants, control leases and peer activity. History sharing and
managed coordinator adoption remain separate. Revocation and grant expiry are
checked by Codex before every request, including queued requests. Disconnects
do not unlock the daemon's coordinator or permit a local fallback. Requests are
not automatically replayed after a helper failure; callers recover mutations
using their existing request IDs.
