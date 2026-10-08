//! Select an unambiguous complete derivation tied to the full source hash.

use std::collections::BTreeSet;

use editchain_core::SourceId;
use idle_history::{
    provider::{CodexDerivationContract, CodexLogicalChange, ProviderFact},
    query::ActivityKind,
    timeline::Source,
};

use super::super::{
    model::{Fact, Snapshot, key},
    project,
};

pub(super) fn record(snapshot: &Snapshot, id: SourceId) -> Option<&Fact> {
    project::exact(snapshot, &key(Source::Current, id.id()))
        .or_else(|| project::exact(snapshot, &key(Source::Retained, id.id())))
}

pub(super) fn select(snapshot: &Snapshot, source: SourceId) -> Option<ProviderFact> {
    let raw = record(snapshot, source)?;
    let mut candidates = snapshot
        .logical
        .claims
        .get(&source)?
        .iter()
        .filter_map(|id| snapshot.facts.get(id)?.provider.as_ref())
        .filter(|contract| raw.raw_hash == Some(contract.raw_hash))
        .map(|contract| &contract.fact);
    let first = candidates.next()?;
    let mut chosen = first;
    let mut conflicting = false;
    for candidate in candidates {
        if std::mem::discriminant(candidate) != std::mem::discriminant(first) {
            return None;
        }
        match priority(candidate).cmp(&priority(chosen)) {
            std::cmp::Ordering::Greater => {
                chosen = candidate;
                conflicting = false;
            }
            std::cmp::Ordering::Equal => conflicting |= candidate != chosen,
            std::cmp::Ordering::Less => {}
        }
    }
    (!conflicting && complete(snapshot, source, chosen)).then(|| chosen.clone())
}

fn priority(meta: &ProviderFact) -> (bool, u8) {
    match meta {
        ProviderFact::CodexDerivation(meta) => (
            meta.includes_thinking,
            if meta.contract == CodexDerivationContract::OccurrencesV2 {
                2
            } else {
                1
            },
        ),
        ProviderFact::ClaudeDerivation(meta) => (meta.includes_thinking, 1),
        ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_) => (false, 0),
    }
}

fn complete(snapshot: &Snapshot, source: SourceId, meta: &ProviderFact) -> bool {
    let outputs: BTreeSet<_> = super::outputs(meta).iter().copied().collect();
    if outputs.len() != super::outputs(meta).len()
        || outputs.contains(&source)
        || !outputs
            .iter()
            .all(|id| reaches(snapshot, *id, source, &outputs))
    {
        return false;
    }
    match meta {
        ProviderFact::ClaudeDerivation(_) => true,
        ProviderFact::CodexDerivation(meta) => {
            !meta.thread.0.is_empty()
                && meta.changes.iter().all(|change| match change {
                    CodexLogicalChange::RemoveTurn { turn } => !turn.is_empty(),
                    CodexLogicalChange::Upsert {
                        turn,
                        item,
                        incarnation,
                        outputs: owned,
                    } => {
                        !turn.is_empty()
                            && !item.is_empty()
                            && incarnation.node == source.node
                            && incarnation.boot == source.boot
                            && incarnation.seq > 0
                            && incarnation.seq.trailing_zeros() >= 16
                            && incarnation.seq <= source.seq
                            && record(snapshot, *incarnation)
                                .is_none_or(|fact| fact.row.kind == ActivityKind::Original)
                            && owned.iter().all(|id| outputs.contains(id))
                    }
                })
        }
        ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_) => false,
    }
}

fn reaches(
    snapshot: &Snapshot,
    mut id: SourceId,
    source: SourceId,
    outputs: &BTreeSet<SourceId>,
) -> bool {
    if id.node == source.node || id.boot != source.boot || id.seq >> 16 != source.seq >> 16 {
        return false;
    }
    let mut seen = BTreeSet::new();
    while id != source {
        if !outputs.contains(&id) || !seen.insert(id) {
            return false;
        }
        let Some(fact) = record(snapshot, id) else {
            return false;
        };
        if fact.row.kind == ActivityKind::Original
            || fact.provider.is_some()
            || fact.parents.len() != 1
        {
            return false;
        }
        let Some(parent) = fact
            .parents
            .first()
            .and_then(|parent| project::exact(snapshot, parent))
            .and_then(|parent| parent.source)
        else {
            return false;
        };
        id = parent;
    }
    true
}
