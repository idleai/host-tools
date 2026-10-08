//! Persistent bounded Activity queries over accepted current and retained records.
//!
//! Native reads release both checkpoint and engine handles at every request.
//! Copy-on-write pages keep the previous complete revision readable during builds.

mod build;
mod facts;
mod git;
mod groups;
mod logical;
mod model;
mod presentation;
mod project;
mod providers;
mod read;
mod routing;
#[cfg(test)]
mod tests;

use std::io;

use editchain_engine::queries::ChainQueries;
use editchain_index::{Map, OrderedSet, Storage, boundary};
use idle_history::timeline::{Action, Progress, Request, Response, VERSION, View};

use crate::history::service::Binding;
use model::{Build, Checkpoint, INDEX_VERSION, Phase, Snapshot};

/// Execute a timeline request against storage locations installed by the host.
///
/// # Errors
/// Rejects unsupported versions, invalid cursors, inaccessible storage and
/// corrupt source records. Derived pages can always be rebuilt from recordings.
pub fn execute(binding: &Binding, request: &Request) -> io::Result<Response> {
    if request.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This host does not support the requested Activity timeline version.",
        ));
    }
    let storage = Storage::open(&binding.chain_directory.join("activity-timeline-v1"))?;
    let mut checkpoint: Checkpoint = storage.load().or_else(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::InvalidData
        ) {
            Ok(Checkpoint::default())
        } else {
            Err(error)
        }
    })?;
    if checkpoint.version != INDEX_VERSION {
        checkpoint = Checkpoint::default();
    }
    boundary(|| handle(binding, &request.action, &mut checkpoint)).and_then(|result| {
        // Only edited pages are written; window reads leave the checkpoint cold.
        if !matches!(request.action, Action::Members { .. }) || checkpoint.build.is_some() {
            let _cold: Checkpoint = storage.commit(&checkpoint)?;
        }
        Ok(result)
    })?
}

fn handle(binding: &Binding, action: &Action, checkpoint: &mut Checkpoint) -> io::Result<Response> {
    if let Action::Cancel { build } = action {
        if checkpoint
            .build
            .as_ref()
            .is_some_and(|pending| pending.id == *build)
        {
            checkpoint.build = None;
        }
        return Ok(Response::Cancelled);
    }
    let queries = ChainQueries::open(&binding.chain_directory)?;
    let revision = queries.index().revision().clone();
    let retained = binding
        .retained_directory
        .as_ref()
        .map(|path| {
            let reader = ChainQueries::open(path)?;
            Ok::<_, io::Error>(reader.index().revision().clone())
        })
        .transpose()?;
    let observed = crate::git::observe(binding);
    let same_archive = |snapshot: &Snapshot| {
        snapshot.retained == retained && snapshot.retained_directory == binding.retained_directory
    };
    if checkpoint.build.as_ref().is_some_and(|build| {
        !same_archive(&build.snapshot) || build.snapshot.git_source != observed
    }) {
        checkpoint.build = None;
    }
    if checkpoint.build.is_none()
        && checkpoint.ready.as_ref().is_none_or(|ready| {
            ready.accepted != revision
                || !same_archive(ready)
                || ready.git_source != observed
                || !ready.summaries_ready
                || !ready.routes_ready
        })
    {
        checkpoint.serial = checkpoint
            .serial
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Activity revision exhausted"))?;
        let (snapshot, phase) = if let Some(ready) = &checkpoint.ready
            && same_archive(ready)
            && queries.index().changes_since(&ready.accepted, 1)?.is_some()
        {
            (
                if ready.routes_ready {
                    ready.clone()
                } else {
                    routing::prepare(ready)
                },
                if !ready.routes_ready {
                    Phase::Routes { after: None }
                } else if ready.summaries_ready {
                    Phase::Changes {
                        after: ready.accepted.clone(),
                    }
                } else {
                    Phase::Summaries { after: None }
                },
            )
        } else {
            (
                Snapshot {
                    accepted: revision.clone(),
                    retained: retained.clone(),
                    retained_directory: binding.retained_directory.clone(),
                    git_source: observed.clone(),
                    ..Snapshot::default()
                },
                Phase::Scan {
                    source: idle_history::timeline::Source::Current,
                    after: None,
                },
            )
        };
        let changed_git = snapshot.git_source != observed;
        let mut pending = Build {
            id: format!(
                "{}:{}:{}",
                INDEX_VERSION, revision.generation, checkpoint.serial
            ),
            snapshot,
            phase,
            processed: 0,
            ready: OrderedSet::default(),
            waiting: Map::default(),
            dirty: OrderedSet::default(),
            relation_dirty: OrderedSet::default(),
            group_dirty: OrderedSet::default(),
        };
        if changed_git {
            git::begin(&mut pending, observed, true);
        }
        checkpoint.build = Some(pending);
    }
    if let Action::Advance { build } = action
        && let Some(mut pending) = checkpoint.build.take()
    {
        if pending.id != *build {
            checkpoint.build = Some(pending);
            return Ok(Response::Stale);
        }
        if build::advance(binding, &queries, &mut pending)? {
            pending.snapshot.revision.clone_from(&pending.id);
            checkpoint.ready = Some(pending.snapshot);
            checkpoint.views.clear();
        } else {
            checkpoint.build = Some(pending);
        }
    }
    let progress = checkpoint
        .build
        .as_ref()
        .map(|build| progress(build, checkpoint.ready.as_ref()));
    let Some(snapshot) = &checkpoint.ready else {
        return Ok(progress.map_or(Response::Cancelled, Response::Building));
    };
    if matches!(action, Action::Advance { .. }) {
        if let Some(progress) = progress {
            return Ok(Response::Building(progress));
        }
        return read::window(
            snapshot,
            &View::default(),
            (
                &idle_history::timeline::Position::Latest,
                idle_history::timeline::DEFAULT_LIMIT,
            ),
            None,
            &mut checkpoint.views,
        );
    }
    read::execute(snapshot, action, progress, &mut checkpoint.views)
}

fn progress(build: &Build, ready: Option<&Snapshot>) -> Progress {
    Progress {
        build: build.id.clone(),
        stage: match build.phase {
            Phase::Git(_) => "Reading Git history",
            Phase::Scan { .. } => "Reading recorded activities",
            Phase::Resolve { .. } | Phase::Relations => "Resolving recorded relationships",
            Phase::Materialize | Phase::MaterializeChanges => "Updating logical activities",
            Phase::Project { .. } => "Resolving activity attachments",
            Phase::PrepareOrder { .. } | Phase::Order => "Ordering and routing activities",
            Phase::Group { .. } => "Preparing task groups",
            Phase::Changes { .. } | Phase::Repair => "Updating recorded activities",
            Phase::Summaries { .. } => "Preparing activity read pages",
            Phase::Routes { .. } => "Rebuilding Activity routes",
        }
        .into(),
        processed: build.processed,
        total: None,
        previous_revision: ready.map(|snapshot| snapshot.revision.clone()),
    }
}
