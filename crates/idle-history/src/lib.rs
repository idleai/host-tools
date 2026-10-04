//! Portable history presentation, peer state and recorded application contracts.
//! Crux owns application state; import adapters and native hosts reuse these
//! types without depending on the UI runtime.

pub mod binding;
pub mod connection;
mod presentation;
pub mod projections;
pub mod query;
pub mod reconciliation;
pub mod requests;
mod selection;

pub use presentation::{ContentText, MAX_ROW_TEXT_BYTES, MAX_TOOL_LABEL_BYTES, RowContent};
pub use selection::Selection;

/// Recorded editor identity and work contracts.
pub mod human;
/// Versioned source-provider identity and derivation contracts.
pub mod provider;
/// Application classifications used by history presentation.
pub mod taxonomy;
