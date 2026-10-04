//! Portable bounded projection reads and native-shell result contracts.

mod adapter;
mod types;

use crate::binding::Context;
use serde::{Deserialize, Serialize};
pub use types::*;

/// Read all five destinations for one authorized audience and chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionQuery {
    /// Explicit provider, workspace, authenticated contributor and logical chain.
    pub context: Context,
    /// Maximum engine candidates to read, from 1 to 1000; not a result total.
    pub limit: u32,
    /// Revalidate upstream sources instead of reusing a recent read.
    #[serde(default)]
    pub refresh_sources: bool,
}

/// Complete projection result or an explicit read failure.
pub type ProjectionOutput = Result<ProjectionSnapshot, crate::query::Error>;
