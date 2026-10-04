//! Native port of the TypeScript peer coordinator and its saved-state lifecycle.

mod bridge;
mod loops;
mod shared;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use editchain_sync::Progress;
use idle_history::connection::{Connection, ConnectionStatus, JoinState};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    clock::Clock,
    discovery::Advertisement,
    engine::{Engine, ScopeChoice, SharingScope},
    invitation::{
        HostLease, Invitation, InviteKind, JoinRequest, RequestKind, SavedSharing, encode,
    },
    persistence::Persistence,
    transport::{Credentials, RelayDescriptor, RelayProvider, bounded},
};

use self::{
    loops::HostRequest,
    shared::{Runtime, SAVED_KEY, STOP_KEY, Shared},
};

/// One shared connection's status, after native authentication/inventory checks.
#[derive(Clone, Debug, Serialize)]
pub struct PeerStatus {
    /// Connection-local edge identity for progress observations across reconnects.
    pub connection: Option<String>,
    /// Exact supplying device when known; independent of record authorship.
    pub fingerprint: Option<String>,
    /// Shared Rust connection lifecycle.
    pub state: ConnectionStatus,
    /// Native durable counts and separately reported in-progress checks.
    pub progress: Option<Progress>,
}

/// Credential-free status suitable for native consumers and service responses.
#[derive(Clone, Debug, Serialize)]
pub struct SharingStatus {
    /// Bound collaboration space, if configured.
    pub space: Option<String>,
    /// Exact engine consent, including interrupted changes.
    pub scope: Option<SharingScope>,
    /// The current generation is enabled.
    pub enabled: bool,
    /// The current host attempt is connected.
    pub hosting: bool,
    /// Host retry/authentication lifecycle.
    pub host_state: ConnectionStatus,
    /// Approved outbound and incoming connections.
    pub peers: Vec<PeerStatus>,
    /// Change counter for consumers to refresh newly durable records/content.
    pub durable_changes: u64,
    /// Fixed error context without SDK diagnostics or tokens.
    pub message: Option<String>,
}

/// All platform capabilities are explicitly injected.
#[derive(Debug)]
pub struct PeerOptions {
    /// Native chain and persistent device identity.
    pub engine: Engine,
    /// Opaque relay implementation.
    pub relay: Arc<dyn RelayProvider>,
    /// Management credentials and approved-grant renewal.
    pub credentials: Arc<dyn Credentials>,
    /// Private saved-session persistence.
    pub storage: Arc<dyn Persistence>,
    /// Host authority clock.
    pub clock: Arc<dyn Clock>,
}

#[derive(Debug)]
struct Task {
    cancel: CancellationToken,
    join: JoinHandle<Result<()>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StopIntent {
    host: Option<HostLease>,
}

/// Repository-scoped peer owner. Call `suspend` or `stop` to await full teardown.
/// Drop cancels all owned work; private journals cover interrupted cloud cleanup.
#[derive(Debug)]
pub struct PeerCoordinator {
    shared: Arc<Shared>,
    cancel: CancellationToken,
    host_commands: Option<mpsc::Sender<HostRequest>>,
    host_task: Option<Task>,
    peers: BTreeMap<String, Task>,
}

impl PeerCoordinator {
    /// Recover binding and private saved formats without enabling sharing.
    /// An earlier durable Stop is completed before any session may resume.
    ///
    /// # Errors
    /// Rejects corrupt saved state, mismatched engine bindings and storage failures.
    pub async fn open(options: PeerOptions) -> Result<Self> {
        let identity = options.engine.identity().await?;
        let scope = options.engine.scope().await?;
        let persisted = options.storage.load(SAVED_KEY)?;
        let saved: Option<SavedSharing> = persisted
            .as_ref()
            .map(|bytes| serde_json::from_slice(bytes))
            .transpose()?;
        if let Some(saved) = &saved {
            saved.validate()?;
            if scope
                .as_ref()
                .is_none_or(|scope| scope.space != saved.space)
            {
                return Err(Error::Conflict);
            }
        }
        let cancel = CancellationToken::new();
        let runtime = Runtime {
            session: JoinState::default(),
            cancel: cancel.clone(),
            saved,
            persisted,
            scope,
            host: Connection::default(),
            peers: BTreeMap::new(),
            edges: BTreeMap::new(),
            next_edge: 0,
            durable_changes: 0,
            last_error: None,
        };
        let (status, _receiver) = watch::channel(runtime.status());
        let shared = Arc::new(Shared {
            engine: options.engine,
            relay: options.relay,
            credentials: options.credentials,
            storage: options.storage,
            clock: options.clock,
            identity,
            runtime: Mutex::new(runtime),
            status,
        });
        let mut coordinator = Self {
            shared,
            cancel,
            host_commands: None,
            host_task: None,
            peers: BTreeMap::new(),
        };
        if coordinator.shared.storage.load(STOP_KEY)?.is_some() {
            if let Err(error) = coordinator.stop().await {
                coordinator.shared.access(|state| {
                    state.last_error = Some(error);
                    Ok(())
                })?;
            }
        } else {
            let retained = coordinator
                .shared
                .access(|state| Ok(state.saved.as_ref().and_then(|saved| saved.host.clone())))?;
            if let Err(error) = coordinator
                .shared
                .relay
                .cleanup(retained.as_ref(), &coordinator.cancel)
                .await
            {
                coordinator.shared.access(|state| {
                    state.last_error = Some(error);
                    Ok(())
                })?;
            }
        }
        Ok(coordinator)
    }

    /// Transfer an earlier host's saved session without selecting new consent.
    /// A retry may acknowledge the same import, but never overwrite native state.
    ///
    /// # Errors
    /// Rejects changed spaces, devices, approvals, stopped consent or conflicting state.
    pub async fn import_saved(&mut self, saved: SavedSharing) -> Result<()> {
        saved.validate()?;
        let digest = blake3::hash(&serde_json::to_vec(&saved)?);
        if let Some(imported) = self.shared.storage.load("sharing-import")? {
            return if imported == digest.as_bytes() {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        if self.shared.storage.load(STOP_KEY)?.is_some() {
            return Err(Error::Forbidden);
        }
        let scope = self.shared.engine.scope().await?.ok_or(Error::Forbidden)?;
        if !scope.active || scope.space != saved.space {
            return Err(Error::Forbidden);
        }
        let approved = self.shared.engine.devices(&saved.space).await?;
        for peer in &saved.peers {
            if peer.version != 1
                || peer.space != saved.space
                || peer.guest != self.shared.identity.fingerprint
                || !approved.contains(&peer.host)
            {
                return Err(Error::Forbidden);
            }
            peer.endpoint.validate()?;
            let _expiration = crate::invitation::token_expiration(&peer.connect_token)?;
        }
        self.shared.access(|state| {
            if let Some(previous) = &state.saved {
                return if previous == &saved {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                };
            }
            state.scope = Some(scope);
            self.shared.persist(state, Some(saved))
        })?;
        self.shared
            .storage
            .compare_exchange("sharing-import", None, Some(digest.as_bytes()))
    }

    /// Retain imported cleanup markers before the old host erases its copy.
    ///
    /// # Errors
    /// Rejects invalid markers or failed private journal writes.
    pub fn import_cleanup(&self, markers: &[String]) -> Result<()> {
        self.shared.relay.import_cleanup(markers)
    }

    /// Retry pending cleanup while preserving this coordinator's retained host.
    ///
    /// # Errors
    /// Returns failed management or cleanup, retaining the journal for retry.
    pub async fn cleanup(&self, cancel: &CancellationToken) -> Result<()> {
        let retained = self
            .shared
            .access(|state| Ok(state.saved.as_ref().and_then(|saved| saved.host.clone())))?;
        self.shared.relay.cleanup(retained.as_ref(), cancel).await
    }

    pub(crate) fn credentials(&self) -> Arc<dyn Credentials> {
        self.shared.credentials.clone()
    }

    /// Subscribe to shared Rust status and native durable-change counters.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<SharingStatus> {
        self.shared.status.subscribe()
    }

    /// Current credential-free status; has no effect on sharing consent.
    #[must_use]
    pub fn status(&self) -> SharingStatus {
        self.shared.status.borrow().clone()
    }

    /// Construct a join request for this persistent device identity.
    ///
    /// # Errors
    /// Reports oversized/invalid encoding.
    pub fn join_request(&self) -> Result<String> {
        encode(&JoinRequest {
            version: 1,
            kind: RequestKind::Request,
            device: self.shared.identity.clone(),
        })
    }

    /// Validate a fresh invitation before any approval or transport work.
    ///
    /// # Errors
    /// Rejects expired, incompatible or incorrectly addressed invitations.
    pub fn inspect_invitation(&self, text: &str) -> Result<Invitation> {
        Invitation::parse(text, &self.shared.identity, self.shared.clock.now_ms()?)
    }

    /// Explicitly approve a join request, select consent and host a private relay.
    ///
    /// # Errors
    /// Returns binding, cancellation, persistence or transport failures.
    pub async fn host_history(
        &mut self,
        request: &str,
        choice: ScopeChoice,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let guest = JoinRequest::parse(request)?.device;
        if guest == self.shared.identity {
            return Err(Error::Forbidden);
        }
        let space = self
            .status()
            .space
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.configure(&space, choice, cancel).await?;
        self.shared.engine.approve(&space, &guest).await?;
        active(cancel)?;
        self.enable(&space)?;
        self.launch_saved()?;
        self.launch_host()?;
        let descriptor = match self.descriptor(cancel).await {
            Ok(descriptor) => descriptor,
            Err(error) => {
                if error == Error::Cancelled {
                    self.suspend().await?;
                }
                return Err(error);
            }
        };
        if cancel.is_cancelled() {
            self.suspend().await?;
            return Err(Error::Cancelled);
        }
        encode(&Invitation {
            version: 1,
            kind: InviteKind::Invite,
            space,
            host: self.shared.identity.clone(),
            guest: guest.fingerprint,
            endpoint: descriptor.endpoint,
            connect_token: descriptor.connect_token,
            expires_at: descriptor.expires_at,
        })
    }

    /// Approve an invitation and connect with the explicit outgoing scope choice.
    ///
    /// # Errors
    /// Rejects cross-space/device invitations, inactive consent or failed persistence.
    pub async fn join_history(
        &mut self,
        text: &str,
        choice: ScopeChoice,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let invitation = self.inspect_invitation(text)?;
        self.configure(&invitation.space, choice, cancel).await?;
        self.shared
            .engine
            .approve(&invitation.space, &invitation.host)
            .await?;
        active(cancel)?;
        self.enable(&invitation.space)?;
        let fingerprint = invitation.host.fingerprint.clone();
        self.shared.access(|state| {
            let mut saved = state.saved.clone().ok_or(Error::Invalid)?;
            saved
                .peers
                .retain(|peer| peer.host.fingerprint != fingerprint);
            if saved.peers.len() >= 32 {
                return Err(Error::Busy);
            }
            saved.peers.push(invitation);
            self.shared.persist(state, Some(saved))
        })?;
        if let Some(previous) = self.peers.remove(&fingerprint) {
            previous.cancel.cancel();
            previous.finish().await?;
        }
        self.launch_saved()
    }

    /// Resume only existing engine approval and the exact stored consent boundary.
    /// Expired peers remain expired unless the injected adapter renews their grant.
    ///
    /// # Errors
    /// Rejects interrupted consent changes, mismatched state and incomplete Stops.
    pub async fn resume(&mut self) -> Result<()> {
        if self.status().enabled {
            return Ok(());
        }
        let saved = self
            .shared
            .access(|state| state.saved.clone().ok_or(Error::Invalid))?;
        let scope = self.shared.engine.scope().await?.ok_or(Error::Invalid)?;
        if !scope.active || scope.space != saved.space {
            return Err(Error::Forbidden);
        }
        let approved = self.shared.engine.devices(&saved.space).await?;
        let mut resumed = saved.clone();
        resumed.peers.retain(|invitation| {
            invitation.space == saved.space
                && invitation.version == 1
                && invitation.guest == self.shared.identity.fingerprint
                && approved.contains(&invitation.host)
                && invitation.endpoint.validate().is_ok()
                && crate::invitation::token_expiration(&invitation.connect_token).is_ok()
        });
        self.shared.access(|state| {
            state.scope = Some(scope);
            self.shared.persist(state, Some(resumed))
        })?;
        self.enable(&saved.space)?;
        self.launch_saved()
    }

    pub(crate) async fn resume_saved(&mut self) -> Result<()> {
        if self.shared.access(|state| Ok(state.saved.is_some()))? {
            self.resume().await?;
        }
        Ok(())
    }

    /// Drain old connections and restart from durable inventories and saved grants.
    ///
    /// # Errors
    /// Rejects stopped sharing or failed resource teardown/recovery.
    pub async fn reconnect(&mut self) -> Result<()> {
        if !self.status().enabled {
            return Err(Error::Forbidden);
        }
        self.suspend().await?;
        self.resume().await
    }

    /// Explicitly replace the outgoing boundary after draining every old worker.
    ///
    /// # Errors
    /// Returns native consent failures; interrupted changes stay disabled.
    pub async fn change_scope(
        &mut self,
        choice: ScopeChoice,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let space = self.status().space.ok_or(Error::Invalid)?;
        let was_enabled = self.status().enabled;
        self.configure(&space, choice, cancel).await?;
        if was_enabled {
            self.resume().await?;
        }
        Ok(())
    }

    /// Remove device approval and drain its existing inbound/outbound connections.
    ///
    /// # Errors
    /// Reports native revocation or private saved-state failures.
    pub async fn revoke(&mut self, fingerprint: &str) -> Result<()> {
        let space = self.status().space.ok_or(Error::Invalid)?;
        self.shared.engine.revoke(&space, fingerprint).await?;
        self.shared.access(|state| {
            for edge in state.edges.values() {
                if edge.fingerprint.as_deref() == Some(fingerprint) {
                    edge.cancel.cancel();
                }
            }
            let _peer = state.peers.remove(fingerprint);
            if let Some(mut saved) = state.saved.clone() {
                saved
                    .peers
                    .retain(|peer| peer.host.fingerprint != fingerprint);
                self.shared.persist(state, Some(saved))?;
            }
            Ok(())
        })?;
        if let Some(task) = self.peers.remove(fingerprint) {
            task.cancel.cancel();
            task.finish().await?;
        }
        let mut status = self.subscribe();
        bounded(&CancellationToken::new(), Duration::from_secs(30), async {
            while self.shared.access(|state| {
                Ok(state
                    .edges
                    .values()
                    .any(|edge| edge.fingerprint.as_deref() == Some(fingerprint)))
            })? {
                status.changed().await.map_err(|_error| Error::Transport)?;
            }
            Ok(())
        })
        .await
    }

    /// Pause all transports and native workers while preserving consent/saved state.
    ///
    /// # Errors
    /// Reports task teardown failures; no stale callback can re-enable the session.
    pub async fn suspend(&mut self) -> Result<()> {
        self.cancel.cancel();
        self.shared.access(|state| {
            state.session.retire();
            state.host.stop();
            for peer in state.peers.values_mut() {
                peer.stop();
            }
            for edge in state.edges.values() {
                edge.cancel.cancel();
            }
            Ok(())
        })?;
        self.host_commands = None;
        let mut failed = false;
        if let Some(task) = self.host_task.take() {
            failed |= task.finish().await.is_err();
        }
        for (_fingerprint, task) in std::mem::take(&mut self.peers) {
            failed |= task.finish().await.is_err();
        }
        self.cancel = CancellationToken::new();
        self.shared.access(|state| {
            state.cancel = self.cancel.clone();
            Ok(())
        })?;
        let retained = self
            .shared
            .access(|state| Ok(state.saved.as_ref().and_then(|saved| saved.host.clone())))?;
        let cleanup = self
            .shared
            .relay
            .cleanup(retained.as_ref(), &self.cancel)
            .await;
        if failed {
            Err(Error::Transport)
        } else {
            cleanup
        }
    }

    /// Durably stop sharing, then remove exact owned cloud resources.
    /// Failed/uncertain cleanup stays journaled and is retried after restart.
    ///
    /// # Errors
    /// Reports incomplete persistence or resource removal, without losing its owner key.
    pub async fn stop(&mut self) -> Result<()> {
        self.cancel.cancel();
        let recorded = self.record_stop();
        let suspended = self.suspend().await;
        let (intent, bytes) = recorded?;
        self.shared.access(|state| {
            state.peers.clear();
            self.shared.persist(state, None)
        })?;
        let cancel = CancellationToken::new();
        if let Some(lease) = &intent.host {
            self.shared.relay.remove(lease, &cancel).await?;
        }
        self.shared.relay.cleanup(None, &cancel).await?;
        self.shared
            .storage
            .compare_exchange(STOP_KEY, Some(&bytes), None)?;
        suspended
    }

    fn record_stop(&self) -> Result<(StopIntent, Vec<u8>)> {
        let previous = self.shared.storage.load(STOP_KEY)?;
        let intent: StopIntent = if let Some(bytes) = &previous {
            serde_json::from_slice(bytes)?
        } else {
            self.shared.access(|state| {
                Ok(StopIntent {
                    host: state.saved.as_ref().and_then(|saved| saved.host.clone()),
                })
            })?
        };
        let bytes = serde_json::to_vec(&intent)?;
        self.shared
            .storage
            .compare_exchange(STOP_KEY, previous.as_deref(), Some(&bytes))?;
        Ok((intent, bytes))
    }

    /// Produce credential-free discovery data for the current hosting resource.
    ///
    /// # Errors
    /// Returns current descriptor/management failures; never exposes connect grants.
    pub async fn describe(&self, cancel: &CancellationToken) -> Result<Option<Advertisement>> {
        if !self.status().enabled || !self.status().hosting {
            return Ok(None);
        }
        let descriptor = self.descriptor(cancel).await?;
        let lease = self.shared.access(|state| {
            state
                .saved
                .as_ref()
                .and_then(|saved| saved.host.clone())
                .ok_or(Error::Invalid)
        })?;
        let advertisement = Advertisement {
            version: 1,
            protocol: crate::invitation::DISCOVERY_PROTOCOL,
            encoding: 1,
            space: self.status().space.ok_or(Error::Invalid)?,
            device: self.shared.identity.clone(),
            instance: lease.marker,
            endpoint: descriptor.endpoint,
            expires_at: self.shared.clock.now_ms()?.saturating_add(600_000),
        };
        advertisement.validate(self.shared.clock.now_ms()?)?;
        Ok(Some(advertisement))
    }

    /// Discovery refreshes only existing certificate/tunnel approvals.
    /// It cannot enroll a new device or replace an expired connect grant.
    ///
    /// # Errors
    /// Reports local approval/persistence failures; malformed candidates are ignored.
    pub async fn discover(&mut self, candidates: &[Advertisement]) -> Result<()> {
        if !self.status().enabled {
            return Ok(());
        }
        let space = self.status().space.ok_or(Error::Invalid)?;
        let approved = self.shared.engine.devices(&space).await?;
        let now = self.shared.clock.now_ms()?;
        let changed = self.shared.access(|state| {
            let mut saved = state.saved.clone().ok_or(Error::Invalid)?;
            let mut changed = false;
            for candidate in candidates.iter().take(32) {
                if candidate.validate(now).is_err()
                    || candidate.space != space
                    || !approved.contains(&candidate.device)
                {
                    continue;
                }
                if let Some(peer) = saved.peers.iter_mut().find(|peer| {
                    peer.host == candidate.device
                        && peer.endpoint.tunnel_id == candidate.endpoint.tunnel_id
                        && peer.endpoint.cluster_id == candidate.endpoint.cluster_id
                }) && peer.endpoint != candidate.endpoint
                {
                    peer.endpoint = candidate.endpoint.clone();
                    changed = true;
                }
            }
            if changed {
                self.shared.persist(state, Some(saved))?;
            }
            Ok(changed)
        })?;
        if changed {
            self.reconnect().await?;
        }
        Ok(())
    }

    async fn configure(
        &mut self,
        space: &str,
        choice: ScopeChoice,
        cancel: &CancellationToken,
    ) -> Result<()> {
        active(cancel)?;
        if self
            .status()
            .space
            .as_ref()
            .is_some_and(|bound| bound != space)
        {
            return Err(Error::Conflict);
        }
        if choice != ScopeChoice::Keep {
            self.suspend().await?;
        }
        let result = self.shared.engine.configure(space, choice).await;
        let scope = self.shared.engine.scope().await?;
        self.shared.access(|state| {
            state.scope = scope;
            Ok(())
        })?;
        let _scope = result?;
        active(cancel)
    }

    fn enable(&mut self, space: &str) -> Result<()> {
        if self.shared.storage.load(STOP_KEY)?.is_some() {
            return Err(Error::Conflict);
        }
        self.shared.access(|state| {
            if state
                .scope
                .as_ref()
                .is_none_or(|scope| !scope.active || scope.space != space)
            {
                return Err(Error::Forbidden);
            }
            if !state.session.enable(state.session.generation()) {
                return Err(Error::Cancelled);
            }
            let saved = state.saved.clone().unwrap_or_else(|| SavedSharing {
                version: 1,
                space: space.into(),
                host: None,
                peers: Vec::new(),
            });
            self.shared.persist(state, Some(saved))
        })
    }

    fn launch_saved(&mut self) -> Result<()> {
        let (saved, generation) = self.shared.access(|state| {
            Ok((
                state.saved.clone().ok_or(Error::Invalid)?,
                state.session.generation(),
            ))
        })?;
        if saved.host.is_some() {
            self.launch_host()?;
        }
        for invitation in saved.peers {
            let fingerprint = invitation.host.fingerprint;
            if self
                .peers
                .get(&fingerprint)
                .is_some_and(|task| !task.join.is_finished())
            {
                continue;
            }
            self.shared.access(|state| {
                let _previous = state
                    .peers
                    .insert(fingerprint.clone(), Connection::default());
                Ok(())
            })?;
            let cancel = self.cancel.child_token();
            let join = tokio::spawn(loops::peer_loop(
                self.shared.clone(),
                generation,
                fingerprint.clone(),
                cancel.clone(),
            ));
            let _previous = self.peers.insert(fingerprint, Task { cancel, join });
        }
        Ok(())
    }

    fn launch_host(&mut self) -> Result<()> {
        if self
            .host_task
            .as_ref()
            .is_some_and(|task| !task.join.is_finished())
        {
            return Ok(());
        }
        let generation = self.shared.access(|state| Ok(state.session.generation()))?;
        let (commands, receiver) = mpsc::channel(8);
        let cancel = self.cancel.child_token();
        let join = tokio::spawn(loops::host_loop(
            self.shared.clone(),
            generation,
            cancel.clone(),
            receiver,
        ));
        self.host_commands = Some(commands);
        self.host_task = Some(Task { cancel, join });
        Ok(())
    }

    async fn descriptor(&self, cancel: &CancellationToken) -> Result<RelayDescriptor> {
        let commands = self.host_commands.as_ref().ok_or(Error::Transport)?;
        let (reply, result) = oneshot::channel();
        bounded(cancel, Duration::from_mins(1), async {
            commands
                .send(HostRequest::Descriptor(reply))
                .await
                .map_err(|_error| Error::Transport)?;
            result.await.map_err(|_error| Error::Transport)?
        })
        .await
    }
}

fn active(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

impl Task {
    async fn finish(self) -> Result<()> {
        match self.join.await.map_err(|_error| Error::Transport)? {
            Err(Error::Cancelled) | Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for PeerCoordinator {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
