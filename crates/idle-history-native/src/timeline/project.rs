//! Explicit occurrence attachments, independent of logical-item summaries.

use std::collections::BTreeSet;

use editchain_core::activity::Entity;
use idle_history::{
    query::ActivityKind,
    timeline::{Relationship, RelationshipKind},
};

use super::{
    logical,
    model::{Fact, Node, Snapshot, key},
};

pub(super) fn visible(snapshot: &Snapshot, fact: &Fact) -> bool {
    if !fact.is_primary()
        || logical::hidden(snapshot, fact)
        || annotated_observation(snapshot, fact)
    {
        return false;
    }
    if annotated_observation(snapshot, origin(snapshot, fact))
        || (!fact.is_projected()
            && fact.row.kind == ActivityKind::File
            && origin(snapshot, fact).human_edit.is_some())
    {
        return false;
    }
    if fact
        .row
        .address
        .record()
        .is_some_and(|address| address.source == idle_history::timeline::Source::Retained)
        && snapshot
            .aliases
            .get(&fact.row.occurrence)
            .into_iter()
            .flatten()
            .filter_map(|id| snapshot.facts.get(id))
            .any(|record| {
                record.row.address.record().is_some_and(|address| {
                    address.source == idle_history::timeline::Source::Current
                }) && record.row.kind == fact.row.kind
            })
    {
        return false;
    }
    match fact.row.kind {
        ActivityKind::Author | ActivityKind::Link | ActivityKind::Initialization => false,
        ActivityKind::Original => !snapshot
            .outputs
            .get(&fact.row.occurrence)
            .into_iter()
            .flatten()
            .filter_map(|id| snapshot.facts.get(id))
            .any(|output| {
                !matches!(
                    output.row.kind,
                    ActivityKind::Author
                        | ActivityKind::Link
                        | ActivityKind::Initialization
                        | ActivityKind::Original
                )
            }),
        ActivityKind::Session
        | ActivityKind::Turn
        | ActivityKind::Message
        | ActivityKind::Tool
        | ActivityKind::File
        | ActivityKind::Commit
        | ActivityKind::Note
        | ActivityKind::Unknown => true,
    }
}

fn annotated_observation(snapshot: &Snapshot, fact: &Fact) -> bool {
    snapshot
        .supporting
        .get(&fact.row.occurrence)
        .into_iter()
        .flatten()
        .filter_map(|id| snapshot.facts.get(id))
        .any(Fact::is_observation)
}

pub(super) fn origin<'a>(snapshot: &'a Snapshot, fact: &'a Fact) -> &'a Fact {
    fact.original
        .as_ref()
        .and_then(|id| exact(snapshot, id))
        .or_else(|| {
            (fact.is_legacy() && fact.row.kind != ActivityKind::Original && fact.parents.len() == 1)
                .then(|| fact.parents.first().and_then(|id| exact(snapshot, id)))
                .flatten()
                .filter(|parent| parent.row.kind == ActivityKind::Original)
        })
        .unwrap_or(fact)
}

pub(super) fn session<'a>(snapshot: &'a Snapshot, fact: &'a Fact) -> Option<&'a String> {
    fact.session
        .as_ref()
        .or_else(|| origin(snapshot, fact).session.as_ref())
}

pub(super) fn exact<'a>(snapshot: &'a Snapshot, id: &str) -> Option<&'a Fact> {
    // Converted Originals carry explicit old-address aliases. Prefer a unique
    // current Original when an archive supplies the older provider metadata.
    let candidates = snapshot.aliases.get(id);
    if let Some(candidates) = candidates {
        for source in [
            idle_history::timeline::Source::Current,
            idle_history::timeline::Source::Retained,
        ] {
            let mut originals = candidates
                .iter()
                .filter_map(|key| snapshot.facts.get(key))
                .filter(|fact| {
                    fact.row.kind == ActivityKind::Original
                        && fact
                            .row
                            .address
                            .record()
                            .is_some_and(|address| address.source == source)
                });
            if let Some(first) = originals.next()
                && originals.next().is_none()
            {
                return Some(first);
            }
        }
    }
    snapshot.facts.get(id).or_else(|| {
        candidates
            .filter(|values| values.len() == 1)
            .and_then(|values| values.first())
            .and_then(|key| snapshot.facts.get(key))
    })
}

pub(super) fn endpoints(snapshot: &Snapshot, id: &str, exclude: &str) -> Vec<String> {
    walk(snapshot, id, exclude, false)
}

/// A recorded source starts at its first displayed descendant, after hidden setup records.
pub(super) fn starts(snapshot: &Snapshot, id: &str) -> Vec<String> {
    let mut pending = vec![id.to_owned()];
    let mut seen = BTreeSet::new();
    let mut result = BTreeSet::new();
    while let Some(id) = pending.pop() {
        let Some(fact) = exact(snapshot, &id) else {
            continue;
        };
        if !seen.insert(fact.row.occurrence.clone()) {
            continue;
        }
        let owners: Vec<_> = logical::owners(snapshot, &fact.row.occurrence, true)
            .into_iter()
            .filter(|id| {
                snapshot
                    .facts
                    .get(id)
                    .is_some_and(|fact| visible(snapshot, fact))
            })
            .collect();
        if !owners.is_empty() {
            result.extend(owners);
            continue;
        }
        if visible(snapshot, fact) {
            let _inserted = result.insert(fact.row.occurrence.clone());
            continue;
        }
        for reference in std::iter::once(&fact.row.occurrence).chain(&fact.aliases) {
            pending.extend(
                snapshot
                    .references
                    .get(reference)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| snapshot.facts.get(id))
                    .filter(|child| {
                        child.parents.iter().any(|parent| {
                            exact(snapshot, parent)
                                .is_some_and(|parent| parent.row.occurrence == fact.row.occurrence)
                        })
                    })
                    .map(|child| child.row.occurrence.clone()),
            );
        }
    }
    result.into_iter().collect()
}

fn walk(snapshot: &Snapshot, id: &str, exclude: &str, ancestry: bool) -> Vec<String> {
    let mut pending = vec![id.to_owned()];
    let mut seen = BTreeSet::new();
    let mut result = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let owners: Vec<_> = logical::owners(snapshot, &id, ancestry)
            .into_iter()
            .filter(|id| {
                snapshot
                    .facts
                    .get(id)
                    .is_some_and(|fact| visible(snapshot, fact))
            })
            .collect();
        if owners.iter().any(|owner| owner != exclude) {
            result.extend(owners.into_iter().filter(|owner| owner != exclude));
            continue;
        }
        let Some(fact) = exact(snapshot, &id) else {
            continue;
        };
        if fact.row.occurrence == exclude {
            continue;
        }
        if visible(snapshot, fact) {
            let _inserted = result.insert(fact.row.occurrence.clone());
        } else if fact.row.kind == ActivityKind::Original {
            let mut has_output = false;
            for output in snapshot
                .outputs
                .get(&fact.row.occurrence)
                .into_iter()
                .flatten()
            {
                if snapshot
                    .facts
                    .get(output)
                    .is_some_and(|record| visible(snapshot, record))
                {
                    pending.push(output.clone());
                    has_output = true;
                }
            }
            if !has_output {
                pending.extend(fact.parents.iter().cloned());
            }
        } else {
            pending.extend(fact.parents.iter().cloned());
        }
    }
    result.into_iter().collect()
}

pub(super) fn node(snapshot: &Snapshot, fact: &Fact) -> Option<Node> {
    if !visible(snapshot, fact) {
        return None;
    }
    let mut relations = Vec::new();
    let mut physical = fact.parents.clone();
    let original = origin(snapshot, fact);
    if original.row.occurrence != fact.row.occurrence {
        physical.retain(|parent| parent != &original.row.occurrence);
        physical.extend(original.parents.iter().cloned());
    }
    for parent in physical {
        attach(
            snapshot,
            fact,
            &parent,
            Attachment::Ancestry,
            &mut relations,
        );
    }
    for item in &fact.causes {
        let candidates = snapshot.items.get(item);
        if let Some(candidates) = candidates.filter(|values| values.len() == 1) {
            if let Some(parent) = candidates.iter().next() {
                attach(snapshot, fact, parent, Attachment::Cause, &mut relations);
            }
        } else {
            relations.push(unresolved(
                fact,
                RelationshipKind::Parent,
                "The logical cause has no unique recorded occurrence.",
            ));
        }
    }
    if let Some((_, parents)) = &fact.git {
        for parent in parents {
            if let Some(parent) = unique_git(snapshot, parent) {
                attach(snapshot, fact, &parent, Attachment::Git, &mut relations);
            } else {
                relations.push(unresolved(
                    fact,
                    RelationshipKind::Git,
                    "The recorded parent commit is unavailable or ambiguous.",
                ));
            }
        }
    }
    relations.extend(
        snapshot
            .extra
            .get(&fact.row.occurrence)
            .into_iter()
            .flatten()
            .map(|connection| connection.relation.clone()),
    );
    relations.sort_by(|left, right| left.parent.cmp(&right.parent));
    relations.dedup();
    let mut parents: Vec<_> = relations
        .iter()
        .filter_map(|relation| relation.parent.clone())
        .chain(
            snapshot
                .extra
                .get(&fact.row.occurrence)
                .into_iter()
                .flatten()
                .filter_map(|connection| connection.boundary.clone()),
        )
        .filter(|parent| parent != &fact.row.occurrence)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // Repository anchors establish routing before their accompanying work parents.
    parents.sort_by_key(|parent| {
        !parent.starts_with("git:")
            && snapshot
                .facts
                .get(parent)
                .is_none_or(|fact| fact.git.is_none())
    });
    Some(Node {
        key: fact.row.occurrence.clone(),
        parents,
        clock: fact.order_time.or(fact.row.timestamp).unwrap_or(0),
        source: session(snapshot, fact)
            .cloned()
            .unwrap_or_else(|| origin(snapshot, fact).recorder.clone()),
        source_closed: fact.terminal
            || fact.source.is_some_and(|source| {
                snapshot
                    .terminals
                    .get(&(source.node.0, source.boot))
                    .and_then(editchain_index::OrderedMap::last_key_value)
                    .is_some_and(|(sequence, _)| *sequence >= source.seq)
            }),
        git: fact.git.is_some(),
        protected: fact.protected
            || fact.task.is_none()
            || relations
                .iter()
                .any(|relation| relation.unresolved.is_some()),
        relationships: relations,
    })
}

#[derive(Clone, Copy)]
enum Attachment {
    Ancestry,
    Cause,
    Git,
}

fn attach(
    snapshot: &Snapshot,
    fact: &Fact,
    parent: &str,
    attachment: Attachment,
    relations: &mut Vec<Relationship>,
) {
    let kind = match attachment {
        Attachment::Git => RelationshipKind::Git,
        Attachment::Ancestry | Attachment::Cause => RelationshipKind::Parent,
    };
    let parents = walk(
        snapshot,
        parent,
        &fact.row.occurrence,
        matches!(attachment, Attachment::Ancestry),
    );
    if parents.is_empty() && exact(snapshot, parent).is_none() {
        relations.push(unresolved(
            fact,
            kind,
            "The recorded parent is unavailable or conflicted.",
        ));
    }
    for parent in parents {
        relations.push(Relationship {
            kind,
            parent: Some(parent),
            source: fact.row.address.clone(),
            unresolved: None,
        });
    }
}

pub(super) fn unresolved(fact: &Fact, kind: RelationshipKind, message: &str) -> Relationship {
    Relationship {
        kind,
        parent: None,
        source: fact.row.address.clone(),
        unresolved: Some(message.to_owned()),
    }
}

pub(super) fn unique_git(snapshot: &Snapshot, git: &str) -> Option<String> {
    let candidates = snapshot.git.get(git)?;
    // Duplicate observations of the same repository-qualified Git object share
    // one deterministic attachment anchor, while all recordings stay readable.
    candidates.first().cloned()
}

pub(super) fn entity(snapshot: &Snapshot, fact: &Fact, endpoint: &Entity) -> Vec<String> {
    match endpoint {
        Entity::Operation(id) => fact.row.address.record().map_or_else(Vec::new, |address| {
            endpoints(snapshot, &key(address.source, id), "")
        }),
        Entity::Item(item) => snapshot
            .items
            .get(&item.to_string())
            .filter(|items| items.len() == 1)
            .into_iter()
            .flat_map(|items| items.iter().flat_map(|id| endpoints(snapshot, id, "")))
            .collect(),
        Entity::Git { repository, oid } => unique_git(snapshot, &format!("{}:{oid}", repository.0))
            .into_iter()
            .collect(),
    }
}
