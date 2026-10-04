//! Check repository projection rows against the currently accepted chain records.

use editchain_core::{OpId, activity::Operation};
use editchain_engine::queries::{ChainQueries, Lookup};
use idle_protocol::v1::projections::{
    FreshnessStatus, ProjectionAvailability, ProjectionGap, ProjectionInput, ProjectionReference,
};
use std::io;

/// Check every supplied projection source against accepted exact stored records.
/// Hosts call this after refreshing their chain query handle and before showing rows.
///
/// # Errors
/// Returns an error for missing, quarantined, changed or malformed source addresses.
pub fn validate_sources(queries: &ChainQueries, inputs: &[ProjectionInput]) -> io::Result<()> {
    for source in inputs
        .iter()
        .flat_map(|input| &input.rows)
        .flat_map(|row| &row.sources)
    {
        check(queries, source)?;
    }
    Ok(())
}

/// Remove rows whose exact sources have disappeared, changed or become quarantined.
/// Surviving rows and Activity remain readable, with explicit partial coverage.
#[must_use]
pub fn checked_inputs(queries: &ChainQueries, inputs: &[ProjectionInput]) -> Vec<ProjectionInput> {
    inputs
        .iter()
        .cloned()
        .map(|mut input| {
            input.rows.retain(|row| {
                let failure = if row.sources.is_empty() {
                    Some((None, "The row has no exact stored source.".to_owned()))
                } else {
                    row.sources.iter().find_map(|source| {
                        check(queries, source).err().map(|error| {
                            (
                                source.validate().ok().map(|()| source.clone()),
                                error.to_string(),
                            )
                        })
                    })
                };
                if let Some((reference, message)) = failure {
                    input.availability = ProjectionAvailability::Partial;
                    input.freshness.status = FreshnessStatus::Unknown;
                    input.total = None;
                    let gap = ProjectionGap { reference, message };
                    if !input.gaps.contains(&gap) {
                        input.gaps.push(gap);
                    }
                    false
                } else {
                    true
                }
            });
            input
        })
        .collect()
}

fn check(queries: &ChainQueries, source: &ProjectionReference) -> io::Result<()> {
    source.validate().map_err(io::Error::other)?;
    let id = source
        .observation
        .as_deref()
        .and_then(OpId::from_display_str)
        .ok_or_else(|| io::Error::other("Invalid projection observation."))?;
    let Lookup::Found(entry) = queries.operation(id)? else {
        return Err(io::Error::other(
            "A projection source is missing or quarantined.",
        ));
    };
    let operation = Operation::view(&entry.operation)
        .ok_or_else(|| io::Error::other("A projection source is not a supported record."))?;
    if source.item.as_deref() != Some(operation.item.to_string().as_str())
        || source.record_hash.as_deref()
            != Some(
                OpId::from_bytes(entry.record_ref.record_hash)
                    .to_string()
                    .as_str(),
            )
    {
        return Err(io::Error::other(
            "A projection source differs from its exact stored record.",
        ));
    }
    Ok(())
}
