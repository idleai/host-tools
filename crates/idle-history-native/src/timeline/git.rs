//! Resumable HEAD and recorded commit ancestry in the explicitly bound repository.

mod rows;

use std::{collections::BTreeSet, io};

use editchain_core::GitOid;
use editchain_git::RefSnapshot;
use editchain_index::OrderedSet;
use serde::{Deserialize, Serialize};

use super::{
    facts,
    model::{BATCH, Build, Phase, Snapshot},
    read,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Walk {
    pending: Vec<String>,
    seen: OrderedSet<String>,
    previous_head: Option<String>,
    reached: bool,
    resume: Resume,
    retire: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Resume {
    Materialize,
    Changes,
    MaterializeChanges,
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
    let pending = roots(&build.snapshot);
    let previous_head = previous.and_then(|value| value.head);
    refresh_labels(build);
    build.phase = Phase::Git(Walk {
        pending,
        seen: OrderedSet::default(),
        previous_head,
        reached: false,
        resume: if incremental {
            Resume::Changes
        } else {
            Resume::Materialize
        },
        retire,
    });
}

pub(super) fn changed(build: &mut Build) {
    let Some(repository) = build
        .snapshot
        .git_source
        .as_ref()
        .and_then(|source| source.repository)
    else {
        build.phase = Phase::MaterializeChanges;
        return;
    };
    let pending: Vec<_> = roots(&build.snapshot)
        .into_iter()
        .filter(|oid| {
            !build
                .snapshot
                .live_git
                .contains(&format!("git:{repository}:{oid}"))
        })
        .collect();
    build.phase = if pending.is_empty() {
        Phase::MaterializeChanges
    } else {
        Phase::Git(Walk {
            pending,
            seen: OrderedSet::default(),
            previous_head: None,
            reached: true,
            resume: Resume::MaterializeChanges,
            retire: false,
        })
    };
}

fn roots(snapshot: &Snapshot) -> Vec<String> {
    let Some(source) = &snapshot.git_source else {
        return Vec::new();
    };
    let Some(repository) = source.repository else {
        return Vec::new();
    };
    let prefix = format!("git:{repository}:");
    let recorded = snapshot.references.iter().filter_map(|(id, references)| {
        let oid = id.strip_prefix(&prefix)?;
        (GitOid::from_hex(oid).is_some()
            && references.iter().any(|reference| {
                snapshot
                    .facts
                    .get(reference)
                    .is_some_and(|fact| fact.row.address.record().is_some())
            }))
        .then(|| oid.to_owned())
    });
    source
        .head
        .iter()
        .cloned()
        .chain(recorded)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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
        if walk.resume == Resume::Changes && walk.previous_head.is_some() && !walk.reached {
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
            walk.pending = roots(&build.snapshot);
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
                if walk.resume != Resume::Materialize {
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
    build.phase = match walk.resume {
        Resume::Changes => Phase::Changes {
            after: build.snapshot.accepted.clone(),
        },
        Resume::Materialize => Phase::Materialize,
        Resume::MaterializeChanges => Phase::MaterializeChanges,
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
