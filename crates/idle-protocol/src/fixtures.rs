//! Contract examples shared by producer and consumer tests.

/// Standalone discovery response with contributors and resources.
pub const STANDALONE_SNAPSHOT: &str = include_str!("../tests/fixtures/standalone_snapshot.json");

/// Managed discovery response with contributors and resources.
pub const MANAGED_SNAPSHOT: &str = include_str!("../tests/fixtures/managed_snapshot.json");

/// Runtime input command with its scope and correlation fields.
pub const SUBMIT_INPUT: &str = include_str!("../tests/fixtures/submit_input.json");

/// Completed runtime report matching the input command example.
pub const RUNTIME_COMPLETED: &str = include_str!("../tests/fixtures/runtime_completed.json");
