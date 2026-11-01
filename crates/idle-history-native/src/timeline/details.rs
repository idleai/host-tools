//! Attached rows retain exact record or Git file identities without graph nodes.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use idle_history::{
    query::{ActivityKind, OpenTarget},
    timeline::{Details, Geometry, Row, Target},
};

use super::{
    model::{Fact, Snapshot, key},
    project,
};

pub(super) fn disclosure(snapshot: &Snapshot, row: &Row) -> Option<Details> {
    if matches!(row.address, Target::Commit { .. }) {
        return Some(Details {
            count: None,
            expanded: false,
        });
    }
    let count = records(snapshot, row).len();
    (count > 0).then_some(Details {
        count: Some(u64::try_from(count).unwrap_or(u64::MAX)),
        expanded: false,
    })
}

pub(super) fn file_path(snapshot: &Snapshot, row: &Row) -> Option<String> {
    if !row.presentation.file {
        return None;
    }
    if let Some(path) = row
        .presentation
        .file_path
        .as_ref()
        .filter(|path| !path.is_empty())
    {
        return Some(path.clone());
    }
    let paths: BTreeSet<_> = row
        .records
        .iter()
        .chain(row.address.record())
        .filter_map(|address| {
            let id = key(address.source, &address.record.operation);
            snapshot
                .facts
                .get(&id)
                .or_else(|| project::exact(snapshot, &id))
        })
        .filter(|fact| fact.row.kind == ActivityKind::File)
        .flat_map(super::facts::identities)
        .flat_map(|id| snapshot.supporting.get(&id).into_iter().flatten())
        .filter_map(|id| snapshot.facts.get(id))
        .filter(|fact| fact.row.kind == ActivityKind::Note && !fact.is_observation())
        .map(|fact| fact.row.preview.as_str())
        .filter(|path| !path.is_empty())
        .collect();
    (paths.len() == 1)
        .then(|| paths.first().map(|path| (*path).to_owned()))
        .flatten()
}

fn records<'a>(snapshot: &'a Snapshot, row: &Row) -> Vec<&'a Fact> {
    let mut records = BTreeMap::new();
    let addresses = row
        .records
        .iter()
        .map(|address| key(address.source, &address.record.operation));
    let supporting = snapshot
        .supporting
        .get(&row.occurrence)
        .into_iter()
        .flatten()
        .cloned();
    for id in addresses.chain(supporting) {
        let Some(fact) = snapshot
            .facts
            .get(&id)
            .or_else(|| project::exact(snapshot, &id))
        else {
            continue;
        };
        if fact.row.address == row.address
            || fact.is_observation()
            || !matches!(
                fact.row.kind,
                ActivityKind::Message
                    | ActivityKind::Tool
                    | ActivityKind::File
                    | ActivityKind::Note
            )
        {
            continue;
        }
        let _previous = records.insert(
            (
                fact.source.map_or(0, |source| source.seq),
                fact.row.occurrence.clone(),
            ),
            fact,
        );
    }
    records.into_values().collect()
}

pub(super) fn load(snapshot: &Snapshot, row: &Row) -> io::Result<Vec<Row>> {
    if let Target::Commit { repository, oid } = &row.address {
        let source = snapshot
            .git_source
            .as_ref()
            .ok_or_else(|| io::Error::other("Git history is unavailable."))?;
        return crate::git::files::changes(&source.root, repository, oid)?
            .into_iter()
            .map(|change| {
                let identity = serde_json::to_string(&change).map_err(io::Error::other)?;
                let mut child = row.clone();
                child.occurrence = child_key(&row.occurrence, &identity);
                child.item.clone_from(&child.occurrence);
                child.preview = change.old_path.as_ref().map_or_else(
                    || change.path.clone(),
                    |old| format!("{old} → {}", change.path),
                );
                child.title = "change".into();
                child.tags = vec![change.status.clone()];
                if change.binary {
                    child.tags.push("binary".into());
                }
                child.presentation.file_path = Some(change.path.clone());
                child.address = Target::CommitFile(Box::new(change));
                child.kind = ActivityKind::File;
                child.open = OpenTarget::Diff;
                child.presentation.file = true;
                child.presentation.label = "change".into();
                child.presentation.title.clear();
                attach(&mut child, row);
                Ok(child)
            })
            .collect();
    }
    Ok(records(snapshot, row)
        .into_iter()
        .map(|fact| {
            let mut child = fact.row.clone();
            child.presentation.file_path = file_path(snapshot, &child);
            child.occurrence = child_key(&row.occurrence, &child.occurrence);
            attach(&mut child, row);
            child
        })
        .collect())
}

fn child_key(parent: &str, identity: &str) -> String {
    format!(
        "detail:{}",
        blake3::hash(format!("{parent}\0{identity}").as_bytes()).to_hex()
    )
}

fn attach(child: &mut Row, parent: &Row) {
    child.timestamp = parent.timestamp;
    child.presentation.parent = Some(parent.occurrence.clone());
    child.presentation.session_label.clear();
    child.group = None;
    child.details = None;
    child.relationships.clear();
    child.graph = Geometry::default();
}

pub(super) fn decorate(child: &mut Row, parent: &Row) {
    child.graph = Geometry {
        lane: parent.graph.lane,
        above: parent.graph.below.clone(),
        below: parent.graph.below.clone(),
        muted_above: parent.graph.muted_below.clone(),
        muted_below: parent.graph.muted_below.clone(),
        ..Geometry::default()
    };
}
