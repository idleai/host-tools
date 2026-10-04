//! Check repository projection rows against the currently accepted chain records.

use editchain_core::{
    OpId, OpKind,
    activity::{Field, Kind, Operation},
};
use editchain_engine::queries::{ChainQueries, ContentField, ContentQuery, Lookup};
use idle_protocol::v1::projections::{
    FreshnessStatus, ProjectionAvailability, ProjectionGap, ProjectionInput, ProjectionReference,
};
use std::{collections::BTreeMap, io};

type ContentChecks = BTreeMap<OpId, Result<(), String>>;

/// Check every supplied projection source against accepted exact stored records.
/// Hosts call this after refreshing their chain query handle and before showing rows.
///
/// # Errors
/// Returns an error for missing, quarantined, changed or malformed source addresses.
pub fn validate_sources(queries: &ChainQueries, inputs: &[ProjectionInput]) -> io::Result<()> {
    let mut contents = ContentChecks::new();
    for source in inputs
        .iter()
        .flat_map(|input| &input.rows)
        .flat_map(|row| &row.sources)
    {
        check(queries, source, &mut contents)?;
    }
    Ok(())
}

/// Remove rows whose exact sources have disappeared, changed or become quarantined.
/// Surviving rows and Activity remain readable, with explicit partial coverage.
#[must_use]
pub fn checked_inputs(queries: &ChainQueries, inputs: &[ProjectionInput]) -> Vec<ProjectionInput> {
    let mut contents = ContentChecks::new();
    inputs
        .iter()
        .cloned()
        .map(|mut input| {
            input.rows.retain(|row| {
                let failure = if row.sources.is_empty() {
                    Some((None, "The row has no exact stored source.".to_owned()))
                } else {
                    row.sources.iter().find_map(|source| {
                        check(queries, source, &mut contents).err().map(|error| {
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

fn check(
    queries: &ChainQueries,
    source: &ProjectionReference,
    contents: &mut ContentChecks,
) -> io::Result<()> {
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
    if let Kind::Original(original) = &operation.kind {
        let field = if matches!(entry.operation.kind, OpKind::Activity(_)) {
            ContentField::Record(Field::Content)
        } else if matches!(entry.operation.kind, OpKind::Unknown(_)) {
            ContentField::UnknownRaw
        } else {
            ContentField::ImportRaw
        };
        return contents
            .entry(id)
            .or_insert_with(|| original_content(queries, id, field, original.hash))
            .clone()
            .map_err(io::Error::other);
    }
    Ok(())
}

fn original_content(
    queries: &ChainQueries,
    operation: OpId,
    field: ContentField,
    hash: Option<[u8; 32]>,
) -> Result<(), String> {
    let Lookup::Found(content) = queries
        .content(ContentQuery { operation, field })
        .map_err(|error| error.to_string())?
    else {
        return Err("A projection's Original record is missing or quarantined.".into());
    };
    let Some(bytes) = content.value.bytes() else {
        return Err("A projection's Original bytes are missing, corrupt or unavailable.".into());
    };
    if hash.is_some_and(|expected| blake3::hash(bytes).as_bytes() != &expected) {
        return Err("A projection's Original bytes differ from their recorded hash.".into());
    }
    Ok(())
}
