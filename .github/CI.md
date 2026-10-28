# CI and dependency inputs

Fresh PR, main, and manually started builds resolve the latest compatible
internal Cargo packages and complete native or consumer releases. The committed
`Cargo.lock` supplies the initial third-party selection. CI records the selected
lockfile, release tags, and checksums once, then restores that record before
computing Rust cache keys. All checks, packaging, and release verification reuse
the same recorded selection, even if another release appears while they run.

After an upstream release finishes publishing, start a new downstream build or
rerun the entire workflow to adopt it. Retrying only failed downstream jobs reuses
the original dependency artifact. Compatible releases need no dependency-update
commit; a new API outside the declared version range still needs a manifest
change.

The optional `dependency_mode=locked` input replays the committed `Cargo.lock`
and `.github/released-dependencies.lock.json`. Supplying a prepared dependency
artifact always restores that exact selection instead of resolving again.
Release retries retain their prepared selection, and release version commits
retain the tested inputs for explicit replay. Local build commands also resolve
once unless `IDLE_RELEASE_INPUTS` supplies an existing record.

The release watcher checks every six hours and starts main CI when the selected
releases differ. This integration run uses the background workers and the full
auxiliary checks. Fresh PR builds resolve immediately.

Hosted preflight checks run before compilation. Windows dependency-script checks
run when their scripts, artifact definitions or shared build inputs change, and
for explicit full/release runs. The main Rust and WASM checks remain in ordinary
CI. Clippy checks all targets; the ordinary Cargo test invocation also runs
doctests, so neither needs a second equivalent command.

`CI required` is the single merge result. Failed, cancelled, and unexpectedly
skipped jobs fail it. Only checks explicitly excluded by the path selector may
be skipped. Internal PRs use `IDLE_LINUX_RUNNER`; main, release and integration
runs use `IDLE_BACKGROUND_RUNNER`. Unset variables select hosted Linux. Busy
local runners queue work rather than automatically overflowing to hosted jobs.

Version commits are staged on `ci/release/<source>` before release verification.
The publish job records the successful `CI required` check on that exact commit
and advances `main` with a normal fast-forward push. Failed validation or a newer
main commit prevents promotion. The merge rules therefore need no bot bypass.

The repository's `CI required` ruleset accepts this check only from GitHub
Actions, without requiring a fresh base-branch rebuild for every merge. Activate
it after the release promotion workflow above lands on the default branch.
