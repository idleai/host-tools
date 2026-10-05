//! Shared editor wire contracts and durable schema-three capture.
//!
//! The engine supplies immutable storage; this crate owns editor observation conversion.
//! Raw observation schema 1 and operation schema 3 are independently versioned.

mod blobs;
mod context;
mod convert;
mod identity;
mod replay;
pub mod service;
mod state;
pub mod wire;
mod writer;

pub use blobs::CaptureBlobs;
pub use context::observe_context;
pub use identity::revision_id;
pub use replay::EditorReplay;
pub use writer::CaptureWriter;

/// Capture errors include invalid input, unavailable history and storage errors.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[cfg(test)]
mod tests;
