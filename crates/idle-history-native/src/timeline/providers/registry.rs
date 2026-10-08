//! Persistent lookups for source generations and lifecycle dependencies.

use std::collections::BTreeSet;

use editchain_core::SourceId;
use editchain_index::{Map, OrderedMap, OrderedSet};
use idle_history::provider::{CodexLifecycleEvent, ProviderFact};
use serde::{Deserialize, Serialize};

use super::super::model::{Fact, Snapshot};

type Correlations = Map<(String, String), OrderedMap<String, OrderedSet<String>>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(in super::super) struct Registry {
    pub sources: Map<String, OrderedMap<(u64, u32), Generation>>,
    pub threads: Map<(u64, u32), OrderedSet<String>>,
    pub activations: Correlations,
    pub paths: Correlations,
    pub children: Map<String, OrderedSet<String>>,
    pub legacy: Map<String, OrderedSet<String>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(in super::super) struct Generation {
    pub descriptions: OrderedMap<String, u64>,
    pub prefixes: OrderedMap<(u64, String), String>,
    pub ends: OrderedMap<u64, u64>,
    pub hashes: Map<u64, OrderedMap<[u8; 32], u64>>,
}

pub(super) fn edit(snapshot: &mut Snapshot, fact: &Fact, remove: bool) {
    let Some(contract) = &fact.provider else {
        return;
    };
    let id = &fact.row.occurrence;
    match &contract.fact {
        ProviderFact::CodexSource(meta) => {
            let generation = (meta.first.node.0, meta.first.boot);
            let source = snapshot
                .registry
                .sources
                .entry(meta.thread.0.clone())
                .or_default()
                .entry(generation)
                .or_default();
            let description = format!(
                "{:?}/{:?}/{:?}/{:?}",
                meta.first, meta.parent, meta.forked_from, meta.agent_path
            );
            count(&mut source.descriptions, description, remove);
            count(&mut source.ends, meta.last.seq, remove);
            count(
                source.hashes.entry(meta.last.seq).or_default(),
                meta.prefix_hash,
                remove,
            );
            if remove {
                drop(source.prefixes.remove(&(meta.last.seq, id.clone())));
            } else {
                drop(
                    source
                        .prefixes
                        .insert((meta.last.seq, id.clone()), id.clone()),
                );
                let _inserted = snapshot
                    .registry
                    .threads
                    .entry(generation)
                    .or_default()
                    .insert(meta.thread.0.clone());
            }
        }
        ProviderFact::CodexLifecycle(event) => match &event.event {
            CodexLifecycleEvent::Spawn {
                activation,
                child,
                agent_path,
                ..
            } => {
                set(
                    snapshot
                        .registry
                        .activations
                        .entry((event.thread.0.clone(), child.0.clone()))
                        .or_default()
                        .entry(activation.to_string())
                        .or_default(),
                    id,
                    remove,
                );
                if let Some(path) = agent_path {
                    set(
                        snapshot
                            .registry
                            .paths
                            .entry((event.thread.0.clone(), path.clone()))
                            .or_default()
                            .entry(child.0.clone())
                            .or_default(),
                        id,
                        remove,
                    );
                }
                set(
                    snapshot
                        .registry
                        .children
                        .entry(child.0.clone())
                        .or_default(),
                    id,
                    remove,
                );
            }
            CodexLifecycleEvent::Completed { child } => {
                set(
                    snapshot
                        .registry
                        .children
                        .entry(child.0.clone())
                        .or_default(),
                    id,
                    remove,
                );
            }
            CodexLifecycleEvent::LegacyCompleted { .. } => {
                set(
                    snapshot
                        .registry
                        .legacy
                        .entry(event.thread.0.clone())
                        .or_default(),
                    id,
                    remove,
                );
            }
        },
        ProviderFact::CodexDerivation(_) | ProviderFact::ClaudeDerivation(_) => {}
    }
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

fn set(values: &mut OrderedSet<String>, id: &str, remove: bool) {
    if remove {
        let _removed = values.remove(id);
    } else {
        let _inserted = values.insert(id.to_owned());
    }
}

pub(in super::super) fn affected(snapshot: &Snapshot, fact: &Fact) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    if fact.link.is_some() {
        let _inserted = result.insert(fact.row.occurrence.clone());
    }
    if let Some(source) = fact.source {
        for thread in snapshot
            .registry
            .threads
            .get(&(source.node.0, source.boot))
            .into_iter()
            .flat_map(OrderedSet::iter)
        {
            children(snapshot, thread, &mut result);
        }
    }
    if let Some(contract) = &fact.provider {
        match &contract.fact {
            ProviderFact::CodexSource(meta) => children(snapshot, &meta.thread.0, &mut result),
            ProviderFact::CodexLifecycle(event) => {
                if let CodexLifecycleEvent::Spawn { child, .. } = &event.event {
                    children(snapshot, &child.0, &mut result);
                    result.extend(
                        snapshot
                            .registry
                            .legacy
                            .get(&event.thread.0)
                            .into_iter()
                            .flat_map(OrderedSet::iter)
                            .cloned(),
                    );
                }
            }
            ProviderFact::CodexDerivation(_) | ProviderFact::ClaudeDerivation(_) => {}
        }
    }
    result
}

fn children(snapshot: &Snapshot, thread: &str, result: &mut BTreeSet<String>) {
    let records = snapshot
        .registry
        .children
        .get(thread)
        .into_iter()
        .flat_map(OrderedSet::iter);
    for id in records {
        let _inserted = result.insert(id.clone());
        if let Some(contract) = snapshot
            .facts
            .get(id)
            .and_then(|fact| fact.provider.as_ref())
            && let ProviderFact::CodexLifecycle(event) = &contract.fact
        {
            result.extend(
                snapshot
                    .registry
                    .legacy
                    .get(&event.thread.0)
                    .into_iter()
                    .flat_map(OrderedSet::iter)
                    .cloned(),
            );
        }
    }
}

pub(super) fn complete_through(snapshot: &Snapshot, first: SourceId) -> u64 {
    let Some(generation) = snapshot.generations.get(&(first.node.0, first.boot)) else {
        return 0;
    };
    let mut low = 0;
    let mut high = generation.last_key_value().map_or(0, |(seq, _)| seq >> 16);
    while low < high {
        let middle = low.saturating_add(high.saturating_sub(low).div_ceil(2));
        let sequence = middle << 16;
        let count = generation
            .count_before(&sequence)
            .saturating_add(u64::from(generation.contains_key(&sequence)));
        if count == middle {
            low = middle;
        } else {
            high = middle.saturating_sub(1);
        }
    }
    low << 16
}
