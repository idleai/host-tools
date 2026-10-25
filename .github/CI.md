# CI and dependency inputs

Ordinary pull requests use the committed `Cargo.lock` and
`.github/released-dependencies.lock.json`. CI records those inputs once, checks
their requirements and checksums, and restores the record before computing Rust
cache keys. A retry of the same source uses the same dependency selection.

To update dependencies deliberately, run
`python3 scripts/release_dependencies.py pin`, review the Cargo lockfile and
artifact selection together, and commit them. Local build commands retain their
existing release-resolution behavior unless `IDLE_RELEASE_INPUTS` is set.

The release watcher checks every six hours and starts CI with
`dependency_mode=latest` only when the selected releases differ. This integration
run uses the background workers and the full auxiliary checks. Release
verification reuses the exact prepared dependency record. Release version
commits also retain the tested artifact selections for subsequent PRs.

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
