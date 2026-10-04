//! Portable history operations and lossless results for every host.

mod helpers;
mod types;

pub use helpers::{full_id, item_key, kind};
pub use types::*;

/// A presentable read or conversion failure without private diagnostics.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    /// Explanation suitable for the requesting client.
    pub message: String,
}

impl Error {
    /// Preserve the supplied read failure as a presentable message.
    #[must_use]
    pub fn new(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
