//! Native previews over exact engine records and explicit repository bindings.
//!
//! No working file, display preview, patch application or ID prefix participates
//! in resolving historical bytes. Retained migration inputs use a separately
//! installed source; an old digest never selects a converted representation.

mod resolve;
pub mod service;
mod types;

pub use types::{Document, Failure, FailureCode, Preview, Request, Source, Target};

use editchain_engine::{IdQuery, OpId, queries::ChainQueries};
use idle_history::{binding::RepositoryChainBinding, query::RecordRef};

/// Resolve one action against the explicitly installed source and binding.
/// Refresh observes late blobs and conflict retractions before every action.
///
/// # Errors
/// Returns distinct binding, record, alias, conflict and content failures.
pub fn prepare(
    queries: &mut ChainQueries,
    binding: &RepositoryChainBinding,
    source: Source,
    request: &Request,
) -> Result<Preview, Failure> {
    if binding != &request.binding || request.source != source {
        return Err(Failure::new(
            FailureCode::BindingMismatch,
            "The action belongs to a different repository, chain or source.",
        ));
    }
    validate_binding(binding)?;
    let operation = full_id(&request.record.operation)?;
    let _hash = full_id(&request.record.hash)?;
    let _changes = queries.refresh()?;
    resolve::prepare(queries, operation, request)
}

pub(crate) fn validate_binding(binding: &RepositoryChainBinding) -> Result<(), Failure> {
    if [
        &binding.workspace_id,
        &binding.repository_id,
        &binding.chain,
    ]
    .into_iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(Failure::new(
            FailureCode::BindingMismatch,
            "An explicit workspace, repository and chain binding is required.",
        ));
    }
    Ok(())
}

pub(crate) fn full_id(value: &str) -> Result<OpId, Failure> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && let Some(id) = IdQuery::parse(value).and_then(|query| query.full())
    {
        return Ok(id);
    }
    Err(Failure::new(
        FailureCode::InvalidReference,
        "A complete lowercase 256-bit operation ID and record digest are required.",
    ))
}

pub(crate) fn reference(value: editchain_engine::queries::RecordRef) -> RecordRef {
    RecordRef {
        operation: value.operation.to_string(),
        hash: OpId::from_bytes(value.record_hash).to_string(),
    }
}

#[cfg(test)]
mod tests;
