# Standalone repository reads

`idle-repository` supplies local Git details, GitHub repository data and recorded
session items without a running agent or managed service. Hosts install one
immutable workspace/repository/chain binding and absolute checkout/history paths.
The shared snapshot types live in `idle-protocol::v1::repository`.

`Reader::read_local` returns the checkout and recorded sessions without HTTP or
credentials. A configured GitHub source is reported as still loading. Hosts can
display this initial snapshot while requesting complete data on a separate
channel; local startup does not queue behind an in-progress GitHub read.

Git commands run in the selected folder without fetching or changing the worktree.
The reader removes ambient `GIT_*` overrides, disables external fsmonitor and
optional locks, and limits each command to five seconds and the Git read to ten.
It reports the resolved checkout, separate worktree/shared Git directories,
branch, full HEAD, sanitized remote, changed-path counts and authors from at most
500 commits. Unborn and detached checkouts, linked worktrees, missing paths and
non-Git folders have distinct results. A failed source never silently selects a
different folder.

Only a normal `github.com` remote enables GitHub REST reads. The API origin is
fixed to GitHub's HTTPS endpoint; redirects are rejected and pagination constructs
its own next page. Reads cover repository metadata, contributors, accessible
collaborators, open issues/pull requests, requested reviewers, and checks/workflow
runs for the checkout's HEAD. Each list has at most two pages of 50, each response
is limited to two MiB, and the complete GitHub attempt has a 20-second deadline.
Reports retain check times, declared bounds, permission failures and retry times.
An inaccessible collaborator list does not turn Git authors into collaborators.

The file-owning host supplies the current account and token over framed standard
input. Tokens are never process arguments, UI data, saved preferences or retained
API records. Conditional pages are scoped to repository, account and token.
Automatic updates may reuse a read for 60 seconds while preserving its original
check time and removing its currentness claim. Explicit refresh revalidates the
service. Account changes, rejected authentication and failed requests cannot fall
back to an earlier private response. Rate-limit replies establish a local retry
deadline; even manual refresh respects it.

GitHub fields map into four independent shared projections:

| View | Supplied GitHub fields |
| --- | --- |
| Tasks | Open issues and pull requests, deduplicated by source page |
| Errors | HEAD checks/runs with `failure`, `timed_out` or `startup_failure` conclusions |
| Triage | Labels `triage`, `needs-triage` or `needs triage`, case insensitive |
| Needs input | Labels `needs-input`, `needs input`, `needs:input`, `needs-human-input`, or explicit requested reviewers/teams |

Every projected row links to a retained `Original` containing the exact response
bytes and to its HTTPS source page. Projection rows over eight KiB are omitted
with partial coverage; the complete response remains in history. Before showing
rows, the native history reader verifies their full observation/item IDs and
stored hashes against the accepted chain. Missing, changed or quarantined sources
remove only affected rows and add gaps. Original bytes must also resolve and
match their recorded hash; missing or corrupt blobs reduce coverage even while
their operation record is still accepted. Each Original is checked once per
replacement and checked again on later reads. Activity remains an independent engine
read if the repository adapter fails. GitHub categories do not imply controller
state or Idle permissions.

Recorded sessions group up to 1,000 session observations by full logical item ID.
A second bounded scan includes older encodings that lack the current Kind index.
Distinct recorded label previews, lifecycle events and source records remain visible;
these do not establish that an agent is running. Missing content, scan limits and
quarantined records produce partial coverage. Selecting a session applies its full
logical ID to history. Hosts retain selection per contributor, binding and surface;
the application validates a restored choice against the current session list.

The `idle-repository` binary accepts the binding as its sole JSON argument. Requests
and replies use four-byte little-endian length-prefixed JSON:

```json
{"id":1,"body":{"credentials":null,"refresh_github":false}}
```

The optional `local_only: true` field selects the local read. It defaults to false,
so existing requests still include GitHub. The shared native host advertises
`repository.local` in its handshake before clients may use this field.

Replies carry the same `id` and `body: {"Ok": {"repository": ..., "projections": ...}}`
or a safe `Err` message. Requests are limited to 16 KiB and replies to eight MiB.
The host cancels individual waiters, stops a read when its last waiter closes,
and retires the process on trust, account or binding changes.
