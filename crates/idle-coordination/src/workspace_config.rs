//! Repository-backed authored configuration with bounded reads and atomic saves.

mod files;
mod validation;

use std::{collections::BTreeMap, path::Path};

use idle_protocol::v1::{
    standalone::RepositorySnapshot,
    workspace_config::{WorkspaceConfiguration, WorkspaceManifest},
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, persistence::Persistence};

pub(crate) use files::FileWrite;
pub(crate) use validation::projection_document;
pub(crate) use validation::projection_path;

/// A complete bounded file observation. Kept privately for revision/retry recovery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Observation {
    pub documents: BTreeMap<String, String>,
    pub configuration: WorkspaceConfiguration,
}

impl Observation {
    pub(crate) fn from_documents(documents: BTreeMap<String, String>) -> Result<Self> {
        files::check_limits(&documents)?;
        let configuration = validation::decode(&documents)?;
        Ok(Self {
            documents,
            configuration,
        })
    }
}

/// Shared native adapter; callers supply a checkout, never a UI state directory.
#[derive(Debug)]
pub struct RepositoryFiles {
    directory: std::path::PathBuf,
}

impl RepositoryFiles {
    /// Bind the `.idle/workspace` directory of one existing checkout.
    ///
    /// # Errors
    /// Rejects missing roots and symlinked configuration directories.
    pub fn open(checkout: &Path) -> Result<Self> {
        let root = checkout.canonicalize()?;
        if !root.is_dir() {
            return Err(Error::Invalid);
        }
        let store = Self {
            directory: root.join(".idle/workspace"),
        };
        store.validate_directories()?;
        Ok(store)
    }

    /// Read a validated configuration without starting a runtime or creating files.
    ///
    /// # Errors
    /// Reports missing manifests, invalid JSON, unknown versions and bounded-read failures.
    pub fn read(&self) -> Result<WorkspaceConfiguration> {
        Ok(self.observe()?.configuration)
    }

    pub(crate) fn observe(&self) -> Result<Observation> {
        let documents = self.documents()?;
        Observation::from_documents(documents)
    }

    pub(crate) fn initialize(
        &self,
        snapshot: &RepositorySnapshot,
        storage: &dyn Persistence,
    ) -> Result<()> {
        const KEY: &str = "workspace-seed";
        let _guard = self.lock()?;
        let saved = storage.load(KEY)?;
        if saved.is_none() && self.read_file("workspace.json")?.is_some() {
            return Ok(());
        }
        let documents: BTreeMap<String, String> = if let Some(bytes) = &saved {
            serde_json::from_slice(bytes)?
        } else {
            if !self.documents()?.is_empty() {
                return Err(Error::Conflict);
            }
            let manifest = WorkspaceManifest {
                schema_version: 1,
                id: idle_protocol::v1::identity::WorkspaceId(format!(
                    "workspace:{}",
                    uuid::Uuid::new_v4()
                )),
                name: snapshot.workspace.value.name.clone(),
                extensions: BTreeMap::new(),
            };
            let mut documents = BTreeMap::from([
                ("workspace.json".into(), pretty(&manifest)?),
                ("control.json".into(), "{}\n".into()),
                ("hosts.json".into(), "{\"hosts\": []}\n".into()),
                ("providers.json".into(), "{\"providers\": []}\n".into()),
            ]);
            for (name, record) in [
                ("settings.json", &snapshot.settings),
                ("agent-rules.json", &snapshot.agent_rules),
            ] {
                if let Some(record) = record {
                    if record.value.schema_version != 1 {
                        return Err(Error::Version);
                    }
                    let _old = documents.insert(name.into(), record.value.json.clone());
                }
            }
            for view in &snapshot.views {
                let _old = documents.insert(
                    projection_path(&view.value.id)?,
                    projection_document(&view.value, None)?,
                );
            }
            let _validated = validation::decode(&documents)?;
            storage.compare_exchange(KEY, None, Some(&serde_json::to_vec(&documents)?))?;
            documents
        };
        for (name, content) in &documents {
            self.finish_write(&FileWrite {
                name: name.clone(),
                before: None,
                after: content.clone(),
            })?;
        }
        let persisted = storage.load(KEY)?.ok_or(Error::Storage)?;
        storage.compare_exchange(KEY, Some(&persisted), None)
    }
}

pub(crate) fn pretty(value: &impl Serialize) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}
