//! Provider and Git relationships resolved from exact recorded identifiers.

mod registry;

use std::{
    collections::BTreeSet,
    ops::Bound::{Included, Unbounded},
};

use editchain_core::SourceId;
use idle_history::{
    provider::{CodexLifecycleEvent, CodexSourceEvidence, ProviderFact},
    timeline::{Relationship, RelationshipKind, Source},
};

use super::{
    model::{Connection, Fact, Snapshot, key},
    project,
};
pub(super) use registry::{Registry, affected};

pub(super) fn index(snapshot: &mut Snapshot, fact: &Fact, remove: bool) {
    registry::edit(snapshot, fact, remove);
}

pub(super) fn resolve(snapshot: &mut Snapshot, keys: &[String]) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    for id in keys {
        let next = snapshot
            .facts
            .get(id)
            .map_or_else(Vec::new, |record| relations(snapshot, record));
        let old = snapshot.contributions.get(id).cloned().unwrap_or_default();
        if old == next {
            continue;
        }
        for (child, relation) in old {
            if let Some(relations) = snapshot.extra.get_mut(&child) {
                relations.retain(|stored| stored != &relation);
            }
            let _inserted = changed.insert(child);
        }
        for (child, relation) in &next {
            snapshot
                .extra
                .entry(child.clone())
                .or_default()
                .push(relation.clone());
            let _inserted = changed.insert(child.clone());
        }
        if next.is_empty() {
            drop(snapshot.contributions.remove(id));
        } else {
            drop(snapshot.contributions.insert(id.clone(), next));
        }
    }
    changed
}

fn relations(snapshot: &Snapshot, record: &Fact) -> Vec<(String, Connection)> {
    let Some(contract) = &record.provider else {
        return typed(snapshot, record);
    };
    if !valid(snapshot, record) {
        return Vec::new();
    }
    if let ProviderFact::CodexSource(meta) = &contract.fact
        && meta.forked_from.is_some()
    {
        if meta.parent == meta.forked_from
            && meta.parent.as_ref().is_some_and(|parent| {
                unique_activation(snapshot, &parent.0, &meta.thread.0).is_some()
            })
        {
            return Vec::new();
        }
        return connect(
            snapshot,
            record,
            (meta.first, None),
            RelationshipKind::Fork,
            "The source names a forked thread without an exact divergence record.",
        );
    }
    let ProviderFact::CodexLifecycle(event) = &contract.fact else {
        return Vec::new();
    };
    match &event.event {
        CodexLifecycleEvent::Spawn {
            activation, child, ..
        } => {
            let extent = extent(snapshot, &child.0);
            let first = extent
                .as_ref()
                .filter(|meta| meta.parent.as_ref() == Some(&event.thread))
                .filter(|_| {
                    unique_activation(snapshot, &event.thread.0, &child.0) == Some(*activation)
                })
                .map(|meta| meta.first);
            connect(
                snapshot,
                record,
                (first.unwrap_or(contract.source), first.map(|_| *activation)),
                RelationshipKind::Spawn,
                "Child activation or source generation is unavailable or ambiguous.",
            )
        }
        CodexLifecycleEvent::Completed { child } => {
            completion(snapshot, record, contract.source, &child.0)
        }
        CodexLifecycleEvent::LegacyCompleted { agent_path } => {
            let candidates: Vec<_> = snapshot
                .registry
                .paths
                .get(&(event.thread.0.clone(), agent_path.clone()))
                .into_iter()
                .flat_map(editchain_index::OrderedMap::iter)
                .filter(|(_, ids)| {
                    ids.iter().any(|id| {
                        snapshot
                            .facts
                            .get(id)
                            .is_some_and(|fact| valid(snapshot, fact))
                    })
                })
                .take(2)
                .map(|(child, _)| child)
                .collect();
            completion(
                snapshot,
                record,
                contract.source,
                if candidates.len() == 1 {
                    candidates.first().map_or("", |value| value.as_str())
                } else {
                    ""
                },
            )
        }
    }
}

fn unique_activation(snapshot: &Snapshot, parent: &str, child: &str) -> Option<SourceId> {
    let mut candidates = snapshot
        .registry
        .activations
        .get(&(parent.into(), child.into()))?
        .iter()
        .filter_map(|(_, ids)| {
            ids.iter().find_map(|id| {
                let fact = snapshot.facts.get(id)?;
                if !valid(snapshot, fact) {
                    return None;
                }
                let ProviderFact::CodexLifecycle(event) = &fact.provider.as_ref()?.fact else {
                    return None;
                };
                let CodexLifecycleEvent::Spawn { activation, .. } = event.event else {
                    return None;
                };
                Some(activation)
            })
        });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

fn extent(snapshot: &Snapshot, thread: &str) -> Option<CodexSourceEvidence> {
    let generations = snapshot.registry.sources.get(thread)?;
    let mut result = None;
    for (_, generation) in generations.iter() {
        if generation.prefixes.is_empty() {
            continue;
        }
        if generation.descriptions.len() != 1 {
            return None;
        }
        let (_, id) = generation.prefixes.first_key_value()?;
        let contract = snapshot.facts.get(id)?.provider.as_ref()?;
        let ProviderFact::CodexSource(meta) = &contract.fact else {
            return None;
        };
        let through = registry::complete_through(snapshot, meta.first);
        let bound = (through, String::from(char::MAX));
        let candidate = generation
            .prefixes
            .range((Unbounded, Included(bound)))
            .rev()
            .filter_map(|(_, id)| snapshot.facts.get(id))
            .find(|record| valid(snapshot, record));
        let Some(contract) = candidate.and_then(|record| record.provider.as_ref()) else {
            continue;
        };
        let ProviderFact::CodexSource(meta) = &contract.fact else {
            return None;
        };
        if meta.first.seq != 65_536
            || meta.last.seq < meta.first.seq
            || meta.first.node != meta.last.node
            || meta.first.boot != meta.last.boot
            || meta.last.seq.trailing_zeros() < 16
            || generation
                .hashes
                .get(&meta.last.seq)
                .is_none_or(|hashes| hashes.len() != 1)
            || result.is_some()
        {
            return None;
        }
        result = Some((**meta).clone());
    }
    result
}

fn raw(snapshot: &Snapshot, source: SourceId) -> Option<&Fact> {
    project::exact(snapshot, &key(Source::Current, source.id()))
        .or_else(|| project::exact(snapshot, &key(Source::Retained, source.id())))
}

fn valid(snapshot: &Snapshot, record: &Fact) -> bool {
    record.provider.as_ref().is_some_and(|contract| {
        raw(snapshot, contract.source).is_some_and(|fact| fact.raw_hash == Some(contract.raw_hash))
    })
}

fn completion(
    snapshot: &Snapshot,
    record: &Fact,
    occurrence: SourceId,
    thread: &str,
) -> Vec<(String, Connection)> {
    let parent = record.provider.as_ref().and_then(|contract| {
        if let ProviderFact::CodexLifecycle(event) = &contract.fact {
            Some(&event.thread)
        } else {
            None
        }
    });
    let terminal = extent(snapshot, thread)
        .filter(|meta| meta.parent.as_ref() == parent)
        .filter(|_| {
            parent
                .and_then(|parent| unique_activation(snapshot, &parent.0, thread))
                .is_some_and(|activation| {
                    activation.node == occurrence.node
                        && activation.boot == occurrence.boot
                        && activation.seq <= occurrence.seq
                })
        })
        .and_then(|meta| {
            // An explicit terminal fixes the execution boundary even after a resume.
            snapshot
                .terminals
                .get(&(meta.first.node.0, meta.first.boot))
                .into_iter()
                .flat_map(|values| values.range(meta.first.seq..=meta.last.seq))
                .find_map(|(_, id)| snapshot.facts.get(id).and_then(|fact| fact.source))
                .or_else(|| {
                    snapshot
                        .registry
                        .sources
                        .get(thread)
                        .and_then(|values| values.get(&(meta.first.node.0, meta.first.boot)))
                        .filter(|generation| generation.ends.len() == 1)
                        .map(|_| meta.last)
                })
        });
    connect(
        snapshot,
        record,
        (occurrence, terminal),
        RelationshipKind::Completion,
        "A unique recorded child execution boundary is unavailable.",
    )
}

fn connect(
    snapshot: &Snapshot,
    record: &Fact,
    endpoints: (SourceId, Option<SourceId>),
    kind: RelationshipKind,
    gap: &str,
) -> Vec<(String, Connection)> {
    let children = raw(snapshot, endpoints.0)
        .map(|fact| {
            if kind == RelationshipKind::Spawn || kind == RelationshipKind::Fork {
                project::starts(snapshot, &fact.row.occurrence)
            } else {
                project::endpoints(snapshot, &fact.row.occurrence, "")
            }
        })
        .unwrap_or_default();
    let parents = endpoints
        .1
        .and_then(|parent| raw(snapshot, parent))
        .map(|fact| project::endpoints(snapshot, &fact.row.occurrence, ""))
        .unwrap_or_default();
    attach(record, children, &parents, kind, gap)
}

fn typed(snapshot: &Snapshot, record: &Fact) -> Vec<(String, Connection)> {
    let Some((from, to, relation)) = &record.link else {
        return Vec::new();
    };
    let kind = match relation.as_str() {
        "ProviderParent" | "LogicalParent" | "provider_parent" | "logical_parent" => {
            RelationshipKind::Parent
        }
        "SpawnedBy" | "spawned_by" => RelationshipKind::Spawn,
        "ReconnectsTo" | "reconnects_to" => RelationshipKind::Completion,
        "ForkedFrom" | "forked_from" => RelationshipKind::Fork,
        "BasedOn" | "based_on" | "Checkpoint" | "checkpoint" | "CommittedAs" | "committed_as" => {
            RelationshipKind::Git
        }
        _ => return Vec::new(),
    };
    let children = project::entity(snapshot, record, from);
    to.iter()
        .flat_map(|target| {
            let mut connections = attach(
                record,
                children.clone(),
                &project::entity(snapshot, record, target),
                kind,
                "The typed relationship endpoint is unavailable or ambiguous.",
            );
            if let editchain_core::activity::Entity::Git { repository, oid } = target {
                for (_, connection) in &mut connections {
                    if connection.relation.parent.is_none() {
                        connection.boundary = Some(format!("git:{}:{oid}", repository.0));
                    }
                }
            }
            connections
        })
        .collect()
}

fn attach(
    record: &Fact,
    children: Vec<String>,
    parents: &[String],
    kind: RelationshipKind,
    gap: &str,
) -> Vec<(String, Connection)> {
    let mut result = Vec::new();
    for child in children {
        if parents.is_empty() {
            result.push((
                child,
                Connection {
                    relation: project::unresolved(record, kind, gap),
                    boundary: None,
                },
            ));
        } else {
            for parent in parents {
                result.push((
                    child.clone(),
                    Connection {
                        relation: Relationship {
                            kind,
                            parent: Some(parent.clone()),
                            source: record.row.address.clone(),
                            unresolved: None,
                        },
                        boundary: None,
                    },
                ));
            }
        }
    }
    result
}
