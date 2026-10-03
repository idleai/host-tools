//! Bounded provider reducers and the engine's durable schema-three admission.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use editchain_git::RepositoryCatalog;
use editchain_store::{LogStore, SegmentStore};
use idle_history_import::{
    BufferedBlobSink, FsBlobSink, FsCursorStore, ImportError, ImportOptions,
    batch::ImportBatch,
    codex::{
        CodexDiscoveryRequest, HelperCommand, RepositoryLookup,
        live::{LiveCodex, LiveSinks},
    },
};

use crate::{Collector, Update};

impl Collector {
    pub(crate) fn reconcile(&self) -> io::Result<usize> {
        use idle_history_import::batch::DurableOpSink as _;
        let mut writer = LogStore::new(SegmentStore::open_wait(
            &self.binding.chain,
            Duration::from_secs(2),
        )?);
        let links =
            crate::git_links::derive(&self.binding.workspace, &self.binding.chain, &[], true)?;
        Ok(writer
            .append_durable(&links)
            .map_err(io::Error::other)?
            .written)
    }

    pub(crate) fn collect(&mut self, paths: &[PathBuf]) -> io::Result<Update> {
        let paths = selected_paths(&self.binding.sessions, paths)?;
        let repositories = Repositories(RepositoryCatalog::discover(&self.binding.workspace)?);
        let mut store = LogStore::new(SegmentStore::open_wait(
            &self.binding.chain,
            Duration::from_secs(2),
        )?);
        let migrated = idle_history_import::activity::uses_migration_ids(&self.binding.chain)?;
        let mut blobs = BufferedBlobSink::new(FsBlobSink::new(self.binding.chain.join("blobs"))?);
        let mut cursors = FsCursorStore::new(self.binding.chain.join("cursors-v3"))?;
        if self.provider.is_none() {
            self.provider = Some(
                LiveCodex::start(&HelperCommand::new(
                    self.binding.helper.to_string_lossy().into_owned(),
                    Vec::new(),
                ))
                .map_err(io::Error::other)?,
            );
        }
        let provider = self
            .provider
            .as_mut()
            .ok_or_else(|| io::Error::other("collector provider unavailable"))?;
        let discovery = CodexDiscoveryRequest {
            workspace_path: self.binding.workspace.clone(),
            raw_root: self.binding.sessions.clone(),
            selected_paths: paths,
            repositories: &repositories,
        };
        let options = ImportOptions::default();
        let batch = ImportBatch::capture_bounded(&cursors, options.batch_limits, |ops, pending| {
            provider.capture(
                &discovery,
                &options,
                LiveSinks {
                    ops,
                    blobs: &mut blobs,
                    cursors: pending,
                },
            )
        })
        .map_err(io::Error::other)?;
        let batch = if migrated {
            batch.into_migrated_schema3(&mut blobs)
        } else {
            batch.into_schema3(&mut blobs)
        }
        .map_err(io::Error::other)?;
        blobs.flush().map_err(io::Error::other)?;
        let links = crate::git_links::derive(
            &self.binding.workspace,
            &self.binding.chain,
            batch.operations(),
            false,
        )?;
        let outcome = batch
            .extend_operations(links)
            .map_err(io::Error::other)?
            .persist(&mut store, &mut cursors)
            .map_err(io::Error::other)?;
        Ok(Update {
            pending: provider.work.pending,
            written: outcome.admission.written,
            duplicates: outcome.admission.duplicates,
            conflicts: outcome.admission.conflicts,
            source_bytes: provider.work.source_bytes,
            ..Update::default()
        })
    }
}

fn selected_paths(root: &Path, paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if paths.len() > 32 {
        return Err(io::Error::other("collector batch exceeds 32 sources"));
    }
    let root = std::fs::canonicalize(root)?;
    paths
        .iter()
        .map(|path| {
            let path = std::fs::canonicalize(path)?;
            if !path.starts_with(&root)
                || path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "source is outside the bound rollout directory",
                ));
            }
            Ok(path)
        })
        .collect()
}

#[derive(Debug)]
struct Repositories(RepositoryCatalog);

impl RepositoryLookup for Repositories {
    fn repository_for_cwd(
        &self,
        cwd: &Path,
    ) -> Result<Option<editchain_core::RepositoryId>, ImportError> {
        if !self.0.is_complete() {
            return Err(ImportError::OpSink(
                "incomplete collector repository catalog".into(),
            ));
        }
        Ok(self
            .0
            .repository_for_path(cwd)
            .map(|repository| repository.id))
    }
}
