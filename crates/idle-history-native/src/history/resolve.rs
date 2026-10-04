use editchain_engine::{
    IdQuery, OpId, OpKind,
    activity::{Field, Kind, Operation},
    queries::{
        ChainQueries, ContentField, ContentQuery, ContentResult, ContentValue, HistoryEntry,
        IdResolution, Lookup,
    },
};

use super::{Document, Failure, FailureCode, Preview, Request, Target, reference};

pub(super) fn prepare(
    queries: &ChainQueries,
    operation: OpId,
    request: &Request,
) -> Result<Preview, Failure> {
    let records = queries.record_variants(operation)?;
    if records.is_empty() {
        return Err(missing_record(queries, operation)?);
    }
    let record = records
        .iter()
        .find(|record| reference(record.reference) == request.record)
        .ok_or_else(|| Failure {
            candidates: records.iter().map(|record| reference(record.reference)).collect(),
            ..Failure::new(
                FailureCode::RecordMismatch,
                "The requested digest does not match a retained record. Select an exact representation.",
            )
        })?;
    let documents = if request.target == Target::Record {
        vec![Document {
            name: format!("{operation}.ec-record"),
            record: request.record.clone(),
            field: None,
            reference: None,
            bytes: record.encoded.clone(),
        }]
    } else {
        let entry = accepted(queries.operation(operation)?)?;
        content_documents(queries, &entry, &request.target)?
    };
    Ok(Preview {
        request: request.clone(),
        documents,
    })
}

fn missing_record(queries: &ChainQueries, operation: OpId) -> Result<Failure, Failure> {
    let query = IdQuery::parse(&operation.to_string()).ok_or_else(|| {
        Failure::new(FailureCode::InvalidReference, "Invalid operation identity.")
    })?;
    let candidates = match queries.resolve_id(&query)? {
        IdResolution::Found(id) if id != operation => vec![id],
        IdResolution::Ambiguous(ids) => ids,
        IdResolution::Missing | IdResolution::Found(_) => Vec::new(),
    };
    if candidates.is_empty() {
        return Ok(Failure::new(
            FailureCode::MissingRecord,
            "The requested record is unavailable.",
        ));
    }
    let mut failure = Failure::new(
        FailureCode::MigratedAlias,
        "This identity was migrated. Select a converted record or explicitly open the retained input source.",
    );
    for id in candidates {
        failure.candidates.extend(
            queries
                .record_variants(id)?
                .into_iter()
                .map(|record| reference(record.reference)),
        );
    }
    Ok(failure)
}

fn accepted<T>(lookup: Lookup<T>) -> Result<T, Failure> {
    match lookup {
        Lookup::Found(value) => Ok(value),
        Lookup::Missing => Err(Failure::new(
            FailureCode::MissingRecord,
            "The requested record is unavailable.",
        )),
        Lookup::Conflicted(records) => Err(Failure {
            candidates: records.into_iter().map(reference).collect(),
            ..Failure::new(
                FailureCode::Conflicted,
                "Conflicting records cannot supply a file or content preview. Open an exact raw record instead.",
            )
        }),
    }
}

fn content_documents(
    queries: &ChainQueries,
    entry: &HistoryEntry,
    target: &Target,
) -> Result<Vec<Document>, Failure> {
    let operation = entry.operation.id;
    match target {
        Target::Record => Err(Failure::new(
            FailureCode::Unavailable,
            "Expected a content action.",
        )),
        Target::Original => Ok(vec![original(queries, entry)?]),
        Target::File | Target::Diff => {
            if !matches!(entry.operation.kind, OpKind::File(_))
                && !matches!(&entry.operation.kind, OpKind::Activity(value) if matches!(value.kind, Kind::File(_)))
            {
                return Err(Failure::new(
                    FailureCode::Unavailable,
                    "This action requires a recorded file revision.",
                ));
            }
            let diff = accepted(queries.diff(operation)?)?;
            let name = file_name(queries, entry, true)?;
            if *target == Target::File {
                Ok(vec![document(diff.after, name)?])
            } else {
                Ok(vec![
                    document(diff.before, file_name(queries, entry, false)?)?,
                    document(diff.after, name)?,
                ])
            }
        }
        Target::Content { field, reference } => {
            let content = accepted(queries.content(ContentQuery {
                operation,
                field: *field,
            })?)?;
            if content.reference != *reference {
                return Err(Failure::new(
                    FailureCode::InvalidReference,
                    "The content address or length differs from the recorded field.",
                ));
            }
            Ok(vec![document(content, format!("{operation}.content"))?])
        }
    }
}

fn original(queries: &ChainQueries, entry: &HistoryEntry) -> Result<Document, Failure> {
    let activity = Operation::view(&entry.operation).ok_or_else(|| {
        Failure::new(
            FailureCode::NotRecorded,
            "No Original was recorded for this operation.",
        )
    })?;
    let original_entry;
    let selected = if matches!(activity.kind, Kind::Original(_)) {
        entry
    } else if let Some(original) = &activity.original {
        original_entry = accepted(queries.operation(original.operation)?)?;
        &original_entry
    } else {
        return Err(Failure::new(
            FailureCode::NotRecorded,
            "No Original was recorded for this operation.",
        ));
    };
    let field = if matches!(&selected.operation.kind, OpKind::Activity(value) if matches!(value.kind, Kind::Original(_)))
    {
        ContentField::Record(Field::Content)
    } else if matches!(selected.operation.kind, OpKind::Import(_)) {
        ContentField::ImportRaw
    } else if matches!(selected.operation.kind, OpKind::Unknown(_)) {
        ContentField::UnknownRaw
    } else {
        return Err(Failure::new(
            FailureCode::Unavailable,
            "The recorded Original reference does not identify an Original record.",
        ));
    };
    let content = accepted(queries.content(ContentQuery {
        operation: selected.operation.id,
        field,
    })?)?;
    document(content, format!("{}.original", selected.operation.id))
}

fn file_name(
    queries: &ChainQueries,
    entry: &HistoryEntry,
    renamed: bool,
) -> Result<String, Failure> {
    if let OpKind::Activity(activity) = &entry.operation.kind
        && let Kind::File(file) = &activity.kind
    {
        let field = if renamed && file.renamed_to.is_some() {
            Field::RenamedTo
        } else {
            Field::Path
        };
        let content = accepted(queries.content(ContentQuery {
            operation: entry.operation.id,
            field: ContentField::Record(field),
        })?)?;
        if let ContentValue::Available(bytes) = content.value
            && let Ok(name) = String::from_utf8(bytes)
            && !name.is_empty()
        {
            return Ok(name);
        }
    }
    Ok(format!("{}.file", entry.operation.id))
}

fn document(content: ContentResult, name: String) -> Result<Document, Failure> {
    let bytes = match content.value {
        ContentValue::Available(bytes) => bytes,
        ContentValue::NotRecorded => {
            return Err(Failure::new(
                FailureCode::NotRecorded,
                "This content was not recorded; it is not an empty file.",
            ));
        }
        ContentValue::Missing => {
            return Err(Failure::new(
                FailureCode::MissingContent,
                "The referenced content has not arrived.",
            ));
        }
        ContentValue::Corrupt => {
            return Err(Failure::new(
                FailureCode::CorruptContent,
                "Recorded content failed its address or length check.",
            ));
        }
        ContentValue::Unresolvable => {
            return Err(Failure::new(
                FailureCode::UnresolvableContent,
                "The recorded content address cannot be resolved.",
            ));
        }
    };
    Ok(Document {
        name,
        record: reference(content.record_ref),
        field: Some(content.field),
        reference: content.reference,
        bytes,
    })
}
