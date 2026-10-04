//! Exact source retention and bounded discovery of recorded session items.

use std::{collections::BTreeMap, io, path::Path};

use editchain_core::{
    OpId, Payload,
    activity::{Field, ItemId, Kind, KindName, Location, NativeId, Operation, Original},
};
use editchain_engine::{
    Admission, Engine, encode_op,
    queries::{
        ChainQueries, ContentField, ContentQuery, ContentValue, HistoryEntry, IndexKey, Lookup,
        PageRequest,
    },
};
use idle_protocol::v1::{
    projections::ProjectionReference,
    repository::{ReadReport, ReadState, RecordedSession, SourceRecord},
};

pub(crate) fn capture(
    chain: &Path,
    repository: &str,
    endpoint: &str,
    bytes: &[u8],
) -> io::Result<ProjectionReference> {
    let engine = Engine::open(chain)?;
    let digest = blake3::hash(bytes);
    let key = serde_json::to_vec(&(repository, endpoint, digest.to_hex().to_string()))?;
    let id = OpId::from_bytes(blake3::derive_key("idle.github.response.v1", &key));
    let item = ItemId::derive("github-response", id.as_bytes());
    let blob = engine.store_blob(bytes)?;
    let operation = Operation::new(
        id,
        item,
        ItemId::derive("recorder", b"idle.github-rest.v1"),
        Kind::Original(Original {
            provider: "github".into(),
            format: Some("rest-json-2026-03-10".into()),
            native: vec![NativeId {
                kind: "endpoint".into(),
                value: endpoint.into(),
            }],
            location: Some(Location {
                path: endpoint.into(),
                record: None,
                offset: None,
            }),
            bytes: Payload::Blob(blob),
            hash: Some(*digest.as_bytes()),
        }),
    )
    .into_op()
    .map_err(io::Error::other)?;
    let encoded = encode_op(&operation).map_err(io::Error::other)?;
    if engine.append_encoded(&encoded)? == Admission::Conflict {
        return Err(io::Error::other(
            "The retained GitHub response has conflicting stored variants.",
        ));
    }
    let queries = engine.queries()?;
    let Lookup::Found(entry) = queries.operation(id)? else {
        return Err(io::Error::other(
            "The retained GitHub response is not accepted history.",
        ));
    };
    let hash = blake3::hash(&encoded);
    if entry.record_ref.record_hash != *hash.as_bytes() {
        return Err(io::Error::other(
            "The retained GitHub response has changed.",
        ));
    }
    Ok(ProjectionReference {
        observation: Some(id.to_string()),
        item: Some(item.to_string()),
        record_hash: Some(hash.to_hex().to_string()),
    })
}

pub(crate) fn sessions(chain: &Path, now: u64) -> (Vec<RecordedSession>, ReadReport) {
    if !chain.exists() {
        return (
            Vec::new(),
            crate::report(
                "history.sessions",
                ReadState::Complete,
                "No history has been captured in the bound chain yet.",
                now,
            ),
        );
    }
    match read_sessions(chain, now) {
        Ok(result) => result,
        Err(_error) => (
            Vec::new(),
            crate::report(
                "history.sessions",
                ReadState::Unavailable,
                "Recorded sessions could not be read from the bound chain. Refresh to retry.",
                now,
            ),
        ),
    }
}

fn read_sessions(chain: &Path, now: u64) -> io::Result<(Vec<RecordedSession>, ReadReport)> {
    let mut queries = ChainQueries::open(chain)?;
    let _changes = queries.refresh()?;
    let page = queries.history(
        Some(IndexKey::Kind(KindName::Session)),
        PageRequest {
            after: None,
            limit: 1000,
        },
    )?;
    // Older encodings have no Kind index. A bounded candidate scan includes
    // their session registrations without assuming operation order is chronology.
    let legacy = queries.history(
        None,
        PageRequest {
            after: None,
            limit: 1000,
        },
    )?;
    let stats = queries.index().stats();
    let mut limited = page.next_after.is_some()
        || legacy.next_after.is_some()
        || stats.quarantined > 0
        || stats.undecodable > 0
        || stats.incomplete_tails > 0;
    let mut sessions = BTreeMap::<String, RecordedSession>::new();
    let mut entries = page.items;
    entries.extend(
        legacy
            .items
            .into_iter()
            .filter(|entry| matches!(entry.operation.kind, editchain_core::OpKind::Session(_))),
    );
    if entries.len() > 1000 {
        limited = true;
        entries.truncate(1000);
    }
    for entry in entries {
        let Some(operation) = Operation::view(&entry.operation) else {
            continue;
        };
        let Kind::Session(value) = &operation.kind else {
            continue;
        };
        let id = operation.item.to_string();
        let session = sessions
            .entry(id.clone())
            .or_insert_with(|| RecordedSession {
                id,
                labels: Vec::new(),
                actions: Vec::new(),
                sources: Vec::new(),
                records: Vec::new(),
            });
        let action = format!("{:?}", value.action);
        insert_unique(&mut session.actions, action);
        let label = queries.content(ContentQuery {
            operation: operation.id,
            field: if matches!(entry.operation.kind, editchain_core::OpKind::Session(_)) {
                ContentField::SessionLabel
            } else {
                ContentField::Record(Field::Label)
            },
        })?;
        if let Lookup::Found(content) = label {
            match content.value {
                ContentValue::Available(bytes) => {
                    if let Ok(label) = String::from_utf8(bytes) {
                        if !label.is_empty() {
                            insert_unique(&mut session.labels, crate::short_text(&label, 256));
                        }
                    } else {
                        limited = true;
                    }
                }
                ContentValue::NotRecorded => {}
                ContentValue::Missing | ContentValue::Corrupt | ContentValue::Unresolvable => {
                    limited = true;
                }
            }
        } else {
            limited = true;
        }
        let provider = operation
            .original
            .as_ref()
            .and_then(|original| queries.operation(original.operation).ok())
            .and_then(|result| match result {
                Lookup::Found(entry) => {
                    Operation::view(&entry.operation).and_then(|operation| match operation.kind {
                        Kind::Original(original) => Some(original.provider),
                        Kind::Session(_)
                        | Kind::Turn(_)
                        | Kind::Message(_)
                        | Kind::Tool(_)
                        | Kind::File(_)
                        | Kind::Commit(_)
                        | Kind::Note(_)
                        | Kind::Author(_)
                        | Kind::Link(_) => None,
                    })
                }
                Lookup::Missing | Lookup::Conflicted(_) => None,
            })
            .unwrap_or_else(|| format!("recorder {}", operation.recorder));
        insert_unique(&mut session.sources, crate::short_text(&provider, 256));
        session.records.push(source_record(&entry, operation.item));
    }
    let report = crate::report(
        "history.sessions",
        if limited {
            ReadState::Partial
        } else {
            ReadState::Complete
        },
        if limited {
            "Up to 1,000 session observations and 1,000 candidates for older encodings; additional, quarantined or unreadable records remain. Open session history for exact records."
        } else {
            "Recorded local and imported session items; recorded lifecycle events do not indicate a running process."
        },
        now,
    );
    Ok((sessions.into_values().collect(), report))
}

fn insert_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn source_record(entry: &HistoryEntry, item: ItemId) -> SourceRecord {
    SourceRecord {
        observation: entry.operation.id.to_string(),
        item: item.to_string(),
        record_hash: OpId::from_bytes(entry.record_ref.record_hash).to_string(),
    }
}
