//! Resumable construction and small-delta maintenance of timeline checkpoints.

use std::{
    collections::BTreeSet,
    io,
    ops::Bound::{Excluded, Unbounded},
};

use editchain_engine::queries::{ChainQueries, Lookup, PageRequest};
use editchain_index::rank::Measure;
use history_geometry::live::GraphNode;
use idle_history::timeline::Source;

use super::{
    facts, groups, logical,
    model::{BATCH, Build, Node, Phase, Snapshot, key},
    project, providers, read,
};
use crate::history::service::Binding;

pub(super) fn advance(
    binding: &Binding,
    queries: &ChainQueries,
    build: &mut Build,
) -> io::Result<bool> {
    match build.phase.clone() {
        Phase::Git(walk) => super::git::advance(build, walk)?,
        Phase::Scan { source, after } => {
            let retained;
            let reader = if source == Source::Retained {
                let Some(path) = &binding.retained_directory else {
                    return Err(io::Error::other("Retained history is unavailable."));
                };
                let reader = ChainQueries::open(path)?;
                retained = reader;
                build.snapshot.retained = Some(retained.index().revision().clone());
                &retained
            } else {
                queries
            };
            let page = reader.history(
                None,
                PageRequest {
                    after,
                    limit: BATCH,
                },
            )?;
            for entry in page.items {
                if let Some((fact, text)) = facts::read(reader, &entry, source)? {
                    facts::install(&mut build.snapshot, fact, text);
                }
                build.processed = build.processed.saturating_add(1);
            }
            if let Some(after) = page.next_after {
                build.phase = Phase::Scan {
                    source,
                    after: Some(after),
                };
            } else if source == Source::Current && binding.retained_directory.is_some() {
                build.phase = Phase::Scan {
                    source: Source::Retained,
                    after: None,
                };
            } else {
                super::git::begin(build, build.snapshot.git_source.clone(), false);
                build.processed = 0;
            }
        }
        Phase::Materialize => {
            let _changed = logical::resolve(&mut build.snapshot);
            if !logical::pending(&build.snapshot) {
                build.phase = Phase::Resolve { after: None };
            }
        }
        Phase::MaterializeChanges => {
            for id in logical::resolve(&mut build.snapshot) {
                dirty(build, &id);
            }
            if !logical::pending(&build.snapshot) {
                build.phase = Phase::Relations;
            }
        }
        Phase::Resolve { after } => {
            let keys: Vec<_> = build
                .snapshot
                .links
                .range((after.map_or(Unbounded, Excluded), Unbounded))
                .take(BATCH)
                .cloned()
                .collect();
            let _affected = providers::resolve(&mut build.snapshot, &keys);
            build.processed = build
                .processed
                .saturating_add(u64::try_from(keys.len()).unwrap_or(u64::MAX));
            build.phase = if keys.len() < BATCH {
                build.processed = 0;
                Phase::Project { after: None }
            } else {
                Phase::Resolve {
                    after: keys.last().cloned(),
                }
            };
        }
        Phase::Project { after } => {
            let keys = keys_after(&build.snapshot.facts, after);
            for id in &keys {
                if let Some(fact) = build.snapshot.facts.get(id)
                    && let Some(node) = project::node(&build.snapshot, fact)
                {
                    set_node(&mut build.snapshot, node);
                }
            }
            build.processed = build
                .processed
                .saturating_add(u64::try_from(keys.len()).unwrap_or(u64::MAX));
            build.phase = if keys.len() < BATCH {
                Phase::PrepareOrder { after: None }
            } else {
                Phase::Project {
                    after: keys.last().cloned(),
                }
            };
        }
        Phase::PrepareOrder { after } => {
            let keys = keys_after(&build.snapshot.nodes, after);
            for id in &keys {
                let Some(node) = build.snapshot.nodes.get(id) else {
                    continue;
                };
                let parents = u64::try_from(
                    node.parents
                        .iter()
                        .filter(|parent| build.snapshot.nodes.contains_key(*parent))
                        .count(),
                )
                .unwrap_or(u64::MAX);
                if parents == 0 {
                    let _inserted = build.ready.insert((node.clock, id.clone()));
                } else {
                    let _old = build.waiting.insert(id.clone(), parents);
                }
            }
            build.phase = if keys.len() < BATCH {
                build.processed = 0;
                Phase::Order
            } else {
                Phase::PrepareOrder {
                    after: keys.last().cloned(),
                }
            };
        }
        Phase::Order => order(build)?,
        Phase::Routes { after } => super::routing::advance(build, after),
        Phase::Summaries { after } => {
            let mut cursor = after;
            for _ in 0..BATCH {
                let next = build
                    .snapshot
                    .order
                    .bound(cursor.as_ref().map_or(Unbounded, Excluded), false)
                    .map(|(order, id)| (order.clone(), id.clone()));
                let Some((order, id)) = next else {
                    build.snapshot.summaries_ready = true;
                    return Ok(finish(queries, build));
                };
                if let Some(summary) = build
                    .snapshot
                    .nodes
                    .get(&id)
                    .and_then(|node| read::summary(&build.snapshot, node))
                {
                    drop(build.snapshot.summaries.insert(order.clone(), summary));
                }
                cursor = Some(order);
                build.processed = build.processed.saturating_add(1);
            }
            build.phase = Phase::Summaries { after: cursor };
        }
        Phase::Group { after } => {
            let mut cursor = after;
            let mut keys = Vec::new();
            for _ in 0..BATCH {
                let next = build
                    .snapshot
                    .order
                    .bound(cursor.as_ref().map_or(Unbounded, Excluded), true)
                    .map(|(order, id)| (order.clone(), id.clone()));
                let Some((order, id)) = next else {
                    break;
                };
                cursor = Some(order);
                keys.push(id);
            }
            groups::rebuild(&mut build.snapshot, &keys);
            build.processed = build
                .processed
                .saturating_add(u64::try_from(keys.len()).unwrap_or(u64::MAX));
            if keys.len() < BATCH {
                return Ok(finish(queries, build));
            }
            build.phase = Phase::Group { after: cursor };
        }
        Phase::Changes { after } => {
            let Some(changes) = queries.index().changes_since(&after, BATCH)? else {
                build.snapshot = Snapshot {
                    accepted: queries.index().revision().clone(),
                    retained: build.snapshot.retained.clone(),
                    retained_directory: binding.retained_directory.clone(),
                    git_source: crate::git::observe(binding),
                    ..Snapshot::default()
                };
                build.phase = Phase::Scan {
                    source: Source::Current,
                    after: None,
                };
                build.processed = 0;
                return Ok(false);
            };
            for change in changes.changes {
                let id = key(Source::Current, change.operation);
                dirty(build, &id);
                if let Some(node) = build.snapshot.nodes.get(&id).cloned() {
                    read::index_order(&mut build.snapshot, &node, true);
                }
                match queries.operation(change.operation)? {
                    Lookup::Found(entry) => {
                        if let Some((fact, text)) = facts::read(queries, &entry, Source::Current)? {
                            facts::install(&mut build.snapshot, fact, text);
                        }
                    }
                    Lookup::Missing | Lookup::Conflicted(_) => {
                        facts::remove(&mut build.snapshot, &id);
                    }
                }
                dirty(build, &id);
                build.processed = build.processed.saturating_add(1);
            }
            build.snapshot.accepted.clone_from(&changes.next);
            build.phase = if changes.complete {
                let keys: Vec<_> = build.relation_dirty.iter().cloned().collect();
                logical::schedule(&mut build.snapshot, &keys);
                Phase::MaterializeChanges
            } else {
                Phase::Changes {
                    after: changes.next,
                }
            };
        }
        Phase::Relations => {
            let keys: Vec<_> = build.relation_dirty.iter().take(BATCH).cloned().collect();
            for id in providers::resolve(&mut build.snapshot, &keys) {
                let _inserted = build.dirty.insert(id);
            }
            for id in keys {
                let _removed = build.relation_dirty.remove(&id);
            }
            if build.relation_dirty.is_empty() {
                build.phase = Phase::Repair;
            }
        }
        Phase::Repair => {
            let keys: Vec<_> = build.dirty.iter().take(BATCH).cloned().collect();
            repair(build, &keys)?;
            for id in &keys {
                let _removed = build.dirty.remove(id);
            }
            if build.dirty.is_empty() {
                let affected: Vec<_> = build.group_dirty.iter().cloned().collect();
                groups::rebuild(&mut build.snapshot, &affected);
                build.group_dirty.clear();
                return Ok(finish(queries, build));
            }
        }
    }
    Ok(false)
}

fn keys_after<V>(
    values: &editchain_index::OrderedMap<String, V>,
    after: Option<String>,
) -> Vec<String> {
    values
        .range((after.map_or(Unbounded, Excluded), Unbounded))
        .take(BATCH)
        .map(|(key, _)| key.clone())
        .collect()
}

pub(super) fn dirty(build: &mut Build, id: &str) {
    let _inserted = build.dirty.insert(id.to_owned());
    let mut identities = vec![id.to_owned()];
    if let Some(fact) = build.snapshot.facts.get(id) {
        identities.extend(facts::identities(fact));
        for key in providers::affected(&build.snapshot, fact) {
            let _inserted = build.relation_dirty.insert(key);
        }
        let original = fact.original.clone();
        if let Some(original) = original {
            let _inserted = build.dirty.insert(original.clone());
            for output in build.snapshot.outputs.get(&original).into_iter().flatten() {
                let _inserted = build.dirty.insert(output.clone());
            }
        }
    }
    let mut seen = BTreeSet::new();
    while let Some(identity) = identities.pop() {
        if !seen.insert(identity.clone()) {
            continue;
        }
        if build.snapshot.facts.contains_key(&identity) {
            let _inserted = build.dirty.insert(identity.clone());
        }
        for alias in build.snapshot.aliases.get(&identity).into_iter().flatten() {
            let _inserted = build.dirty.insert(alias.clone());
        }
        for child in build
            .snapshot
            .references
            .get(&identity)
            .into_iter()
            .flat_map(editchain_index::OrderedSet::iter)
        {
            let _inserted = build.dirty.insert(child.clone());
            if let Some(fact) = build.snapshot.facts.get(child) {
                if fact.link.is_some() {
                    let _inserted = build.relation_dirty.insert(child.clone());
                }
                if !project::visible(&build.snapshot, fact) {
                    identities.extend(facts::identities(fact));
                }
            }
        }
    }
}

fn finish(queries: &ChainQueries, build: &mut Build) -> bool {
    if build.snapshot.accepted == queries.index().revision() {
        true
    } else {
        build.phase = Phase::Changes {
            after: build.snapshot.accepted.clone(),
        };
        false
    }
}

fn order(build: &mut Build) -> io::Result<()> {
    let mut upserts = Vec::new();
    for _ in 0..BATCH {
        let Some(next) = build.ready.iter().next().cloned() else {
            break;
        };
        let _removed = build.ready.remove(&next);
        let Some(mut node) = build.snapshot.nodes.get(&next.1).cloned() else {
            continue;
        };
        for parent in &node.parents {
            if let Some(parent) = build.snapshot.nodes.get(parent) {
                node.clock = node.clock.max(
                    parent
                        .clock
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("Activity ordering clock exhausted."))?,
                );
            }
        }
        drop(build.snapshot.nodes.insert(node.key.clone(), node.clone()));
        drop(build.snapshot.order.insert(
            node.order(),
            node.key.clone(),
            Measure {
                expanded: 1,
                visible: 1,
            },
        ));
        read::index_order(&mut build.snapshot, &node, false);
        for child in build
            .snapshot
            .children
            .get(&node.key)
            .into_iter()
            .flat_map(editchain_index::OrderedSet::iter)
        {
            if let Some(waiting) = build.waiting.get_mut(child) {
                *waiting = waiting.saturating_sub(1);
                if *waiting == 0
                    && let Some(child) = build.snapshot.nodes.get(child)
                {
                    let _inserted = build.ready.insert((child.clock, child.key.clone()));
                }
            }
        }
        let _removed = build.waiting.remove(&node.key);
        upserts.push(node);
        build.processed = build.processed.saturating_add(1);
    }
    build.snapshot.graph.edit_stream(&[], &upserts);
    if build.ready.is_empty() {
        if let Some((id, _)) = build.waiting.iter().next() {
            let id = id.clone();
            if let Some(mut node) = build.snapshot.nodes.get(&id).cloned() {
                for relation in &mut node.relationships {
                    if relation.parent.is_some() {
                        relation.unresolved =
                            Some("This recorded component contains a causal cycle.".into());
                    }
                }
                node.parents.clear();
                let _inserted = build.ready.insert((node.clock, id.clone()));
                drop(build.snapshot.nodes.insert(id, node));
                let _inserted = build
                    .snapshot
                    .gaps
                    .insert("A cyclic component is shown with unresolved attachments.".into());
            }
        } else {
            build.snapshot.summaries_ready = true;
            build.snapshot.routes_ready = true;
            build.phase = Phase::Group { after: None };
            build.processed = 0;
        }
    }
    Ok(())
}

fn set_node(snapshot: &mut Snapshot, node: Node) {
    if let Some(old) = snapshot.nodes.get(&node.key) {
        for parent in &old.parents {
            if let Some(children) = snapshot.children.get_mut(parent) {
                let _removed = children.remove(&old.key);
            }
        }
    }
    for parent in &node.parents {
        let _inserted = snapshot
            .children
            .entry(parent.clone())
            .or_default()
            .insert(node.key.clone());
    }
    drop(snapshot.nodes.insert(node.key.clone(), node));
}

fn repair(build: &mut Build, keys: &[String]) -> io::Result<()> {
    let mut removed = Vec::new();
    let mut upserts = Vec::new();
    for key in keys {
        if let Some(fact) = build.snapshot.facts.get(key)
            && fact.row.kind == idle_history::query::ActivityKind::Turn
            && let Some(task) = &fact.task
        {
            for group in build
                .snapshot
                .task_groups
                .get(task)
                .into_iter()
                .flat_map(editchain_index::OrderedSet::iter)
            {
                if let Some(detail) = build.snapshot.group_details.get(group) {
                    let _inserted = build.group_dirty.insert(detail.exit.clone());
                }
            }
        }
        if let Some(node) = build
            .snapshot
            .facts
            .get(key)
            .and_then(|fact| project::node(&build.snapshot, fact))
        {
            upserts.push(node);
        } else if build.snapshot.nodes.contains_key(key) {
            removed.push(key.clone());
        }
    }
    let updates = match build.snapshot.graph.causal_updates(&upserts) {
        Ok(updates) => updates,
        Err(message) => {
            for node in &mut upserts {
                for relation in &mut node.relationships {
                    relation.unresolved = Some(message.clone());
                }
                node.parents.clear();
            }
            build
                .snapshot
                .graph
                .causal_updates(&upserts)
                .map_err(io::Error::other)?
        }
    };
    for key in removed.iter().chain(updates.iter().map(|node| &node.key)) {
        if let Some(old) = build.snapshot.nodes.get(key).cloned() {
            drop(build.snapshot.order.remove(&old.order()));
            read::index_order(&mut build.snapshot, &old, true);
        }
        let _inserted = build.group_dirty.insert(key.clone());
    }
    for id in &removed {
        if let Some(old) = build.snapshot.nodes.remove(id) {
            for parent in old.parents {
                if let Some(children) = build.snapshot.children.get_mut(&parent) {
                    let _removed = children.remove(id);
                }
            }
        }
    }
    for node in &updates {
        set_node(&mut build.snapshot, node.clone());
        drop(build.snapshot.order.insert(
            node.order(),
            node.key.clone(),
            Measure {
                expanded: 1,
                visible: 1,
            },
        ));
        read::index_order(&mut build.snapshot, node, false);
    }
    build.snapshot.graph.edit_stream(&removed, &updates);
    for key in build.snapshot.graph.changed_boundaries() {
        let _inserted = build.group_dirty.insert(key.clone());
    }
    // Neighboring intervals may acquire or lose a safe folding boundary.
    let neighbors: BTreeSet<_> = updates
        .iter()
        .flat_map(|node| node.parents.iter().cloned())
        .collect();
    for key in neighbors {
        let _inserted = build.group_dirty.insert(key);
    }
    Ok(())
}
