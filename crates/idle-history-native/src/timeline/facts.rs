//! Read compact display facts while retaining all full content on the host.

use std::{collections::BTreeSet, io};

use editchain_core::{
    OpId,
    activity::{Field, FileAction, Kind, Operation, Stage, TurnAction},
};
use editchain_engine::queries::{ChainQueries, ContentField, ContentValue, HistoryEntry, Lookup};
use idle_history::{
    ContentText,
    provider::ProviderEvidence,
    query::{ActivityKind, OpenTarget, RecordRef},
    timeline::{Address, Row, Source, Target},
};

use super::{
    logical,
    model::{Fact, Snapshot, key},
    presentation, project, providers,
};

pub(super) fn read(
    queries: &ChainQueries,
    entry: &HistoryEntry,
    source: Source,
) -> io::Result<Option<(Fact, String)>> {
    let Some(operation) = Operation::view(&entry.operation) else {
        return Ok(None);
    };
    let address = Address {
        source,
        record: RecordRef {
            operation: entry.operation.id.to_string(),
            hash: OpId::from_bytes(entry.record_ref.record_hash).to_string(),
        },
    };
    let mut fact = Fact {
        row: Row {
            occurrence: key(source, operation.id),
            item: operation.item.to_string(),
            records: vec![address.clone()],
            address: Target::Record(address),
            kind: idle_history::query::kind(&entry.operation),
            title: format!("{:?}", operation.kind.name()),
            preview: String::new(),
            author: String::new(),
            session: String::new(),
            tags: Vec::new(),
            timestamp: operation.time_ms,
            open: OpenTarget::OperationJson,
            unavailable: None,
            group: None,
            relationships: Vec::new(),
            graph: idle_history::timeline::Geometry::default(),
        },
        order_time: None,
        parents: operation.parents.iter().map(|id| key(source, id)).collect(),
        causes: operation.causes.iter().map(ToString::to_string).collect(),
        original: operation
            .original
            .as_ref()
            .map(|raw| key(source, raw.operation)),
        aliases: Vec::new(),
        author: operation.author.map(|id| id.to_string()),
        session: operation.session.map(|id| id.to_string()),
        recorder: operation.recorder.to_string(),
        task: operation.turn.map(|id| id.to_string()),
        task_title: None,
        attempt: None,
        path: None,
        source: entry
            .operation
            .source
            .or_else(|| operation.legacy.as_ref().and_then(|old| old.source)),
        raw_hash: None,
        provider: None,
        link: None,
        git: None,
        label: None,
        terminal: false,
        protected: false,
        outcome: editchain_core::activity::Status::Unknown,
        searchable: true,
        visibility: super::model::Visibility::Primary,
        supports: Vec::new(),
        form: if matches!(entry.operation.kind, editchain_core::OpKind::Activity(_)) {
            super::model::RecordForm::Native
        } else {
            super::model::RecordForm::Legacy
        },
        human_edit: None,
        note_turn: None,
    };
    if let Some(old) = &operation.legacy {
        fact.aliases.push(key(Source::Retained, old.operation));
        fact.aliases.push(key(source, old.operation));
        fact.aliases.extend(
            old.folded
                .iter()
                .flat_map(|id| [key(source, id), key(Source::Retained, id)]),
        );
    }
    if let Some(origin) = fact.source {
        fact.aliases.push(key(source, origin.id()));
    }
    fact.aliases.sort();
    fact.aliases.dedup();
    let contents = match queries.contents(entry.operation.id)? {
        Lookup::Found(contents) => contents,
        Lookup::Missing | Lookup::Conflicted(_) => Vec::new(),
    };
    let payloads = operation.kind.fields();
    let text = |field: Field| {
        contents
            .iter()
            .find(|value| value.field == ContentField::Record(field))
            .and_then(|value| {
                if let ContentValue::Available(bytes) = &value.value {
                    std::str::from_utf8(bytes).ok()
                } else {
                    None
                }
            })
            .or_else(|| {
                payloads
                    .iter()
                    .find(|(candidate, _)| *candidate == field)
                    .and_then(|(_, payload)| match payload {
                        editchain_core::Payload::Inline(bytes) => std::str::from_utf8(bytes).ok(),
                        editchain_core::Payload::Blob(reference) => contents
                            .iter()
                            .find(|value| {
                                value
                                    .reference
                                    .is_some_and(|stored| stored.id == reference.id)
                            })
                            .and_then(|value| {
                                if let ContentValue::Available(bytes) = &value.value {
                                    std::str::from_utf8(bytes).ok()
                                } else {
                                    None
                                }
                            }),
                        editchain_core::Payload::Empty => None,
                    })
            })
            .unwrap_or_default()
    };
    let mut search = String::new();
    for value in &contents {
        match &value.value {
            ContentValue::Available(bytes) => {
                if let Ok(value) = std::str::from_utf8(bytes) {
                    search.push_str(value);
                    search.push('\n');
                }
            }
            ContentValue::NotRecorded => {}
            ContentValue::Missing | ContentValue::Corrupt | ContentValue::Unresolvable => {
                fact.searchable = false;
            }
        }
    }
    match &operation.kind {
        Kind::Author(_) => fact.label = Some(text(Field::Label).to_owned()),
        Kind::Session(session) => {
            fact.label = Some(text(Field::Label).to_owned());
            text(Field::Label).clone_into(&mut fact.row.preview);
            fact.row.title = format!("Session {:?}", session.action);
            if let Some(parent) = session.initiated_by {
                fact.parents.push(key(source, parent));
            }
        }
        Kind::Turn(turn) => {
            fact.row.title = format!("Execution {:?}", turn.action);
            fact.task = Some(operation.item.to_string());
            fact.attempt = Some(turn.attempt.to_string());
            fact.terminal = matches!(turn.action, TurnAction::Finished | TurnAction::Removed);
            fact.protected = true;
            fact.parents
                .extend(turn.triggers.iter().map(|id| key(source, id)));
        }
        Kind::Message(message) => {
            fact.row.title = format!("{:?}", message.category);
            fact.row.preview = message
                .blocks
                .iter()
                .enumerate()
                .map(|(index, _)| text(Field::MessageBlock(index)))
                .collect::<Vec<_>>()
                .join("\n");
            fact.outcome = message
                .outcome
                .as_ref()
                .map_or(editchain_core::activity::Status::Unknown, |outcome| {
                    outcome.status
                });
            fact.protected = message.outcome.is_some();
        }
        Kind::Tool(tool) => {
            fact.attempt = Some(tool.attempt.to_string());
            text(Field::ToolName).clone_into(&mut fact.row.title);
            if fact.row.title.is_empty() {
                fact.row.title = "Tool".into();
            }
            (if tool.stage == Stage::Started {
                text(Field::Arguments)
            } else {
                text(Field::Output)
            })
            .clone_into(&mut fact.row.preview);
            fact.outcome = tool
                .outcome
                .as_ref()
                .map_or(editchain_core::activity::Status::Unknown, |outcome| {
                    outcome.status
                });
            fact.protected = matches!(tool.stage, Stage::Started | Stage::Finished)
                || tool.outcome.as_ref().is_some_and(|outcome| {
                    matches!(
                        outcome.status,
                        editchain_core::activity::Status::Failure
                            | editchain_core::activity::Status::Cancelled
                    )
                });
        }
        Kind::File(file) => {
            fact.path = Some(file.path.0.to_string());
            fact.row.title = format!("File {:?}", file.action);
            text(Field::Path).clone_into(&mut fact.row.preview);
            fact.protected = true;
            let available = |field| {
                contents.iter().any(|value| {
                    value.field == field && matches!(value.value, ContentValue::Available(_))
                })
            };
            let diff = matches!(
                file.action,
                FileAction::Create
                    | FileAction::Change
                    | FileAction::Save
                    | FileAction::Rename
                    | FileAction::Delete
            );
            fact.row.open = if diff
                && available(ContentField::FileBase)
                && available(ContentField::FileAfter)
            {
                OpenTarget::Diff
            } else if !diff && available(ContentField::FileAfter) {
                OpenTarget::File
            } else {
                fact.row.unavailable = Some(
                    "Recorded file content is unavailable; opening the operation JSON.".into(),
                );
                OpenTarget::OperationJson
            };
            fact.row.tags.push(format!("{:?}", file.action));
        }
        Kind::Commit(commit) => {
            fact.git = Some((
                format!("{}:{}", commit.repository.0, commit.oid),
                commit
                    .parents
                    .iter()
                    .map(|oid| format!("{}:{oid}", commit.repository.0))
                    .collect(),
            ));
            fact.row.title = "Commit".into();
            text(Field::Content).clone_into(&mut fact.row.preview);
            fact.protected = true;
        }
        Kind::Note(note) => {
            fact.row.title = format!("{:?}", note.category);
            text(Field::Content).clone_into(&mut fact.row.preview);
            fact.protected = true;
        }
        Kind::Link(link) => {
            fact.link = Some((link.from.clone(), link.to.clone(), link.relation.clone()));
            fact.provider = serde_json::from_str::<ProviderEvidence>(text(Field::Content)).ok();
        }
        Kind::Original(original) => {
            let bytes = contents
                .iter()
                .find(|value| {
                    matches!(
                        value.field,
                        ContentField::Record(Field::Content) | ContentField::ImportRaw
                    )
                })
                .and_then(|value| {
                    if let ContentValue::Available(bytes) = &value.value {
                        Some(bytes.as_slice())
                    } else {
                        None
                    }
                });
            fact.raw_hash = original
                .hash
                .filter(|hash| bytes.is_some_and(|bytes| blake3::hash(bytes).as_bytes() == hash));
            fact.row.title.clone_from(&original.provider);
            text(Field::Content).clone_into(&mut fact.row.preview);
            // Native lifecycle markers remain inspectable when normalization
            // has no Turn record for this historical provider format.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(text(Field::Content)) {
                let shape = value
                    .pointer("/payload/type")
                    .or_else(|| value.get("type"))
                    .and_then(serde_json::Value::as_str);
                fact.terminal = matches!(
                    shape,
                    Some("task_complete" | "turn_completed" | "turn_aborted")
                );
            }
        }
    }
    presentation::apply(&mut fact, &operation, &entry.operation);
    fact.row.title = ContentText::tool_label(fact.row.title, true).text;
    fact.label = fact
        .label
        .map(|label| ContentText::tool_label(label, true).text);
    fact.row.preview = ContentText::new(fact.row.preview, true).text;
    fact.row
        .author
        .clone_from(&fact.author.clone().unwrap_or_default());
    fact.row
        .session
        .clone_from(&fact.session.clone().unwrap_or_default());
    fact.parents.sort();
    fact.parents.dedup();
    search.push_str(&fact.row.title);
    Ok(Some((fact, search.to_lowercase())))
}

pub(super) fn install(snapshot: &mut Snapshot, fact: Fact, text: String) {
    let id = fact.row.occurrence.clone();
    remove(snapshot, &id);
    for alias in &fact.aliases {
        let _inserted = snapshot
            .aliases
            .entry(alias.clone())
            .or_default()
            .insert(id.clone());
    }
    for parent in references(&fact) {
        let _inserted = snapshot
            .references
            .entry(parent)
            .or_default()
            .insert(id.clone());
    }
    if let Some(original) = &fact.original {
        let _inserted = snapshot
            .outputs
            .entry(original.clone())
            .or_default()
            .insert(id.clone());
    }
    for target in &fact.supports {
        let _inserted = snapshot
            .supporting
            .entry(target.clone())
            .or_default()
            .insert(id.clone());
    }
    let _inserted = snapshot
        .items
        .entry(fact.row.item.clone())
        .or_default()
        .insert(id.clone());
    if let Some(label) = &fact.label {
        drop(
            snapshot
                .labels
                .entry(fact.row.item.clone())
                .or_default()
                .insert((fact.row.timestamp.unwrap_or(0), id.clone()), label.clone()),
        );
    }
    if fact.provider.is_some() {
        let _inserted = snapshot.providers.insert(id.clone());
    }
    providers::index(snapshot, &fact, false);
    logical::index(snapshot, &fact, false);
    if fact.link.is_some() {
        let _inserted = snapshot.links.insert(id.clone());
    }
    if fact.row.kind == ActivityKind::Original
        && fact.raw_hash.is_some()
        && let Some(source) = fact.source
        && source.seq.trailing_zeros() >= 16
    {
        drop(
            snapshot
                .generations
                .entry((source.node.0, source.boot))
                .or_default()
                .insert(source.seq, id.clone()),
        );
        if fact.terminal {
            drop(
                snapshot
                    .terminals
                    .entry((source.node.0, source.boot))
                    .or_default()
                    .insert(source.seq, id.clone()),
            );
        }
    }
    if let Some((git, _)) = &fact.git {
        let _inserted = snapshot
            .git
            .entry(git.clone())
            .or_default()
            .insert(id.clone());
    }
    for word in grams(&text) {
        let _inserted = snapshot.words.entry(word).or_default().insert(id.clone());
    }
    if !fact.searchable {
        snapshot.unavailable = snapshot.unavailable.saturating_add(1);
    }
    drop(snapshot.text.insert(id.clone(), text));
    drop(snapshot.facts.insert(id, fact));
}

pub(super) fn remove(snapshot: &mut Snapshot, id: &str) {
    let Some(fact) = snapshot.facts.remove(id) else {
        return;
    };
    providers::index(snapshot, &fact, true);
    logical::index(snapshot, &fact, true);
    for parent in references(&fact) {
        if let Some(children) = snapshot.references.get_mut(&parent) {
            let _removed = children.remove(id);
        }
    }
    for alias in fact.aliases {
        if let Some(aliases) = snapshot.aliases.get_mut(&alias) {
            let _removed = aliases.remove(id);
        }
    }
    if let Some(original) = fact.original
        && let Some(outputs) = snapshot.outputs.get_mut(&original)
    {
        let _removed = outputs.remove(id);
    }
    for target in &fact.supports {
        if let Some(records) = snapshot.supporting.get_mut(target) {
            let _removed = records.remove(id);
        }
    }
    if let Some(items) = snapshot.items.get_mut(&fact.row.item) {
        let _removed = items.remove(id);
    }
    if let Some(labels) = snapshot.labels.get_mut(&fact.row.item) {
        drop(labels.remove(&(fact.row.timestamp.unwrap_or(0), id.to_owned())));
    }
    let _removed = snapshot.providers.remove(id);
    let _removed = snapshot.links.remove(id);
    if !fact.searchable {
        snapshot.unavailable = snapshot.unavailable.saturating_sub(1);
    }
    if let Some(source) = fact.source {
        restore_generation(snapshot, source, id);
    }
    if let Some((git, _)) = fact.git
        && let Some(aliases) = snapshot.git.get_mut(&git)
    {
        let _removed = aliases.remove(id);
    }
    if let Some(text) = snapshot.text.remove(id) {
        for word in grams(&text) {
            if let Some(postings) = snapshot.words.get_mut(&word) {
                let _removed = postings.remove(id);
            }
        }
    }
}

fn restore_generation(snapshot: &mut Snapshot, source: editchain_core::SourceId, removed: &str) {
    let generation = (source.node.0, source.boot);
    let replacement = project::exact(snapshot, &key(Source::Current, source.id()))
        .or_else(|| project::exact(snapshot, &key(Source::Retained, source.id())))
        .filter(|fact| {
            fact.row.kind == ActivityKind::Original
                && fact.raw_hash.is_some()
                && fact.source == Some(source)
        })
        .map(|fact| (fact.row.occurrence.clone(), fact.terminal));
    for (values, terminal_only) in [
        (&mut snapshot.generations, false),
        (&mut snapshot.terminals, true),
    ] {
        if let Some(records) = values.get_mut(&generation)
            && records
                .get(&source.seq)
                .is_some_and(|stored| stored == removed)
        {
            if let Some((id, _)) = replacement
                .as_ref()
                .filter(|(_, terminal)| !terminal_only || *terminal)
            {
                drop(records.insert(source.seq, id.clone()));
            } else {
                drop(records.remove(&source.seq));
            }
        }
    }
}

pub(super) fn identities(fact: &Fact) -> Vec<String> {
    let mut result = vec![
        fact.row.occurrence.clone(),
        format!("item:{}", fact.row.item),
    ];
    result.extend(fact.aliases.iter().cloned());
    result.extend(fact.original.iter().cloned());
    result.extend(fact.supports.iter().cloned());
    if let Some(source) = fact.source {
        result.extend([
            key(Source::Current, source.id()),
            key(Source::Retained, source.id()),
        ]);
    }
    if let Some((git, _)) = &fact.git {
        result.push(format!("git:{git}"));
    }
    result
}

fn references(fact: &Fact) -> BTreeSet<String> {
    let mut result: BTreeSet<_> = fact
        .parents
        .iter()
        .chain(fact.original.iter())
        .chain(fact.supports.iter())
        .cloned()
        .collect();
    result.extend(fact.causes.iter().map(|item| format!("item:{item}")));
    if let Some((_, parents)) = &fact.git {
        result.extend(parents.iter().map(|git| format!("git:{git}")));
    }
    if let Some((from, to, _)) = &fact.link {
        for endpoint in std::iter::once(from).chain(to) {
            let _inserted = result.insert(match endpoint {
                editchain_core::activity::Entity::Operation(id) => fact
                    .row
                    .address
                    .record()
                    .map_or_else(String::new, |address| key(address.source, id)),
                editchain_core::activity::Entity::Item(id) => format!("item:{id}"),
                editchain_core::activity::Entity::Git { repository, oid } => {
                    format!("git:{}:{oid}", repository.0)
                }
            });
        }
    }
    if let Some(contract) = &fact.provider {
        result.extend([
            key(Source::Current, contract.source.id()),
            key(Source::Retained, contract.source.id()),
        ]);
        if let idle_history::provider::ProviderFact::CodexSource(meta) = &contract.fact {
            result.extend([
                key(Source::Current, meta.first.id()),
                key(Source::Retained, meta.first.id()),
            ]);
        }
        let outputs = match &contract.fact {
            idle_history::provider::ProviderFact::CodexDerivation(meta) => &meta.outputs[..],
            idle_history::provider::ProviderFact::ClaudeDerivation(meta) => &meta.outputs[..],
            idle_history::provider::ProviderFact::CodexSource(_)
            | idle_history::provider::ProviderFact::CodexLifecycle(_) => &[],
        };
        for source in outputs {
            result.extend([
                key(Source::Current, source.id()),
                key(Source::Retained, source.id()),
            ]);
        }
    }
    result
}

pub(super) fn grams(text: &str) -> BTreeSet<Vec<u8>> {
    (1..=3)
        .flat_map(|size| text.as_bytes().windows(size).map(<[u8]>::to_vec))
        .collect()
}
