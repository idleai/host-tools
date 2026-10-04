//! Standalone native process configuration, without application or editor bindings.

use std::{path::PathBuf, sync::Arc};

use idle_protocol::v1::{
    identity::ContributorIdentity, sessions::RuntimeBinding, workspace::Workspace,
};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    authority::{Authority, Bootstrap, Principal},
    clock::SystemClock,
    dev_tunnels::{DevTunnels, EnvironmentCredentials},
    discovery::{DirectorySync, GitHubDirectory},
    engine::Engine,
    peer::{PeerCoordinator, PeerOptions},
    persistence::FilePersistence,
    transport::Credentials,
};

use super::Service;

/// Trusted startup binding read from the local process owner's configuration file.
/// Never accept this structure from a remote client. It contains no bearer tokens.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Configuration {
    /// Private state directory, exclusively owned for this process lifetime.
    pub state_directory: PathBuf,
    /// Existing local chain storage; logical identity comes from `workspace.chain`.
    pub chain_directory: PathBuf,
    /// Persistent private engine device key directory.
    pub device_directory: PathBuf,
    /// Stable repository/chain binding, checked again on every restart.
    pub workspace: Workspace,
    /// Host-authenticated local process owner; fixed for the entire connection.
    pub contributor: ContributorIdentity,
    /// Optional host-authenticated runtime; omitted for metadata-only clients.
    pub runtime: Option<RuntimeBinding>,
    /// Environment variable containing a private GitHub management credential.
    /// Omit to use `IDLE_TUNNELS_GITHUB_TOKEN`; guest connections do not read it.
    pub credential_variable: Option<String>,
    /// Retrieve credentials on demand through this private framed host connection.
    #[serde(default)]
    pub host_credentials: bool,
    /// Explicit GitHub `owner/repository` directory. Omit to disable discovery.
    pub discovery_repository: Option<String>,
    /// Explicitly resume retained consent/grants after restart, without new approval.
    #[serde(default)]
    pub resume_sharing: bool,
}

impl Configuration {
    /// Open native storage, authority, SDK relay and engine adapters.
    ///
    /// # Errors
    /// Rejects changed identities, corrupt/private state, duplicate owners or failed cleanup.
    pub async fn open(self) -> Result<Service> {
        let credentials = Arc::new(EnvironmentCredentials {
            variable: self
                .credential_variable
                .clone()
                .unwrap_or_else(|| "IDLE_TUNNELS_GITHUB_TOKEN".into()),
        });
        self.open_with_credentials(credentials).await
    }

    /// Open using an authenticated host's credential adapter.
    ///
    /// # Errors
    /// Rejects invalid storage, bindings or failed startup recovery.
    pub async fn open_with_credentials(self, credentials: Arc<dyn Credentials>) -> Result<Service> {
        let storage = Arc::new(FilePersistence::open(&self.state_directory)?);
        let clock = Arc::new(SystemClock);
        let principal = Principal {
            contributor: self.contributor,
            runtime: self.runtime,
        };
        let authority = Authority::open(
            storage.clone(),
            clock.clone(),
            Some(Bootstrap {
                workspace: self.workspace,
                owner: principal.contributor.contributor_id.clone(),
            }),
        )?;
        let engine = Engine {
            chain: self.chain_directory,
            device_directory: self.device_directory,
        };
        let relay = Arc::new(DevTunnels::new(
            credentials.clone(),
            storage.clone(),
            clock.clone(),
        )?);
        let mut peers = PeerCoordinator::open(PeerOptions {
            engine: engine.clone(),
            relay,
            credentials: credentials.clone(),
            storage,
            clock: clock.clone(),
        })
        .await?;
        if self.resume_sharing && authority.pending_adoption(&principal)?.is_none() {
            peers.resume_saved().await?;
        }
        let directory = self
            .discovery_repository
            .map(|repository| {
                GitHubDirectory::new(&repository, credentials)
                    .map(|directory| DirectorySync::new(Arc::new(directory)))
            })
            .transpose()?;
        Ok(Service {
            authority,
            peers,
            principal,
            engine,
            clock,
            directory,
            adoption: None,
        })
    }
}
