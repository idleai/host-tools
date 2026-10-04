//! Captured source samples shared by import and CLI compatibility tests.

/// Human activity archive containing one session.
pub const HUMAN_SESSION: &str = include_str!("../tests/fixtures/human/session.jsonl");

/// Claude session source containing messages and tool calls.
pub const CLAUDE_SESSION: &str = include_str!("../tests/fixtures/claude/session.jsonl");

/// Codex rollout source covering the exporter contract.
pub const CODEX_ROLLOUT: &str = include_str!("../tests/fixtures/codex/rollout-contract.jsonl");

/// Normalized Codex projection corresponding to the rollout sample.
pub const CODEX_PROJECTION: &str = include_str!("../tests/fixtures/codex/projection.ndjson");
