//! Current logical activities reconstructed from complete recorded revisions.

mod publish;
mod tasks;
mod validate;

use std::collections::BTreeSet;

use editchain_core::{OpId, SourceId};
use editchain_index::{Map, OrderedMap, OrderedSet};
use idle_history::provider::{CodexLogicalChange, ProviderFact};
use serde::{Deserialize, Serialize};

use super::model::{Fact, Snapshot};

type Turn = ((u64, u32), String, String);
type Item = (Turn, String);
type Owner = (Item, SourceId);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct State {
    claims: Map<SourceId, OrderedSet<String>>,
    pending: OrderedSet<SourceId>,
    human_dirty: OrderedSet<String>,
    selected: Map<SourceId, ProviderFact>,
    items: Map<Item, OrderedMap<SourceId, Revision>>,
    turns: Map<Turn, OrderedSet<Item>>,
    removals: Map<Turn, OrderedSet<SourceId>>,
    published: Map<Item, String>,
    owners: Map<OpId, OrderedMap<Owner, u64>>,
    covered: Map<OpId, OrderedMap<SourceId, u64>>,
    human: Map<String, OrderedMap<(u64, String), String>>,
    human_published: Map<String, String>,
    statuses: Map<Turn, OrderedMap<SourceId, tasks::Status>>,
    task_keys: Map<String, Turn>,
    titles: Map<String, OrderedMap<(u64, String), String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Revision {
    incarnation: SourceId,
    source: SourceId,
    outputs: Vec<SourceId>,
}

pub(super) fn index(snapshot: &mut Snapshot, fact: &Fact, remove: bool) {
    if let Some(task) = &fact.task
        && let Some((sequence, title)) = &fact.task_title
    {
        let titles = snapshot.logical.titles.entry(task.clone()).or_default();
        let order = (*sequence, fact.row.occurrence.clone());
        if remove {
            drop(titles.remove(&order));
        } else {
            drop(titles.insert(order, title.clone()));
        }
    }
    if let Some(contract) = &fact.provider
        && matches!(
            contract.fact,
            ProviderFact::CodexDerivation(_) | ProviderFact::ClaudeDerivation(_)
        )
    {
        let _inserted = snapshot.logical.pending.insert(contract.source);
        let claims = snapshot.logical.claims.entry(contract.source).or_default();
        if remove {
            let _removed = claims.remove(&fact.row.occurrence);
        } else {
            let _inserted = claims.insert(fact.row.occurrence.clone());
        }
    }
    if let Some(group) = &fact.human_edit {
        let _inserted = snapshot.logical.human_dirty.insert(group.clone());
        let revisions = snapshot.logical.human.entry(group.clone()).or_default();
        let order = (
            fact.source.map_or(0, |source| source.seq),
            fact.row.occurrence.clone(),
        );
        if remove {
            drop(revisions.remove(&order));
        } else {
            drop(revisions.insert(order, fact.row.occurrence.clone()));
        }
    }
}

pub(super) fn schedule(snapshot: &mut Snapshot, keys: &[String]) {
    for id in keys {
        if let Some(contract) = snapshot
            .facts
            .get(id)
            .and_then(|fact| fact.provider.as_ref())
        {
            let _inserted = snapshot.logical.pending.insert(contract.source);
        }
    }
}

pub(super) fn pending(snapshot: &Snapshot) -> bool {
    !snapshot.logical.pending.is_empty() || !snapshot.logical.human_dirty.is_empty()
}

pub(super) fn resolve(snapshot: &mut Snapshot) -> BTreeSet<String> {
    let sources: Vec<_> = snapshot
        .logical
        .pending
        .iter()
        .take(super::model::BATCH)
        .copied()
        .collect();
    let mut changes = Changes::default();
    for source in sources {
        let _removed = snapshot.logical.pending.remove(&source);
        let next = validate::select(snapshot, source);
        if snapshot.logical.selected.get(&source) == next.as_ref() {
            continue;
        }
        if let Some(previous) = snapshot.logical.selected.remove(&source) {
            apply(snapshot, source, &previous, true, &mut changes);
        }
        if let Some(next) = next {
            apply(snapshot, source, &next, false, &mut changes);
            drop(snapshot.logical.selected.insert(source, next));
        }
    }
    for item in changes.items {
        publish::item(snapshot, &item, &mut changes.rows);
    }
    let groups: Vec<_> = snapshot
        .logical
        .human_dirty
        .iter()
        .take(super::model::BATCH)
        .cloned()
        .collect();
    for group in groups {
        let _removed = snapshot.logical.human_dirty.remove(&group);
        publish::human(snapshot, &group, &mut changes.rows);
    }
    changes.rows
}

#[derive(Default)]
struct Changes {
    items: BTreeSet<Item>,
    rows: BTreeSet<String>,
}

fn apply(
    snapshot: &mut Snapshot,
    source: SourceId,
    meta: &ProviderFact,
    remove: bool,
    changes: &mut Changes,
) {
    let outputs = outputs(meta);
    for id in std::iter::once(source).chain(outputs.iter().copied()) {
        count(
            snapshot.logical.covered.entry(id.id()).or_default(),
            source,
            remove,
        );
        if let Some(id) = validate::record(snapshot, id).map(|fact| fact.row.occurrence.clone()) {
            let _inserted = changes.rows.insert(id);
        }
    }
    let ProviderFact::CodexDerivation(meta) = meta else {
        let item = (
            ((source.node.0, source.boot), String::new(), String::new()),
            source.to_string(),
        );
        revision(
            snapshot,
            item.clone(),
            Revision {
                incarnation: source,
                source,
                outputs: outputs.to_vec(),
            },
            remove,
        );
        let _inserted = changes.items.insert(item);
        return;
    };
    let metadata = tasks::apply(snapshot, source, meta, remove, &mut changes.items);
    let mut owned = BTreeSet::new();
    for change in &meta.changes {
        match change {
            CodexLogicalChange::Upsert {
                turn,
                item,
                incarnation,
                outputs,
            } => {
                let item = (
                    (
                        (source.node.0, source.boot),
                        meta.thread.0.clone(),
                        turn.clone(),
                    ),
                    item.clone(),
                );
                revision(
                    snapshot,
                    item.clone(),
                    Revision {
                        incarnation: *incarnation,
                        source,
                        outputs: outputs.clone(),
                    },
                    remove,
                );
                owned.extend(outputs.iter().copied());
                let _inserted = changes.items.insert(item);
            }
            CodexLogicalChange::RemoveTurn { turn } => {
                let turn = (
                    (source.node.0, source.boot),
                    meta.thread.0.clone(),
                    turn.clone(),
                );
                let removals = snapshot.logical.removals.entry(turn.clone()).or_default();
                if remove {
                    let _removed = removals.remove(&source);
                } else {
                    let _inserted = removals.insert(source);
                }
                changes.items.extend(
                    snapshot
                        .logical
                        .turns
                        .get(&turn)
                        .into_iter()
                        .flat_map(OrderedSet::iter)
                        .cloned(),
                );
            }
        }
    }
    let extra: Vec<_> = meta
        .outputs
        .iter()
        .filter(|id| !owned.contains(id) && !metadata.contains(id))
        .copied()
        .collect();
    if !extra.is_empty() {
        let item = (
            (
                (source.node.0, source.boot),
                meta.thread.0.clone(),
                String::new(),
            ),
            source.to_string(),
        );
        revision(
            snapshot,
            item.clone(),
            Revision {
                incarnation: source,
                source,
                outputs: extra,
            },
            remove,
        );
        let _inserted = changes.items.insert(item);
    }
}

fn revision(snapshot: &mut Snapshot, item: Item, revision: Revision, remove: bool) {
    for id in std::iter::once(revision.source)
        .chain(std::iter::once(revision.incarnation))
        .chain(revision.outputs.iter().copied())
        .collect::<BTreeSet<_>>()
    {
        count(
            snapshot.logical.owners.entry(id.id()).or_default(),
            (item.clone(), revision.incarnation),
            remove,
        );
    }
    let versions = snapshot.logical.items.entry(item.clone()).or_default();
    if remove {
        drop(versions.remove(&revision.source));
    } else {
        drop(versions.insert(revision.source, revision));
    }
    let _inserted = snapshot
        .logical
        .turns
        .entry(item.0.clone())
        .or_default()
        .insert(item);
}

fn count<K: Clone + Ord>(values: &mut OrderedMap<K, u64>, key: K, remove: bool) {
    let old = values.get(&key).copied().unwrap_or(0);
    let next = if remove {
        old.saturating_sub(1)
    } else {
        old.saturating_add(1)
    };
    if next == 0 {
        let _removed = values.remove(&key);
    } else {
        let _old = values.insert(key, next);
    }
}

fn outputs(meta: &ProviderFact) -> &[SourceId] {
    match meta {
        ProviderFact::CodexDerivation(meta) => &meta.outputs,
        ProviderFact::ClaudeDerivation(meta) => &meta.outputs,
        ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_) => &[],
    }
}

fn current<'a>(snapshot: &'a Snapshot, item: &Item) -> Option<&'a Revision> {
    let (source, revision) = snapshot.logical.items.get(item)?.last_key_value()?;
    let removal = snapshot
        .logical
        .removals
        .get(&item.0)
        .and_then(OrderedSet::last);
    removal
        .is_none_or(|removed| source > removed)
        .then_some(revision)
}

pub(super) fn hidden(snapshot: &Snapshot, fact: &Fact) -> bool {
    !fact.is_projected()
        && (fact.human_edit.is_some()
            || fact
                .source
                .and_then(|source| snapshot.logical.covered.get(&source.id()))
                .is_some_and(|values| !values.is_empty()))
}

pub(super) fn owners(snapshot: &Snapshot, id: &str, ancestry: bool) -> Vec<String> {
    if let Some(group) = snapshot
        .facts
        .get(id)
        .and_then(|fact| fact.human_edit.as_ref())
    {
        if ancestry
            && snapshot
                .logical
                .human
                .get(group)
                .and_then(OrderedMap::first_key_value)
                .is_none_or(|(_, first)| first != id)
        {
            return Vec::new();
        }
        return snapshot
            .logical
            .human_published
            .get(group)
            .cloned()
            .into_iter()
            .collect();
    }
    let source = super::project::exact(snapshot, id)
        .and_then(|fact| fact.source)
        .map(SourceId::id)
        .or_else(|| {
            id.split_once(':')
                .and_then(|(_, id)| OpId::from_display_str(id))
        });
    source
        .and_then(|source| snapshot.logical.owners.get(&source))
        .into_iter()
        .flat_map(OrderedMap::iter)
        .filter(|((item, incarnation), _)| {
            (!ancestry || source == Some(incarnation.id()))
                && current(snapshot, item)
                    .is_some_and(|revision| revision.incarnation == *incarnation)
        })
        .filter_map(|((item, _), _)| snapshot.logical.published.get(item).cloned())
        .collect()
}

pub(super) fn task_live(snapshot: &Snapshot, task: &str) -> Option<bool> {
    snapshot
        .logical
        .statuses
        .get(snapshot.logical.task_keys.get(task)?)
        .and_then(OrderedMap::last_key_value)
        .map(|(_, status)| *status == tasks::Status::Active)
}

pub(super) use tasks::caption as task_caption;
