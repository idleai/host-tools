//! Native service commands over an authenticated, host-owned local connection.

mod credentials;
mod framing;
pub mod native;

use std::sync::Arc;

use idle_protocol::v1::{
    api::Request,
    control::ControlFence,
    events::RecoveryCursor,
    identity::RequestKey,
    standalone::{AccessCheck, Mutation, Presence},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    authority::{
        Authority, Principal,
        adoption::{HistoryConsent, ManagedAdoption},
    },
    discovery::{DirectorySync, GitHubDirectory},
    engine::{Engine, ScopeChoice},
    invitation::{JoinRequest, SavedSharing, Secret},
    peer::PeerCoordinator,
};

pub use framing::{
    Client, Message, ServiceRequest, ServiceResponse, read_frame, serve, serve_configuration,
    write_frame,
};

pub use credentials::{CredentialPurpose, CredentialReply, CredentialRequest};

/// Local service framing version, independent of repository and peer protocols.
pub const SERVICE_VERSION: u16 = 1;

/// Explicit operations available to a native consumer. Authentication is supplied
/// when the service is constructed, never accepted from this enum.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Command {
    /// Report all compatibility boundaries before opening history sharing.
    Versions,
    /// Read the currently authorized replacement snapshot.
    Snapshot,
    /// Read authored definitions, stable workspace identity and content revision.
    WorkspaceConfiguration,
    /// Authorize and commit an idempotent repository mutation.
    Mutate(Box<Request<Mutation>>),
    /// Resolve the original outcome after an uncertain response.
    RequestStatus(RequestKey),
    /// Recover bounded ordered invalidations from an earlier snapshot.
    CatchUp {
        /// Exclusive audience-bound cursor.
        after: RecoveryCursor,
        /// Maximum notifications, between one and 256.
        limit: usize,
    },
    /// Revalidate a resource grant for the connected contributor.
    CheckAccess(AccessCheck),
    /// Revalidate an exact controller fence for the authenticated runtime.
    ValidateControl(ControlFence),
    /// Read fresh peer activity.
    Presence,
    /// Publish peer activity for this connected contributor.
    PublishPresence(Presence),
    /// Remove this contributor's peer activity by connection ID.
    RemovePresence(String),
    /// Read peer connection state without credentials.
    SharingStatus,
    /// Construct a join request for the persistent local device.
    JoinRequest,
    /// Validate a public request before the host presents approval.
    InspectRequest(Secret),
    /// Validate a private invitation before the host presents approval.
    InspectInvitation(Secret),
    /// Read the existing outgoing consent boundary.
    SharingScope,
    /// List the engine's existing approved devices.
    Devices,
    /// Import a previous host's private saved session; never grants consent.
    ImportSharing(Box<SavedSharing>),
    /// Durably transfer pending relay cleanup markers.
    ImportCleanup(Vec<String>),
    /// Retry pending relay cleanup without removing retained hosts.
    Cleanup,
    /// Configure explicitly approved discovery for this connection.
    ConfigureDirectory(Option<String>),
    /// Explicitly approve a guest and create/resume a private relay.
    Host {
        /// Invitation-channel join request from the exact guest.
        request: Secret,
        /// Explicit outgoing consent choice.
        scope: ScopeChoice,
    },
    /// Explicitly accept an invitation addressed to this device.
    Join {
        /// Private invitation, redacted from debug output.
        invitation: Secret,
        /// Explicit outgoing consent choice.
        scope: ScopeChoice,
    },
    /// Resume only retained approvals and the existing active history boundary.
    Resume,
    /// Drain and restart connections from durable engine inventories.
    Reconnect,
    /// Drain old workers before explicitly replacing the outgoing boundary.
    Scope(ScopeChoice),
    /// Revoke an exact device fingerprint and drain its connections.
    Revoke(String),
    /// Drain connections and retain private saved state.
    Suspend,
    /// Durably stop sharing and delete owned relay resources.
    Stop,
    /// Refresh the explicitly configured public discovery directory.
    Discover,
    /// Freeze and export to a host-configured managed destination.
    PrepareAdoption(String),
    /// Retry/complete adoption through the injected authenticated adapter.
    FinishAdoption,
    /// Read the durable frozen package after restart.
    PendingAdoption,
    /// Freeze standalone metadata for the explicitly approved daemon owner.
    PrepareRuntimeTransfer {
        /// Approved daemon and checkout.
        target: crate::authority::runtime_transfer::RuntimeTarget,
        /// Checkout definitions confirmed at both ends before freezing writes.
        configuration_revision: String,
    },
    /// Read the retained daemon route, including after an interrupted transfer.
    RuntimeTransferStatus,
    /// Read a bounded private chunk from the frozen transfer.
    RuntimeTransferChunk(usize),
    /// Retain the daemon's acknowledgement without enabling local writes.
    CompleteRuntimeTransfer(crate::authority::runtime_transfer::RuntimeReceipt),
}

/// One local service bound to a host-authenticated principal. Repository mutation
/// envelopes still undergo exact contributor checks. The process owner authorizes
/// local peer lifecycle commands by creating this service for its principal.
#[derive(Debug)]
pub struct Service {
    /// Repository authority; hosts may expose it through their own authenticated API.
    pub authority: Authority,
    /// Sharing owner, with injected transport, persistence and credential adapters.
    pub peers: PeerCoordinator,
    /// Connection identity resolved by the native host, outside the request stream.
    pub principal: Principal,
    /// Same persistent chain/device paths used by the peer coordinator.
    pub engine: Engine,
    /// Authority clock shared with peer grants and directory freshness.
    pub clock: Arc<dyn crate::clock::Clock>,
    /// Optional explicitly configured discovery synchronization.
    pub directory: Option<DirectorySync>,
    /// Optional managed integration adapter; absent in the standalone executable.
    pub adoption: Option<Arc<dyn ManagedAdoption>>,
}

impl Service {
    /// Dispatch one command with a cooperative lifetime. Mutations remain
    /// recoverable through their original request keys if the response is lost.
    ///
    /// # Errors
    /// Returns typed authorization, cancellation, compatibility and adapter failures.
    pub async fn call(&mut self, command: Command, cancel: &CancellationToken) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if matches!(
            command,
            Command::Snapshot | Command::WorkspaceConfiguration | Command::CatchUp { .. }
        ) {
            self.authority.refresh_repository()?;
        }
        if matches!(
            command,
            Command::Host { .. }
                | Command::Join { .. }
                | Command::Resume
                | Command::Reconnect
                | Command::Scope(_)
                | Command::Revoke(_)
        ) && self.authority.pending_adoption(&self.principal)?.is_some()
        {
            return Err(Error::Forbidden);
        }
        match command {
            Command::Versions => value(&serde_json::json!({
                "service": SERVICE_VERSION, "repository_api": 1, "invitation": 1, "saved_sharing": 1,
                "discovery": crate::invitation::DISCOVERY_PROTOCOL, "engine_peer": editchain_sync::PEER_VERSION,
                "tunnels_revision": crate::dev_tunnels::SDK_REVISION,
                "workspace_configuration": 1,
                "runtime_transfer": 1,
            })),
            Command::Snapshot => value(&self.authority.snapshot(&self.principal)?),
            Command::WorkspaceConfiguration => value(self.authority.workspace_configuration()?),
            Command::Mutate(request) => value(&self.authority.execute(&self.principal, *request)?),
            Command::RequestStatus(key) => {
                value(&self.authority.request_status(&self.principal, &key)?)
            }
            Command::CatchUp { after, limit } => {
                value(&self.authority.catch_up(&self.principal, &after, limit)?)
            }
            Command::CheckAccess(check) => {
                value(&self.authority.check_access(&self.principal, &check)?)
            }
            Command::ValidateControl(fence) => {
                value(&self.authority.validate_control(&self.principal, &fence)?)
            }
            Command::Presence => value(&self.authority.presence(&self.principal)?),
            Command::PublishPresence(entry) => {
                self.authority.publish_presence(&self.principal, entry)?;
                value(&())
            }
            Command::RemovePresence(connection) => {
                self.authority
                    .remove_presence(&self.principal, &connection)?;
                value(&())
            }
            Command::SharingStatus => {
                let mut status = value(&self.peers.status())?;
                if let Some(directory) = &self.directory {
                    let _previous = status
                        .as_object_mut()
                        .ok_or(Error::Invalid)?
                        .insert("discovery".into(), value(directory.status())?);
                }
                Ok(status)
            }
            Command::JoinRequest => value(&self.peers.join_request()?),
            Command::InspectRequest(request) => value(&JoinRequest::parse(&request.0)?),
            Command::InspectInvitation(invitation) => {
                value(&self.peers.inspect_invitation(&invitation.0)?)
            }
            Command::SharingScope => value(&self.engine.scope().await?),
            Command::Devices => {
                let scope = self.engine.scope().await?.ok_or(Error::Invalid)?;
                value(&self.engine.devices(&scope.space).await?)
            }
            Command::ImportSharing(saved) => {
                self.peers.import_saved(*saved).await?;
                value(&())
            }
            Command::ImportCleanup(markers) => {
                self.peers.import_cleanup(&markers)?;
                value(&())
            }
            Command::Cleanup => {
                self.peers.cleanup(cancel).await?;
                value(&())
            }
            Command::ConfigureDirectory(repository) => {
                let next = repository
                    .map(|name| {
                        GitHubDirectory::new(&name, self.peers.credentials())
                            .map(|directory| DirectorySync::new(Arc::new(directory)))
                    })
                    .transpose()?;
                self.remove_directory(cancel).await?;
                self.directory = next;
                self.refresh_directory(cancel).await?;
                value(&())
            }
            Command::Host { request, scope } => {
                value(&self.peers.host_history(&request.0, scope, cancel).await?)
            }
            Command::Join { invitation, scope } => {
                self.peers
                    .join_history(&invitation.0, scope, cancel)
                    .await?;
                value(&())
            }
            Command::Resume => {
                self.peers.resume().await?;
                value(&())
            }
            Command::Reconnect => {
                self.peers.reconnect().await?;
                value(&())
            }
            Command::Scope(scope) => {
                self.peers.change_scope(scope, cancel).await?;
                value(&())
            }
            Command::Revoke(fingerprint) => {
                self.peers.revoke(&fingerprint).await?;
                value(&())
            }
            Command::Suspend => {
                self.suspend().await?;
                value(&())
            }
            Command::Stop => {
                self.peers.stop().await?;
                self.remove_directory(cancel).await?;
                value(&())
            }
            Command::Discover => {
                self.refresh_directory(cancel).await?;
                value(&())
            }
            Command::PrepareAdoption(target) => {
                self.suspend().await?;
                let history = HistoryConsent::from_engine(&self.engine).await?;
                value(
                    &self
                        .authority
                        .prepare_adoption(&self.principal, &target, history)?,
                )
            }
            Command::FinishAdoption => {
                let adapter = self.adoption.as_ref().ok_or(Error::Forbidden)?;
                value(
                    &self
                        .authority
                        .finish_adoption(&self.principal, adapter.as_ref(), cancel)
                        .await?,
                )
            }
            Command::PendingAdoption => value(&self.authority.pending_adoption(&self.principal)?),
            Command::PrepareRuntimeTransfer {
                target,
                configuration_revision,
            } => value(&self.authority.prepare_runtime_transfer(
                &self.principal,
                target,
                &configuration_revision,
            )?),
            Command::RuntimeTransferStatus => {
                value(&self.authority.runtime_transfer_status(&self.principal)?)
            }
            Command::RuntimeTransferChunk(offset) => value(
                &self
                    .authority
                    .runtime_transfer_chunk(&self.principal, offset)?,
            ),
            Command::CompleteRuntimeTransfer(receipt) => value(
                &self
                    .authority
                    .complete_runtime_transfer(&self.principal, receipt)?,
            ),
        }
    }

    /// Await transport/worker teardown and retire the public directory entry.
    ///
    /// # Errors
    /// Reports pending transport or discovery cleanup; saved consent remains intact.
    pub async fn suspend(&mut self) -> Result<()> {
        let peers = self.peers.suspend().await;
        let directory = self.remove_directory(&CancellationToken::new()).await;
        peers.and(directory)
    }

    async fn remove_directory(&mut self, cancel: &CancellationToken) -> Result<()> {
        if let Some(directory) = &mut self.directory {
            directory.stop(cancel).await?;
        }
        Ok(())
    }

    async fn refresh_directory(&mut self, cancel: &CancellationToken) -> Result<()> {
        if let Some(directory) = &mut self.directory {
            directory
                .refresh(&mut self.peers, self.clock.now_ms()?, cancel)
                .await?;
        }
        Ok(())
    }
}

fn value<T: Serialize>(value: &T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
