//! Source stamps advance only after the complete bounded import succeeds.

use std::{collections::BTreeMap, io, path::PathBuf};

use crate::{
    Binding, Mode, Poll, Update,
    discovery::{self, GitState, Sources, Stamp},
};

#[derive(Debug)]
struct Batch {
    sources: Sources,
    picked: Vec<PathBuf>,
    request: Poll,
}

#[derive(Debug, Default)]
pub(crate) struct Schedule {
    accepted: BTreeMap<PathBuf, Stamp>,
    titles: Option<Stamp>,
    git: Option<GitState>,
    pending: Option<Batch>,
    mode: Option<Mode>,
    seen: bool,
}

impl Schedule {
    pub(crate) fn request(&mut self, binding: &Binding, mode: Mode) -> io::Result<Poll> {
        if self.mode != Some(mode) {
            self.pending = None;
            self.mode = Some(mode);
        }
        if self.pending.is_none() {
            let sources = discovery::capture(binding, mode)?;
            self.prepare(sources, |path| {
                discovery::belongs_to_workspace(path, &binding.workspace)
            })?;
        }
        self.pending
            .as_ref()
            .map(|batch| batch.request.clone())
            .ok_or_else(|| io::Error::other("collection batch is unavailable"))
    }

    fn prepare(
        &mut self,
        sources: Sources,
        select: impl Fn(&std::path::Path) -> io::Result<bool>,
    ) -> io::Result<()> {
        let titles_changed = self.titles != sources.titles;
        let picked: Vec<_> = sources
            .newest_first()
            .into_iter()
            .filter(|path| titles_changed || self.accepted.get(*path) != sources.files.get(*path))
            .take(32)
            .cloned()
            .collect();
        let mut paths = Vec::new();
        for path in &picked {
            if select(path)? {
                paths.push(path.clone());
            }
        }
        let request = Poll {
            paths,
            git_changed: self.git.as_ref() != Some(&sources.git),
        };
        self.pending = Some(Batch {
            sources,
            picked,
            request,
        });
        Ok(())
    }

    pub(crate) fn complete(&mut self, update: &mut Update) {
        update.changed |= !self.seen;
        self.seen = true;
        if update.pending {
            if let Some(batch) = &mut self.pending {
                batch.request.git_changed = false;
            }
            return;
        }
        let Some(batch) = self.pending.take() else {
            return;
        };
        if self.titles == batch.sources.titles {
            self.accepted
                .retain(|path, _stamp| batch.sources.files.contains_key(path));
        } else {
            self.accepted.clear();
        }
        for path in batch.picked {
            if let Some(stamp) = batch.sources.files.get(&path) {
                let _previous = self.accepted.insert(path, stamp.clone());
            }
        }
        update.pending = batch
            .sources
            .files
            .iter()
            .any(|(path, stamp)| self.accepted.get(path) != Some(stamp));
        self.titles = batch.sources.titles;
        self.git = Some(batch.sources.git);
    }

    pub(crate) fn failed(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
