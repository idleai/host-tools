# Native collection

The collector library and executable share source discovery, workspace selection,
32-source batches, durable source cursors, Git reconciliation and history-change
monitoring. A host supplies an explicit binding containing absolute `workspace`,
`chain`, `sessions` and `helper` paths. The helper is an installed Codex exporter.

## Library

Construct `Collector::new(binding)` and call `scan(Mode::Import)`. Continue while
`Update.pending` is true, then wait before the next scan. `Mode::Observe` skips
provider sources while retaining history notifications and Git-link refreshes.

Source stamps advance only after the complete bounded import succeeds. Pending
batches retain their original stamps, so an append during capture is revisited.
Failed sources remain retryable; a source removed before retry does not block
other collection. Title updates revisit all sources across bounded batches.
Discovery handles linked worktrees, nested and bare repositories and ignores
source-directory symlinks.

The existing `poll(&Poll)` API remains available to hosts that explicitly supply
source paths. An empty poll observes editor/peer writes and late blobs even when
a provider import fails. The importer validates the projected workspace before
committing source data.

## Framed service

Pass one binding JSON argument to `idle-history-collector`. Requests and replies
use a four-byte little-endian length followed by JSON. Each request has an `id`
and `body`; its response repeats the ID and returns `Ok(Update)` or `Err(String)`.

The new discovery request is `{"id":1,"body":{"scan":"import"}}`; use
`"observe"` to pause source import. The existing explicit request shape remains
`{"id":2,"body":{"paths":[],"git_changed":false}}`.

The service exits when its input closes. The VS Code adapter owns its timer,
trust checks, settings and request cancellation; it sends scan requests and
forwards bound history invalidations. It contains no duplicate source-discovery
or source-checkpoint algorithm.

## Standalone process

Run `idle-history-collector --watch BINDING_JSON` to collect without a UI host.
The binding has the same four paths. The process discovers sources and polls
history once per second, draining pending batches immediately. It writes JSONL
updates and errors to stdout. An import failure still permits unrelated history
notifications and is retried on the next tick. SIGINT/SIGTERM request shutdown;
committed operations and cursors remain available after restart.

The process runs in the foreground so a caller or OS supervisor owns its
lifetime. It requires no VS Code or Node runtime. Node is used only by repository
checks and the separate TypeScript peer coordinator. Local socket routing and
the full Evo coordination service remain separate work.

## Ownership and source contract

Automatic import holds an OS file lock at `CHAIN/collector.lock` after a chain
exists or source work begins. A second automatic collector reports that another
collector owns the chain and can still observe its history. Pausing import or
dropping the collector releases ownership. Manual imports, editor capture and
replication continue using the engine's serialized durable writes.

The lock file is retained on disk; the OS lock determines ownership. It is not a
PID file and needs no stale-file deletion after process failure.

Active sources retain the append-log contract: replacement, truncation and
same-size changes trigger recapture. A larger in-place rewrite is validated on
restart. Source records, record identities and archive formats are unchanged by
the package move.
