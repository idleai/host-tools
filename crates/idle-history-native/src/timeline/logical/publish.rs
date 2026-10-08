//! Stable display blocks retain exact addresses of their current source records.

use std::collections::BTreeSet;

use idle_history::query::ActivityKind;

use super::super::{
    facts,
    model::{Fact, Snapshot},
    read,
};
use super::{Item, current, validate};

pub(super) fn item(snapshot: &mut Snapshot, item: &Item, changed: &mut BTreeSet<String>) {
    if let Some(previous) = snapshot.logical.published.remove(item) {
        retire(snapshot, &previous, changed);
    }
    let Some(revision) = current(snapshot, item) else {
        return;
    };
    let Some(source) = validate::record(snapshot, revision.source) else {
        return;
    };
    let identity = format!("{:?}:{}", item, revision.incarnation);
    let id = format!("activity:{}", blake3::hash(identity.as_bytes()).to_hex());
    let outputs: Vec<_> = revision
        .outputs
        .iter()
        .filter_map(|id| validate::record(snapshot, *id))
        .collect();
    let mut fact = block(
        &id,
        source,
        validate::record(snapshot, revision.incarnation),
        &outputs,
    );
    if !item.0.2.is_empty() {
        let boundary = snapshot
            .logical
            .removals
            .get(&item.0)
            .and_then(editchain_index::OrderedSet::last);
        fact.task = Some(format!(
            "task:{}",
            blake3::hash(format!("{:?}:{boundary:?}", item.0).as_bytes()).to_hex()
        ));
        fact.attempt = None;
    }
    let text = search(snapshot, source, &outputs);
    if let Some(task) = &fact.task {
        drop(
            snapshot
                .logical
                .task_keys
                .insert(task.clone(), item.0.clone()),
        );
    }
    facts::install(snapshot, fact, text);
    drop(snapshot.logical.published.insert(item.clone(), id.clone()));
    let _inserted = changed.insert(id);
}

pub(super) fn human(snapshot: &mut Snapshot, group: &str, changed: &mut BTreeSet<String>) {
    if let Some(previous) = snapshot.logical.human_published.remove(group) {
        retire(snapshot, &previous, changed);
    }
    let Some(revisions) = snapshot.logical.human.get(group) else {
        return;
    };
    let Some(((_, first), (_, last))) = revisions.first_key_value().zip(revisions.last_key_value())
    else {
        return;
    };
    let Some(source) = snapshot.facts.get(last) else {
        return;
    };
    let id = format!("human-edit:{}", blake3::hash(group.as_bytes()).to_hex());
    let outputs: Vec<_> = snapshot
        .references
        .get(last)
        .into_iter()
        .flat_map(editchain_index::OrderedSet::iter)
        .filter_map(|id| snapshot.facts.get(id))
        .filter(|fact| {
            fact.row.kind == ActivityKind::File
                && fact.parents.len() == 1
                && fact.parents.first() == Some(last)
        })
        .collect();
    let mut fact = block(&id, source, snapshot.facts.get(first), &outputs);
    fact.task = None;
    let text = search(snapshot, source, &outputs);
    facts::install(snapshot, fact, text);
    drop(
        snapshot
            .logical
            .human_published
            .insert(group.into(), id.clone()),
    );
    let _inserted = changed.insert(id);
}

fn retire(snapshot: &mut Snapshot, id: &str, changed: &mut BTreeSet<String>) {
    if let Some(node) = snapshot.nodes.get(id).cloned() {
        read::index_order(snapshot, &node, true);
    }
    facts::remove(snapshot, id);
    let _inserted = changed.insert(id.into());
}

fn block(id: &str, source: &Fact, first: Option<&Fact>, outputs: &[&Fact]) -> Fact {
    let content = outputs
        .iter()
        .copied()
        .filter(|fact| fact.is_primary())
        .min_by_key(|fact| match fact.row.kind {
            ActivityKind::Message => 0,
            ActivityKind::Tool => 1,
            ActivityKind::File => 2,
            ActivityKind::Note => 3,
            ActivityKind::Session | ActivityKind::Turn | ActivityKind::Commit => 4,
            ActivityKind::Author
            | ActivityKind::Link
            | ActivityKind::Initialization
            | ActivityKind::Original
            | ActivityKind::Unknown => 5,
        })
        .unwrap_or(source);
    let mut fact = content.clone();
    fact.row.occurrence = id.into();
    fact.row.item = blake3::hash(id.as_bytes()).to_hex().to_string();
    fact.row.records = std::iter::once(source)
        .chain(outputs.iter().copied())
        .chain(first)
        .filter_map(|fact| fact.row.address.record().cloned())
        .collect();
    fact.row.records.sort();
    fact.row.records.dedup();
    if !matches!(source.row.title.as_str(), "system" | "note" | "session")
        && !source.row.preview.is_empty()
    {
        fact.row.title.clone_from(&source.row.title);
        fact.row.tags.clone_from(&source.row.tags);
        fact.row.preview.clone_from(&source.row.preview);
    }
    fact.order_time = first.and_then(|fact| fact.order_time.or(fact.row.timestamp));
    fact.row.timestamp = source.row.timestamp.or(content.row.timestamp);
    fact.row.group = None;
    fact.parents = first.map_or_else(Vec::new, |fact| fact.parents.clone());
    fact.session.clone_from(&source.session);
    fact.recorder.clone_from(&source.recorder);
    fact.row.session = source.session.clone().unwrap_or_default();
    fact.original = None;
    fact.aliases.clear();
    fact.causes.clear();
    fact.source = source.source;
    fact.task_title = outputs
        .iter()
        .find_map(|fact| fact.task_title.clone())
        .or_else(|| source.task_title.clone());
    if let Some((sequence, _)) = &mut fact.task_title {
        *sequence = first
            .and_then(|fact| fact.source)
            .map_or(*sequence, |source| source.seq);
    }
    fact.raw_hash = None;
    fact.provider = None;
    fact.link = None;
    fact.label = None;
    fact.supports.clear();
    fact.form = super::super::model::RecordForm::Projected;
    fact.protected =
        source.protected || source.failed() || outputs.iter().any(|fact| fact.failed());
    fact.human_edit = None;
    fact
}

fn search(snapshot: &Snapshot, source: &Fact, outputs: &[&Fact]) -> String {
    std::iter::once(source)
        .chain(outputs.iter().copied())
        .filter_map(|fact| snapshot.text.get(&fact.row.occurrence).map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}
