//! Native host adapter over the viewer-independent `EditChain` query facade.
//!
//! Hosts resolve a chain binding to their own `ChainQueries` instance, then call
//! [`execute`]. WASM hosts execute the same portable requests remotely. Filesystem
//! I/O stays outside the reducer. Refreshing before a read is not a subscription
//! checkpoint. Reconciliation reads refresh once and materialize all loaded windows
//! against that index before returning a replacement to the reducer.

mod reconciliation;

use std::io;

use editchain_core::{OpId, activity::Operation};
use editchain_engine::queries::{self, ChainQueries, Lookup};

use idle_history::query::{
    Comparison, ContentValue, FieldContent, Filter, HistoryPage, MatchRange, Observation,
    OperationDetails, Page, Query, QueryAction, QueryOutput, QueryResult, RawRecord,
    RecordLookupStatus, RecordRef, SearchMatch, SearchPage,
};

/// Execute a read against the host's explicitly resolved chain.
/// Native-open effects belong to the platform adapter and return an error here.
///
/// # Errors
/// Returns presentable errors for a mismatched chain, invalid identities/pages,
/// unavailable host actions, engine I/O, or an inconsistent record/content lookup.
pub fn execute(queries: &mut ChainQueries, chain: &str, query: &Query) -> QueryOutput {
    if chain != query.chain {
        return Err(idle_history::query::Error::new(
            "History query belongs to a different chain",
        ));
    }
    if matches!(query.action, QueryAction::Open { .. }) {
        return Err(idle_history::query::Error::new(
            "Native history actions require a platform adapter",
        ));
    }
    let _changes = queries.refresh().map_err(failure)?;
    execute_read(queries, &query.action).map_err(failure)
}

fn failure(error: impl std::fmt::Display) -> idle_history::query::Error {
    idle_history::query::Error::new(error)
}

fn id(value: &str) -> io::Result<OpId> {
    idle_history::query::full_id(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.message))
}

fn page(value: &Page) -> io::Result<queries::PageRequest> {
    Ok(queries::PageRequest {
        after: value.after.as_deref().map(id).transpose()?,
        limit: usize::try_from(value.limit).map_err(io::Error::other)?,
    })
}

fn execute_read(queries: &ChainQueries, action: &QueryAction) -> io::Result<QueryResult> {
    match action {
        QueryAction::History {
            filter,
            page: request,
        } => history(queries, request, |op| filter.matches(op)).map(QueryResult::History),
        QueryAction::Item {
            item,
            page: request,
        } => {
            let item = id(item)?;
            history(queries, request, |op| {
                Operation::view(op).map_or(op.id == item, |activity| activity.item.0 == item)
            })
            .map(QueryResult::History)
        }
        QueryAction::Search {
            text,
            filter,
            page: request,
        } => search(queries, text, filter, request).map(QueryResult::Search),
        QueryAction::OperationDetails { operation } => {
            operation_details(queries, id(operation)?).map(QueryResult::OperationDetails)
        }
        QueryAction::Open { .. } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native action requires a platform adapter",
        )),
        QueryAction::Reconcile(request) => reconciliation::capture(queries, request)
            .map(|snapshot| QueryResult::Reconciled(Box::new(snapshot))),
    }
}

fn history(
    queries: &ChainQueries,
    request: &Page,
    matches: impl Fn(&editchain_core::Op) -> bool,
) -> io::Result<HistoryPage> {
    // Scan candidates rather than selecting a schema-three-only index key: the
    // same filter must include legacy records through Operation::view as well.
    let result = queries.history(None, page(request)?)?;
    Ok(HistoryPage {
        scanned: u32::try_from(result.items.len()).map_err(io::Error::other)?,
        next_after: result.next_after.map(|id| id.to_string()),
        observations: result
            .items
            .iter()
            .filter(|entry| matches(&entry.operation))
            .map(observation)
            .collect::<io::Result<_>>()?,
    })
}

fn reference(value: queries::RecordRef) -> RecordRef {
    RecordRef {
        operation: value.operation.to_string(),
        hash: OpId::from_bytes(value.record_hash).to_string(),
    }
}

fn observation(value: &queries::HistoryEntry) -> io::Result<Observation> {
    Ok(Observation {
        record: reference(value.record_ref),
        operation_json: serde_json::to_vec(&value.operation).map_err(io::Error::other)?,
    })
}

fn field(value: queries::ContentResult) -> io::Result<FieldContent> {
    Ok(FieldContent {
        record: reference(value.record_ref),
        field: serde_json::to_string(&value.field).map_err(io::Error::other)?,
        content_id: value
            .reference
            .map(|reference| serde_json::to_string(&reference.id))
            .transpose()
            .map_err(io::Error::other)?,
        declared_length: value.reference.and_then(|reference| reference.len),
        value: match value.value {
            queries::ContentValue::Available(bytes) => ContentValue::Available(bytes),
            queries::ContentValue::NotRecorded => ContentValue::NotRecorded,
            queries::ContentValue::Missing => ContentValue::Missing,
            queries::ContentValue::Corrupt => ContentValue::Corrupt,
            queries::ContentValue::Unresolvable => ContentValue::Unresolvable,
        },
    })
}

fn search(
    queries: &ChainQueries,
    text: &str,
    filter: &Filter,
    request: &Page,
) -> io::Result<SearchPage> {
    let result = queries.search(text, None, page(request)?)?;
    let mut matches = Vec::new();
    for hit in result.hits {
        if let Lookup::Found(entry) = queries.operation(hit.record_ref.operation)?
            && filter.matches(&entry.operation)
        {
            matches.push(SearchMatch {
                observation: observation(&entry)?,
                fields: hit
                    .fields
                    .into_iter()
                    .map(|value| {
                        Ok(MatchRange {
                            field: serde_json::to_string(&value.field).map_err(io::Error::other)?,
                            start: value.range.start,
                            end: value.range.end,
                        })
                    })
                    .collect::<io::Result<_>>()?,
            });
        }
    }
    let mut unavailable = Vec::new();
    for content in result.unavailable {
        if let Lookup::Found(entry) = queries.operation(content.record_ref.operation)?
            && filter.matches(&entry.operation)
        {
            unavailable.push(field(content)?);
        }
    }
    Ok(SearchPage {
        matches,
        unavailable,
        scanned: u32::try_from(result.scanned).map_err(io::Error::other)?,
        next_after: result.next_after.map(|id| id.to_string()),
    })
}

fn operation_details(queries: &ChainQueries, operation: OpId) -> io::Result<OperationDetails> {
    let records = queries
        .record_variants(operation)?
        .into_iter()
        .map(|record| RawRecord {
            record: reference(record.reference),
            bytes: record.encoded,
        })
        .collect();
    let mut result = OperationDetails {
        operation: operation.to_string(),
        status: RecordLookupStatus::Missing,
        observation: None,
        records,
        fields: Vec::new(),
        comparison: None,
    };
    match queries.operation(operation)? {
        Lookup::Missing => {}
        Lookup::Conflicted(_) => result.status = RecordLookupStatus::Conflicted,
        Lookup::Found(entry) => {
            result.status = RecordLookupStatus::Found;
            result.observation = Some(observation(&entry)?);
            let Lookup::Found(fields) = queries.contents(operation)? else {
                return Err(io::Error::other("Operation changed during content lookup"));
            };
            result.fields = fields.into_iter().map(field).collect::<io::Result<_>>()?;
            if idle_history::query::kind(&entry.operation)
                == idle_history::query::ActivityKind::File
            {
                let Lookup::Found(diff) = queries.diff(operation)? else {
                    return Err(io::Error::other("Revision changed during lookup"));
                };
                result.comparison = Some(match diff.comparison {
                    queries::ByteComparison::Unavailable => Comparison::Unavailable,
                    queries::ByteComparison::Identical => Comparison::Identical,
                    queries::ByteComparison::Changed { before, after } => Comparison::Changed {
                        before_start: before.start,
                        before_end: before.end,
                        after_start: after.start,
                        after_end: after.end,
                    },
                });
            }
        }
    }
    Ok(result)
}
