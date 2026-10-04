//! Native editor indicators derived from exact recorded file occurrences.
//!
//! Revision identity and snapshot bytes must both match. Content equality alone
//! never combines independent occurrences, authors or exposure intervals.

mod project;
mod ranges;
mod records;
mod types;

pub use types::{Indicator, IndicatorKind, Preview, Request, Selection, SourceRecord};

use editchain_engine::queries::ChainQueries;
use idle_history::binding::RepositoryChainBinding;

use crate::history::{Failure, FailureCode, Source, validate_binding};

/// Refresh and project accepted records for an explicitly bound revision.
///
/// # Errors
/// Returns binding, missing/conflicted revision, unavailable snapshot and storage
/// failures. Partial observations are retained in the result's issue list.
pub fn prepare(
    queries: &mut ChainQueries,
    binding: &RepositoryChainBinding,
    source: Source,
    request: &Request,
) -> Result<Preview, Failure> {
    validate_binding(binding)?;
    if binding != &request.binding || source != request.source {
        return Err(Failure::new(
            FailureCode::BindingMismatch,
            "The activity request belongs to a different repository, chain or source.",
        ));
    }
    let _changes = queries.refresh()?;
    project::prepare(queries, request)
}

#[cfg(test)]
mod tests;
