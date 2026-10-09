use std::{path::Path, sync::Arc};

use idle_protocol::v1::{
    Record,
    configuration::{ConfigurationDocument, ConfigurationValue},
    identity::Revision,
    standalone::{ChangeNotice, Mutation},
    workspace_config::WorkspaceConfiguration,
};
use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    clock::Clock,
    persistence::{MAX_STATE_BYTES, Persistence},
    workspace_config::{
        FileWrite, Observation, RepositoryFiles, pretty, projection_document, projection_path,
    },
};

use super::{
    Authority, Bootstrap,
    state::{STATE_KEY, State},
};

const JOURNAL: &str = "workspace-write";

#[derive(Debug, Deserialize, Serialize)]
struct PendingWrite {
    previous_hash: String,
    next: String,
    write: FileWrite,
}

impl Authority {
    /// Open an authority backed by tracked definitions in one checkout.
    /// Existing authored definitions win over private cached configuration.
    ///
    /// # Errors
    /// Rejects invalid files, conflicting recovery, changed bindings and failed storage.
    pub fn open_repository(
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
        bootstrap: Bootstrap,
        checkout: &Path,
    ) -> Result<Self> {
        let repository = RepositoryFiles::open(checkout)?;
        recover(storage.as_ref(), &repository)?;
        let mut authority = Self::open(storage, clock, Some(bootstrap))?;
        if authority.state.repository_files.is_none() {
            if authority.storage.load("workspace-original")?.is_none() {
                authority.storage.compare_exchange(
                    "workspace-original",
                    None,
                    Some(&authority.persisted),
                )?;
            }
            repository.initialize(
                &authority.state.all_records(&authority.state.owner),
                authority.storage.as_ref(),
            )?;
        }
        authority.repository = Some(repository);
        if authority.state.runtime_transfer.is_none() {
            authority.refresh_repository()?;
        }
        Ok(authority)
    }

    /// Reconcile external edits/deletions before reads and conditional writes.
    /// Invalid files leave the last confirmed private state untouched and return an error.
    ///
    /// # Errors
    /// Rejects invalid definitions, identity changes and unavailable storage.
    pub fn refresh_repository(&mut self) -> Result<()> {
        let Some(repository) = &self.repository else {
            return Ok(());
        };
        self.healthy()?;
        let observation = repository.observe()?;
        if self.state.repository_files.as_deref() == Some(&observation)
            && self
                .state
                .resource_revisions
                .initialized(&observation.configuration)
        {
            return Ok(());
        }
        if self.state.handoff.is_some() || self.state.runtime_transfer.is_some() {
            return Err(Error::Conflict);
        }
        if self
            .state
            .repository_files
            .as_ref()
            .is_some_and(|previous| {
                previous.configuration.manifest.id != observation.configuration.manifest.id
            })
        {
            return Err(Error::Conflict);
        }
        let mut next = self.state.clone();
        if next.workspace.value.name != observation.configuration.manifest.name {
            next.workspace.revision = revision(&next, Some(next.workspace.revision))?;
            next.workspace
                .value
                .name
                .clone_from(&observation.configuration.manifest.name);
            next.record_change(ChangeNotice::Workspace)?;
        }
        sync_configuration(
            &mut next,
            ConfigurationDocument::Settings,
            observation.configuration.settings.as_ref(),
        )?;
        sync_configuration(
            &mut next,
            ConfigurationDocument::AgentRules,
            observation.configuration.agent_rules.as_ref(),
        )?;
        sync_views(&mut next, &observation)?;
        next.sync_resources(&observation.configuration)?;
        next.record_change(ChangeNotice::Directory)?;
        next.repository_files = Some(Box::new(observation));
        next.validate()?;
        self.commit(next)
    }

    /// Read the current authored documents, including their stable identity and digest.
    ///
    /// # Errors
    /// Returns unavailable for an authority without a repository binding.
    pub fn workspace_configuration(&self) -> Result<&WorkspaceConfiguration> {
        self.healthy()?;
        self.state
            .repository_files
            .as_ref()
            .map(|files| &files.configuration)
            .ok_or(Error::Invalid)
    }

    pub(super) fn prepare_file_write(
        &self,
        next: &mut State,
        mutation: &Mutation,
    ) -> Result<Option<FileWrite>> {
        let Some(previous) = &self.state.repository_files else {
            return Ok(None);
        };
        let (name, after) = match mutation {
            Mutation::Workspace(change) => {
                let mut manifest = previous.configuration.manifest.clone();
                manifest.name.clone_from(&change.value.name);
                ("workspace.json".into(), pretty(&manifest)?)
            }
            Mutation::Configuration(write) => (
                document_name(write.document).to_owned(),
                write.change.value.json.clone(),
            ),
            Mutation::View(change) => {
                let name = projection_path(&change.value.id)?;
                let content = projection_document(&change.value, previous.documents.get(&name))?;
                (name, content)
            }
            Mutation::Session(_)
            | Mutation::Host(_)
            | Mutation::Provider(_)
            | Mutation::Membership(_)
            | Mutation::Grant(_)
            | Mutation::Control(_) => return Ok(None),
        };
        let write = FileWrite {
            before: previous.documents.get(&name).cloned(),
            name: name.clone(),
            after: after.clone(),
        };
        let mut documents = previous.documents.clone();
        let _old = documents.insert(name, after);
        next.repository_files = Some(Box::new(Observation::from_documents(documents)?));
        Ok(Some(write))
    }

    pub(super) fn commit_repository(
        &mut self,
        next: State,
        bytes: Vec<u8>,
        write: FileWrite,
    ) -> Result<()> {
        let repository = self.repository.as_ref().ok_or(Error::Invalid)?;
        let _guard = repository.lock()?;
        // The same precondition covers direct editor changes and other local clients.
        if repository.observe()?.documents
            != self
                .state
                .repository_files
                .as_ref()
                .ok_or(Error::Invalid)?
                .documents
        {
            return Err(Error::Conflict);
        }
        let journal = PendingWrite {
            previous_hash: blake3::hash(&self.persisted).to_hex().to_string(),
            next: String::from_utf8(bytes.clone()).map_err(|_error| Error::Invalid)?,
            write,
        };
        let encoded = serde_json::to_vec(&journal)?;
        if encoded.len() > MAX_STATE_BYTES {
            return Err(Error::Invalid);
        }
        // Until the journal is cleared, only reopening may resolve an uncertain
        // write. A later read must not commit a new observation over its base.
        self.faulted = true;
        self.storage
            .compare_exchange(JOURNAL, None, Some(&encoded))?;
        if let Err(error) = repository.finish_write(&journal.write) {
            // A conflicting editor save was not applied; do not leave a replayable write.
            if error == Error::Conflict {
                self.storage
                    .compare_exchange(JOURNAL, Some(&encoded), None)?;
                self.faulted = false;
            }
            return Err(error);
        }
        self.commit_serialized(next, bytes)?;
        self.storage
            .compare_exchange(JOURNAL, Some(&encoded), None)?;
        self.faulted = false;
        Ok(())
    }
}

pub(super) fn recover(storage: &dyn Persistence, repository: &RepositoryFiles) -> Result<()> {
    let Some(bytes) = storage.load(JOURNAL)? else {
        return Ok(());
    };
    let _guard = repository.lock()?;
    let journal: PendingWrite = serde_json::from_slice(&bytes)?;
    let current = storage.load(STATE_KEY)?.ok_or(Error::Storage)?;
    if current != journal.next.as_bytes() {
        if blake3::hash(&current).to_hex().as_str() != journal.previous_hash {
            return Err(Error::Conflict);
        }
        repository.finish_write(&journal.write)?;
        storage.compare_exchange(STATE_KEY, Some(&current), Some(journal.next.as_bytes()))?;
    }
    storage.compare_exchange(JOURNAL, Some(&bytes), None)
}

fn document_name(document: ConfigurationDocument) -> &'static str {
    match document {
        ConfigurationDocument::Settings => "settings.json",
        ConfigurationDocument::AgentRules => "agent-rules.json",
    }
}

fn sync_configuration(
    state: &mut State,
    document: ConfigurationDocument,
    json: Option<&String>,
) -> Result<()> {
    // Deleting a file resets its logical document to a newer empty revision.
    // Keep that revision while the file remains absent, including after restart.
    if json.is_none()
        && state.repository_files.as_ref().is_some_and(|files| {
            match document {
                ConfigurationDocument::Settings => &files.configuration.settings,
                ConfigurationDocument::AgentRules => &files.configuration.agent_rules,
            }
            .is_none()
        })
    {
        return Ok(());
    }
    if state
        .configuration
        .get(&document)
        .map(|record| &record.value.json)
        == json
    {
        return Ok(());
    }
    let revision = revision(
        state,
        state
            .configuration
            .get(&document)
            .map(|record| record.revision),
    )?;
    let _old = state.configuration.insert(
        document,
        Record {
            revision,
            value: ConfigurationValue {
                schema_version: 1,
                json: json.cloned().unwrap_or_else(|| "{}".into()),
            },
        },
    );
    state.record_change(ChangeNotice::Configuration(document))
}

fn sync_views(state: &mut State, observation: &Observation) -> Result<()> {
    let mut next = std::collections::BTreeMap::new();
    for view in &observation.configuration.projections {
        let previous = state.views.get(&view.id);
        let record = if let Some(previous) = previous.filter(|record| &record.value == view) {
            previous.clone()
        } else {
            Record {
                revision: revision(state, previous.map(|record| record.revision))?,
                value: view.clone(),
            }
        };
        let _old = next.insert(view.id.clone(), record);
    }
    if state.views != next {
        state.views = next;
        state.record_change(ChangeNotice::Views)?;
    }
    Ok(())
}

fn revision(state: &State, previous: Option<Revision>) -> Result<Revision> {
    state
        .sequence
        .max(previous.map_or(0, |revision| revision.0))
        .checked_add(1)
        .map(Revision)
        .ok_or(Error::Invalid)
}
