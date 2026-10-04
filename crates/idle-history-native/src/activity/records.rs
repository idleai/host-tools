use editchain_engine::{
    OpId,
    activity::{FileAction, Kind, Operation},
    queries::{
        ChainQueries, ContentField, ContentQuery, ContentValue, HistoryEntry, IndexKey, Lookup,
        PageRequest,
    },
};

use crate::history::{Failure, FailureCode, full_id, reference};

use super::{Selection, SourceRecord};

pub(super) const MAX_ITEMS: usize = 4000;
pub(super) const MAX_TEXT: usize = idle_editor_capture::wire::MAX_EDITOR_BUFFER_BYTES;

pub(super) fn source(entry: &HistoryEntry) -> SourceRecord {
    SourceRecord {
        record: reference(entry.record_ref),
        original: Operation::view(&entry.operation).is_some_and(|op| op.original.is_some()),
    }
}

pub(super) fn accepted<T>(lookup: Lookup<T>) -> Result<T, Failure> {
    match lookup {
        Lookup::Found(value) => Ok(value),
        Lookup::Missing => Err(Failure::new(
            FailureCode::MissingRecord,
            "The selected revision has not arrived.",
        )),
        Lookup::Conflicted(records) => Err(Failure {
            candidates: records.into_iter().map(reference).collect(),
            ..Failure::new(
                FailureCode::Conflicted,
                "The selected revision has conflicting records.",
            )
        }),
    }
}

pub(super) fn selected(
    queries: &ChainQueries,
    selection: &Selection,
) -> Result<HistoryEntry, Failure> {
    match selection {
        Selection::Record(record) => {
            let id = full_id(&record.operation)?;
            let _hash = full_id(&record.hash)?;
            let entry = accepted(queries.operation(id)?)?;
            if reference(entry.record_ref) != *record {
                return Err(Failure::new(
                    FailureCode::RecordMismatch,
                    "The selected record digest has changed.",
                ));
            }
            Ok(entry)
        }
        Selection::Editor {
            session,
            document,
            version,
        } => {
            if session.is_empty()
                || document.is_empty()
                || session.len() > 256
                || document.len() > 256
            {
                return Err(Failure::new(
                    FailureCode::InvalidReference,
                    "Invalid editor revision identity.",
                ));
            }
            let revision = idle_editor_capture::revision_id(session, document, *version);
            let (entries, complete) = scan(queries, IndexKey::Item(revision))?;
            let mut candidates = entries.into_iter().filter(|entry| {
                Operation::view(&entry.operation).is_some_and(|op| {
                    matches!(&op.kind,
                    Kind::File(file) if file.revision == Some(revision)
                        && matches!(file.action, FileAction::Snapshot | FileAction::Change))
                })
            });
            let entry = candidates.next().ok_or_else(|| Failure::new(FailureCode::MissingRecord,
                "This buffer revision has no accepted snapshot yet. Capture may be pending or incomplete."))?;
            if !complete || candidates.next().is_some() {
                return Err(Failure::new(
                    FailureCode::Conflicted,
                    "This buffer revision has multiple or incomplete snapshot observations.",
                ));
            }
            Ok(entry)
        }
    }
}

pub(super) fn scan(
    queries: &ChainQueries,
    key: IndexKey,
) -> Result<(Vec<HistoryEntry>, bool), Failure> {
    let mut entries = Vec::new();
    let mut after = None;
    let mut pages = MAX_ITEMS / 1000;
    loop {
        let page = queries.history(Some(key), PageRequest { after, limit: 1000 })?;
        entries.extend(page.items);
        after = page.next_after;
        pages = pages.saturating_sub(1);
        if after.is_none() || pages == 0 {
            return Ok((entries, after.is_none()));
        }
    }
}

pub(super) fn bytes(
    queries: &ChainQueries,
    id: OpId,
    field: ContentField,
) -> Result<Vec<u8>, Failure> {
    let value = accepted(queries.content(ContentQuery {
        operation: id,
        field,
    })?)?
    .value;
    match value {
        ContentValue::Available(bytes) if bytes.len() <= MAX_TEXT => Ok(bytes),
        ContentValue::Available(_) => Err(Failure::new(
            FailureCode::TooLarge,
            "The recorded content exceeds the decoration size limit.",
        )),
        ContentValue::NotRecorded => Err(Failure::new(
            FailureCode::NotRecorded,
            "The required content was not recorded.",
        )),
        ContentValue::Missing => Err(Failure::new(
            FailureCode::MissingContent,
            "Referenced content has not arrived.",
        )),
        ContentValue::Corrupt => Err(Failure::new(
            FailureCode::CorruptContent,
            "Recorded content failed validation.",
        )),
        ContentValue::Unresolvable => Err(Failure::new(
            FailureCode::UnresolvableContent,
            "The recorded content address cannot be resolved.",
        )),
    }
}

pub(super) fn text(
    queries: &ChainQueries,
    id: OpId,
    field: ContentField,
) -> Result<String, Failure> {
    String::from_utf8(bytes(queries, id, field)?).map_err(|_error| {
        Failure::new(
            FailureCode::Unavailable,
            "The recorded content is not UTF-8 text.",
        )
    })
}
