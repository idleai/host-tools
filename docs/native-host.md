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
`collection`, `repository` and `coordination`. Clients validate these before
opening channels. The optional `features` array includes `repository.local`,
which permits repository reads with `local_only: true`. Clients must check this
feature before sending that field to an older host. Missing features are unsupported.
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
