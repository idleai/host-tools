//! Resumable HEAD ancestry, separate from immutable recorded operations.

mod rows;

use std::{collections::BTreeSet, io};

use editchain_core::GitOid;
use editchain_git::RefSnapshot;
use editchain_index::OrderedSet;
use serde::{Deserialize, Serialize};

use super::{
    facts,
    model::{BATCH, Build, Phase},
    read,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Walk {
    pending: Vec<String>,
    seen: OrderedSet<String>,
    previous_head: Option<String>,
    reached: bool,
    incremental: bool,
    retire: bool,
}

pub(super) fn begin(
    build: &mut Build,
    observed: Option<crate::git::Observation>,
    incremental: bool,
) {
    let previous = std::mem::replace(&mut build.snapshot.git_source, observed);
    let current = &build.snapshot.git_source;
    let same_repository = previous
        .as_ref()
        .map(|value| (&value.root, value.repository))
        == current
            .as_ref()
            .map(|value| (&value.root, value.repository));
    let retire = incremental && !same_repository;
    let pending = current
        .as_ref()
        .and_then(|value| value.head.clone())
        .into_iter()
        .collect();
    let previous_head = previous.and_then(|value| value.head);
    refresh_labels(build);
    build.phase = Phase::Git(Walk {
        pending,
        seen: OrderedSet::default(),
        previous_head,
        reached: false,
        incremental,
        retire,
    });
}

pub(super) fn advance(build: &mut Build, mut walk: Walk) -> io::Result<()> {
    if let Some(message) = build
        .snapshot
        .git_source
        .as_ref()
        .and_then(|source| source.gap.as_ref())
    {
        let _inserted = build.snapshot.gaps.insert(message.clone());
        complete(build, &walk);
        return Ok(());
    }
    if walk.retire {
        retire(build, &mut walk);
    } else {
        resolve(build, &mut walk)?;
    }
    if !walk.retire && walk.pending.is_empty() {
        if walk.incremental && walk.previous_head.is_some() && !walk.reached {
            walk.retire = true;
            walk.previous_head = None;
            walk.seen.clear();
        } else {
            complete(build, &walk);
            return Ok(());
        }
    }
    build.phase = Phase::Git(walk);
    Ok(())
}

fn retire(build: &mut Build, walk: &mut Walk) {
    for _ in 0..BATCH {
        let Some(id) = build.snapshot.live_git.iter().next().cloned() else {
            walk.retire = false;
            walk.pending = build
                .snapshot
                .git_source
                .as_ref()
                .and_then(|value| value.head.clone())
                .into_iter()
                .collect();
            walk.seen.clear();
            walk.previous_head = None;
            return;
        };
        super::build::dirty(build, &id);
        if let Some(node) = build.snapshot.nodes.get(&id).cloned() {
            read::index_order(&mut build.snapshot, &node, true);
        }
        facts::remove(&mut build.snapshot, &id);
        let _removed = build.snapshot.live_git.remove(&id);
    }
}

fn resolve(build: &mut Build, walk: &mut Walk) -> io::Result<()> {
    let Some(source) = build.snapshot.git_source.clone() else {
        return Ok(());
    };
    let Some(repository) = source.repository else {
        return Ok(());
    };
    let handle = crate::git::open(&source.root)?;
    for _ in 0..BATCH {
        let Some(oid) = walk.pending.pop() else {
            break;
        };
        if !walk.seen.insert(oid.clone()) {
            continue;
        }
        let id = format!("git:{repository}:{oid}");
        if build.snapshot.live_git.contains(&id) {
            walk.reached |= walk.previous_head.as_ref() == Some(&oid);
            continue;
        }
        let hash = GitOid::from_hex(&oid)
            .ok_or_else(|| io::Error::other("Invalid indexed commit hash."))?;
        match editchain_git::resolve::resolve_commit_with_refs(
            &handle,
            &hash,
            &RefSnapshot::default(),
        ) {
            Ok(commit) => {
                walk.pending
                    .extend(commit.parents.iter().map(ToString::to_string));
                let (fact, text) = rows::fact(
                    &commit,
                    source.labels.get(&oid).cloned().unwrap_or_default(),
                );
                facts::install(&mut build.snapshot, fact, text);
                let _inserted = build.snapshot.live_git.insert(id.clone());
                if walk.incremental {
                    super::build::dirty(build, &id);
                }
            }
            Err(error) => {
                let _inserted = build
                    .snapshot
                    .gaps
                    .insert(format!("Commit {oid} is unavailable: {error}"));
            }
        }
        build.processed = build.processed.saturating_add(1);
    }
    Ok(())
}

fn complete(build: &mut Build, walk: &Walk) {
    build.phase = if walk.incremental {
        Phase::Changes {
            after: build.snapshot.accepted.clone(),
        }
    } else {
        Phase::Materialize
    };
    build.processed = 0;
}

fn refresh_labels(build: &mut Build) {
    let Some(source) = &build.snapshot.git_source else {
        return;
    };
    let Some(repository) = source.repository else {
        return;
    };
    let keys: BTreeSet<_> = build
        .snapshot
        .live_git_labels
        .iter()
        .cloned()
        .chain(source.labels.keys().cloned())
        .collect();
    let labels = source.labels.clone();
    build.snapshot.live_git_labels = labels.keys().cloned().collect();
    for oid in keys {
        let id = format!("git:{repository}:{oid}");
        if let Some(fact) = build.snapshot.facts.get_mut(&id) {
            rows::labels(fact, labels.get(&oid).cloned().unwrap_or_default());
            super::build::dirty(build, &id);
        }
    }
}
