# Import contract recordings

These small deterministic inputs are committed so CI does not require private
session directories or a sibling Codex checkout. They are test recordings, not
transcripts of a real person's work.

- `claude/session.jsonl` was copied unchanged from the retired extension's
  `r10-renderer-session.jsonl` fixture.
  It records native event and tool IDs, parent relations and a file write/result.
- `human/session.jsonl` freezes the version-one archive format exercised by the
  human import tests: an attributed recorder, initial buffer, unsaved edit,
  input receipt and stop. Display attribution and complete envelope bytes remain
  in raw records.
- `codex/rollout-contract.jsonl` is a deterministic protocol fixture based on the
  exporter's existing session, response-item and tool lifecycle test records.
  It distinguishes the owning thread from its parent, carries a tool's two
  revisions, and includes an unknown raw record.
- `codex/projection.ndjson` is actual output recorded from the unchanged exporter
  on 2026-09-27 using the command below. The fixture helper replays that output
  through the captured physical line count; it does not implement Codex semantics.
- Each `identities.json` records the public `NativeMapping` values for the input,
  pinning full native IDs, raw operation IDs/hashes, output IDs and incarnations.
  Regenerating these to accommodate an identity change requires a versioned
  compatibility decision, not an automatic test update.

From the repository root, verify the exporter recording below. The comparison
omits only the recorded source filename, which predates the repository move:

```sh
cargo run --manifest-path ../codex/tools/codex-session-exporter/Cargo.toml --locked -- \
  crates/idle-history-import/tests/fixtures/codex/rollout-contract.jsonl > /tmp/idle-projection.ndjson
jq -cS 'del(.sourcePath)' crates/idle-history-import/tests/fixtures/codex/projection.ndjson > /tmp/idle-recorded.ndjson
jq -cS 'del(.sourcePath)' /tmp/idle-projection.ndjson > /tmp/idle-current.ndjson
cmp /tmp/idle-recorded.ndjson /tmp/idle-current.ndjson
```

The unknown final record intentionally produces one decode diagnostic while
preserving all seven raw records. This check tests exporter compatibility;
native Codex runtime recording and native/import reconciliation belong to f10.
