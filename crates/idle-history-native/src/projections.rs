//! Native read adapter over `EditChain`, with a separate controller mapping hook.
//!
//! Reads refresh once and start at the beginning. Neither the candidate boundary
//! nor index refresh results are durable checkpoints. Hosts buffer invalidations
//! while this read runs and request a replacement after a change. WASM hosts
//! forward the same projection query to an authorized service.

use editchain_core::{OpId, activity::Operation};
use editchain_engine::queries::{
    ChainQueries, ContentResult, ContentValue, EntityRef, HistoryEntry, Lookup, PageRequest,
    RecordedRelationship,
};
use idle_protocol::v1::{
    ApiVersion,
    identity::ProjectionCount,
    projections::{
        FreshnessStatus, ProjectionAvailability, ProjectionFreshness, ProjectionGap,
        ProjectionInput, ProjectionKind, ProjectionReference, ProjectionRow, ProjectionSnapshot,
    },
};

use idle_history::projections::{ProjectionOutput, ProjectionQuery};
use idle_history::query::Error as EffectError;

fn error(message: impl std::fmt::Display) -> EffectError {
    EffectError::new(message)
}

/// A complete accepted envelope with exact content or explicit field availability.
#[derive(Clone, Debug)]
pub struct ProjectionRecord {
    /// Full engine operation, original encoding digest and content references.
    pub entry: HistoryEntry,
    /// Every field, resolved through engine queries without preview truncation.
    pub fields: Vec<ContentResult>,
}

/// Bounded engine query result passed to the host's controller mapping.
#[derive(Clone, Debug)]
pub struct ProjectionRead {
    /// Accepted observations; quarantined representations are never facts.
    pub records: Vec<ProjectionRecord>,
    /// Recorded parents, logical connections, targets and summary coverage.
    pub relationships: Vec<RecordedRelationship>,
    /// Candidate boundary only; never chronology or a controller watermark.
    pub next_after: Option<OpId>,
    /// Bounds, content gaps and chain integrity limitations.
    pub gaps: Vec<ProjectionGap>,
}

/// Host-owned mapping from recorded controller state to the four derived views.
/// f11 owns the production implementation and all controller payload meanings.
pub trait ProjectionMapper {
    /// Return task, error, triage and need-input inputs, each exactly once.
    /// The read-only queries permit exact lookups beyond the bounded activity page.
    ///
    /// # Errors
    /// Return an error when mapping cannot produce a valid replacement snapshot.
    fn inputs(
        &self,
        queries: &ChainQueries,
        read: &ProjectionRead,
    ) -> Result<Vec<ProjectionInput>, EffectError>;
}

/// Default production adapter until a controller-state mapping is connected.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableMapper;

impl ProjectionMapper for UnavailableMapper {
    fn inputs(
        &self,
        _queries: &ChainQueries,
        _read: &ProjectionRead,
    ) -> Result<Vec<ProjectionInput>, EffectError> {
        Ok([
            ProjectionKind::Task,
            ProjectionKind::Error,
            ProjectionKind::Triage,
            ProjectionKind::NeedInput,
        ]
        .into_iter()
        .map(|kind| ProjectionInput {
            kind,
            freshness: unknown_freshness(),
            availability: ProjectionAvailability::Unavailable,
            total: None,
            rows: Vec::new(),
            gaps: vec![ProjectionGap {
                reference: None,
                message: "Controller projection adapter is unavailable".into(),
            }],
        })
        .collect())
    }
}

/// Resolve one explicitly bound chain, read records and invoke the host's mapper.
/// The host authenticates the query context before calling this function.
///
/// # Errors
/// Rejects wrong-chain/invalid requests, engine failures and invalid mapped inputs.
pub fn execute(
    queries: &mut ChainQueries,
    chain: &str,
    query: &ProjectionQuery,
    mapper: &impl ProjectionMapper,
) -> ProjectionOutput {
    if chain != query.context.chain
        || [
            &query.context.provider,
            &query.context.workspace,
            &query.context.contributor,
            &query.context.chain,
        ]
        .iter()
        .any(|part| part.is_empty())
    {
        return Err(error(
            "Projection query has an invalid context or a different chain",
        ));
    }
    if !(1..=1000).contains(&query.limit) {
        return Err(error("Projection read limit must be between 1 and 1000"));
    }
    let _changes = queries.refresh().map_err(error)?;
    let read = read(queries, query.limit)?;
    let mut inputs = vec![activity(queries, &read)?];
    inputs.extend(mapper.inputs(queries, &read)?);
    idle_history::projections::ProjectionSnapshot::try_from(ProjectionSnapshot {
        version: ApiVersion::V1,
        workspace_id: query.context.workspace.as_str().into(),
        chain: chain.into(),
        inputs,
    })
}

fn unknown_freshness() -> ProjectionFreshness {
    ProjectionFreshness {
        status: FreshnessStatus::Unknown,
        generated_at: None,
        checkpoint: None,
    }
}

fn read(queries: &ChainQueries, limit: u32) -> Result<ProjectionRead, EffectError> {
    let page = PageRequest {
        after: None,
        limit: usize::try_from(limit).map_err(error)?,
    };
    let history = queries.history(None, page).map_err(error)?;
    let relationships = queries.relationships(None, page).map_err(error)?.items;
    let mut result = ProjectionRead {
        records: Vec::new(),
        relationships,
        next_after: history.next_after,
        gaps: Vec::new(),
    };
    if result.next_after.is_some() {
        result.gaps.push(ProjectionGap {
            reference: None,
            message: "Activity read reached its candidate limit".into(),
        });
    }
    let stats = queries.index().stats();
    for (count, message) in [
        (stats.quarantined, "quarantined record variants"),
        (stats.undecodable, "unsupported records"),
        (stats.incomplete_tails, "incomplete segment tails"),
    ] {
        if count > 0 {
            result.gaps.push(ProjectionGap {
                reference: None,
                message: format!("Chain contains {count} {message}"),
            });
        }
    }
    for entry in history.items {
        let Lookup::Found(fields) = queries.contents(entry.operation.id).map_err(error)? else {
            return Err(error("Projection record changed during content lookup"));
        };
        for field in &fields {
            let message = match &field.value {
                ContentValue::Available(_) | ContentValue::NotRecorded => continue,
                ContentValue::Missing => "Recorded content is missing",
                ContentValue::Corrupt => "Recorded content is corrupt",
                ContentValue::Unresolvable => "Recorded content cannot be resolved",
            };
            result.gaps.push(ProjectionGap {
                reference: Some(source_reference(&entry)),
                message: format!("{message}: {:?}", field.field),
            });
        }
        result.records.push(ProjectionRecord { entry, fields });
    }
    let mut checked = std::collections::BTreeSet::new();
    for relationship in &result.relationships {
        for endpoint in [relationship.source, relationship.target] {
            if let EntityRef::Operation(id) = endpoint
                && checked.insert(id)
            {
                let message = match queries.operation(id).map_err(error)? {
                    Lookup::Found(_) => continue,
                    Lookup::Missing => "Referenced observation is missing",
                    Lookup::Conflicted(_) => "Referenced observation is quarantined",
                };
                result.gaps.push(ProjectionGap {
                    reference: Some(ProjectionReference {
                        observation: Some(id.to_string()),
                        item: None,
                        record_hash: None,
                    }),
                    message: message.into(),
                });
            }
        }
    }
    Ok(result)
}

/// Preserve both identities and the exact stored-encoding digest of a source.
#[must_use]
pub fn source_reference(entry: &HistoryEntry) -> ProjectionReference {
    ProjectionReference {
        observation: Some(entry.operation.id.to_string()),
        item: Operation::view(&entry.operation).map(|activity| activity.item.to_string()),
        record_hash: Some(OpId::from_bytes(entry.record_ref.record_hash).to_string()),
    }
}

/// Resolve an observation to its full logical identity when currently available.
/// Missing and conflicted observations retain their original address without an
/// invented item or a chosen conflicting digest.
///
/// # Errors
/// Returns engine record-lookup failures.
pub fn observation_reference(
    queries: &ChainQueries,
    id: OpId,
) -> Result<ProjectionReference, EffectError> {
    Ok(match queries.operation(id).map_err(error)? {
        Lookup::Found(entry) => source_reference(&entry),
        Lookup::Missing | Lookup::Conflicted(_) => ProjectionReference {
            observation: Some(id.to_string()),
            item: None,
            record_hash: None,
        },
    })
}

fn entity_reference(
    queries: &ChainQueries,
    entity: EntityRef,
) -> Result<Option<ProjectionReference>, EffectError> {
    match entity {
        EntityRef::Operation(id) => observation_reference(queries, id).map(Some),
        EntityRef::Item(item) => Ok(Some(ProjectionReference {
            observation: None,
            item: Some(item.to_string()),
            record_hash: None,
        })),
        // Non-history addresses remain in the full source operation and relationships.
        EntityRef::Session(_) | EntityRef::Git { .. } => Ok(None),
    }
}

/// Collect separate history endpoints without conflating item and observation IDs.
/// Full relation names and non-history endpoints remain in [`ProjectionRead`].
///
/// # Errors
/// Returns engine lookup failures while resolving endpoint observations.
pub fn related_references(
    queries: &ChainQueries,
    read: &ProjectionRead,
    entry: &HistoryEntry,
) -> Result<Vec<ProjectionReference>, EffectError> {
    let source = source_reference(entry);
    let mut related = Vec::new();
    for relation in read
        .relationships
        .iter()
        .filter(|relation| relation.record_ref == entry.record_ref)
    {
        for endpoint in [relation.source, relation.target] {
            if let Some(reference) = entity_reference(queries, endpoint)?
                && reference != source
                && (reference.observation.is_some() || reference.item != source.item)
                && !related.contains(&reference)
            {
                related.push(reference);
            }
        }
    }
    Ok(related)
}

fn activity(queries: &ChainQueries, read: &ProjectionRead) -> Result<ProjectionInput, EffectError> {
    let rows = read
        .records
        .iter()
        .map(|record| {
            let operation = &record.entry.operation;
            Ok(ProjectionRow {
                key: operation.id.to_string(),
                title: Operation::view(operation).map_or_else(
                    || "Recorded operation".into(),
                    |activity| format!("{:?}", activity.kind.name()),
                ),
                summary: None,
                url: None,
                status: None,
                labels: Vec::new(),
                sources: vec![source_reference(&record.entry)],
                related: related_references(queries, read, &record.entry)?,
            })
        })
        .collect::<Result<Vec<_>, EffectError>>()?;
    let complete = read.gaps.is_empty();
    Ok(ProjectionInput {
        kind: ProjectionKind::Activity,
        freshness: unknown_freshness(),
        availability: if complete {
            ProjectionAvailability::Complete
        } else {
            ProjectionAvailability::Partial
        },
        total: if complete {
            Some(ProjectionCount(u64::try_from(rows.len()).map_err(error)?))
        } else {
            None
        },
        rows,
        gaps: read.gaps.clone(),
    })
}
