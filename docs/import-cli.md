# History import tools

`idle-history-tools` owns application history import, reconciliation and conversion.
Build it from this repository with `cargo build -p idle-history-tools --locked`.
The engine CLI handles generic operation queries, archives and storage migration.

## Importing archives

Provider `--input` accepts a file, directory or stdin. Give stdin
a stable `.jsonl` filename for resumable imports; Codex names start with `rollout-`.

```sh
cat session.jsonl | idle-history-tools import --provider human --input - --source-name session.jsonl
idle-history-tools import --provider codex --input /archive/codex \
  --workspace /original/repository --codex-helper codex-session-exporter
```

`--workspace` supplies source discovery context; `--recorded-root` filters human
archives by their recorded root. `--dry-run` previews a full capture without
changing storage or cursors, `--raw-only` skips normalization, and
`--include-thinking` includes private reasoning. Cursors advance after durable
admission; incomplete final lines wait for the next import. Human capture retains
original bytes and identity links; editor capture runs through the Idle editor adapter.

For large directories, add a quoted `--glob` (repeatable) or `--bulk` to capture
and commit one file at a time while keeping one writer open:

```sh
idle-history-tools --chain /tmp/history import --provider codex \
  --input /archive/codex --glob '**/*.jsonl' \
  --workspace /original/repository --codex-helper codex-session-exporter --progress
```

Globs filter the provider's normal discovery results. Relative globs are relative
to `--input`; absolute globs are also accepted. Overlapping globs select a file
once. Keep the same input root across retries so provider-relative source IDs
remain stable. No matches are an input error before storage is opened.

Use `--manifest sources.json` instead of `--input` and `--provider` to import
multiple providers or recorded workspaces in one process:

```json
{
  "schema": 1,
  "sources": [
    {"provider": "claude", "input": "cc", "glob": ["**/*.jsonl"]},
    {"provider": "codex", "input": "codex", "glob": ["**/*.jsonl"],
     "workspace": "/original/repository"},
    {"provider": "human", "input": "human", "glob": ["**/*.jsonl"],
     "recorded_root": "/original/repository"}
  ]
}
```

```sh
idle-history-tools --chain /tmp/history import --manifest sources.json \
  --codex-helper codex-session-exporter --progress
```

Manifest inputs are directories, resolved relative to the manifest's directory.
Each source can additionally specify `paths`, an exact list relative to its input
root; globs then filter that list. `workspace` defaults to the CLI's `--workspace`.
Provider identity stays explicit, including when a manifest mixes providers.

Bulk runs commit after each file. A failure can leave earlier files committed;
rerun the same command to resume. Existing source and capture limits apply per
file (by default 1,000,000 captured operation variants and 512 MiB encoded bytes),
so an individual oversized file still fails. Memory used for the current capture
is bounded by those limits; the writer also retains admission state for the chain.
Bulk `--dry-run` streams one capture record per file followed by a summary, as a
JSON array or JSONL, without creating the destination. Capture reports describe
each file independently; the summary counts exact duplicates and conflicts
across all captured files and manifest sources, retaining admission state in
memory. Preview uses fresh capture state and does not read destination cursors
or operations. Its summary reports zero writes and uses the same exit codes
for malformed input and conflicts as durable import. Successful durable runs
emit one aggregate report with per-source counts and phase timings. `--progress`
writes completed-file progress to stderr; malformed input still returns exit
3 after processing all selected files, while conflicts return exit 4.

Imports keep payloads up to 16 MiB inline and store larger payloads as blobs.
Segments roll over at a 32 MiB target. When recapturing history imported with the
old 4 KiB cutoff, use a fresh destination; see [import compatibility](import-api.md#compatibility-and-checks).


## Inspecting imports

`idle-history-tools --chain PATH import-state --output json` reports selected
derivations, logical items, source copies and incomplete source coverage.

## Converting older activities

`idle-history-tools --chain SOURCE convert --destination TARGET` creates a
separate schema-three chain. See [the conversion contract](import-conversion.md).
