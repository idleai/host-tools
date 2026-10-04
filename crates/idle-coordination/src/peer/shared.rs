use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use editchain_sync::{Progress, PublicDevice};
use idle_history::connection::{
    Connection, ConnectionStatus, JoinState, PeerCheck, PeerProgress, peer_status,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    clock::Clock,
    engine::{Engine, SharingScope},
    invitation::{HostLease, Invitation, SavedSharing},
    persistence::Persistence,
    transport::{Credentials, RelayProvider},
};

use super::{PeerStatus, SharingStatus};

pub(super) const SAVED_KEY: &str = "saved-sharing";
pub(super) const STOP_KEY: &str = "sharing-stop";

#[derive(Debug)]
pub(super) struct Edge {
    pub lifecycle: Connection,
    pub outbound: bool,
    pub fingerprint: Option<String>,
    pub progress: Option<Progress>,
    pub cancel: CancellationToken,
}

#[derive(Debug)]
pub(super) struct Runtime {
    pub session: JoinState,
    pub cancel: CancellationToken,
    pub saved: Option<SavedSharing>,
    pub persisted: Option<Vec<u8>>,
    pub scope: Option<SharingScope>,
    pub host: Connection,
    pub peers: BTreeMap<String, Connection>,
    pub edges: BTreeMap<u64, Edge>,
    pub next_edge: u64,
    pub durable_changes: u64,
    pub last_error: Option<Error>,
}

impl Runtime {
    pub(super) fn status(&self) -> SharingStatus {
        let mut peers: Vec<_> = self
            .edges
            .iter()
            .map(|(id, edge)| PeerStatus {
                connection: Some(id.to_string()),
                fingerprint: edge.fingerprint.clone(),
                state: edge.lifecycle.status(),
                progress: edge.progress.clone(),
            })
            .collect();
        for (fingerprint, lifecycle) in &self.peers {
            if !peers
                .iter()
                .any(|peer| peer.fingerprint.as_ref() == Some(fingerprint))
            {
                peers.push(PeerStatus {
                    connection: None,
                    fingerprint: Some(fingerprint.clone()),
                    state: lifecycle.status(),
                    progress: None,
                });
            }
        }
        SharingStatus {
            space: self.scope.as_ref().map(|scope| scope.space.clone()),
            scope: self.scope.clone(),
            enabled: self.session.enabled(),
            hosting: self.host.status() == ConnectionStatus::Live,
            host_state: self.host.status(),
            peers,
            durable_changes: self.durable_changes,
            message: self.last_error.map(|error| error.to_string()),
        }
    }

    pub(super) fn current(&self, generation: u32) -> bool {
        self.session.enabled() && self.session.is_current(generation)
    }
}

#[derive(Debug)]
pub(super) struct Shared {
    pub engine: Engine,
    pub relay: Arc<dyn RelayProvider>,
    pub credentials: Arc<dyn Credentials>,
    pub storage: Arc<dyn Persistence>,
    pub clock: Arc<dyn Clock>,
    pub identity: PublicDevice,
    pub runtime: Mutex<Runtime>,
    pub status: watch::Sender<SharingStatus>,
}

impl Shared {
    pub(super) fn access<T>(&self, action: impl FnOnce(&mut Runtime) -> Result<T>) -> Result<T> {
        let mut state = self.runtime.lock().map_err(|_error| Error::Storage)?;
        let result = action(&mut state);
        let _previous = self.status.send_replace(state.status());
        result
    }

    pub(super) fn current(&self, generation: u32) -> bool {
        self.runtime
            .lock()
            .is_ok_and(|state| state.current(generation))
    }

    pub(super) fn persist(&self, state: &mut Runtime, saved: Option<SavedSharing>) -> Result<()> {
        let bytes = saved.as_ref().map(serde_json::to_vec).transpose()?;
        if let Err(error) =
            self.storage
                .compare_exchange(SAVED_KEY, state.persisted.as_deref(), bytes.as_deref())
        {
            state.last_error = Some(error);
            state.session.retire();
            state.cancel.cancel();
            state.host.stop();
            for peer in state.peers.values_mut() {
                peer.stop();
            }
            for edge in state.edges.values() {
                edge.cancel.cancel();
            }
            return Err(error);
        }
        state.saved = saved;
        state.persisted = bytes;
        Ok(())
    }

    pub(super) fn save_host(&self, generation: u32, lease: HostLease) -> Result<()> {
        self.access(|state| {
            if !state.current(generation) {
                return Err(Error::Cancelled);
            }
            let mut saved = state.saved.clone().ok_or(Error::Invalid)?;
            saved.host = Some(lease);
            self.persist(state, Some(saved))
        })
    }

    pub(super) fn invitation(&self, generation: u32, fingerprint: &str) -> Result<Invitation> {
        self.access(|state| {
            if !state.current(generation) {
                return Err(Error::Cancelled);
            }
            state
                .saved
                .as_ref()
                .and_then(|saved| {
                    saved
                        .peers
                        .iter()
                        .find(|peer| peer.host.fingerprint == fingerprint)
                })
                .cloned()
                .ok_or(Error::Forbidden)
        })
    }

    pub(super) fn replace_invitation(
        &self,
        generation: u32,
        previous: &Invitation,
        replacement: Invitation,
    ) -> Result<()> {
        self.access(|state| {
            if !state.current(generation) {
                return Err(Error::Cancelled);
            }
            let mut saved = state.saved.clone().ok_or(Error::Invalid)?;
            let invitation = saved
                .peers
                .iter_mut()
                .find(|peer| peer.host.fingerprint == previous.host.fingerprint)
                .ok_or(Error::Forbidden)?;
            if invitation != previous {
                return Err(Error::Conflict);
            }
            *invitation = replacement;
            self.persist(state, Some(saved))
        })
    }

    pub(super) fn add_edge(
        &self,
        generation: u32,
        fingerprint: Option<String>,
        outbound: bool,
        cancel: CancellationToken,
    ) -> Result<u64> {
        self.access(|state| {
            if !state.current(generation) {
                return Err(Error::Cancelled);
            }
            if state.edges.len() >= 8 {
                return Err(Error::Busy);
            }
            let id = state.next_edge.checked_add(1).ok_or(Error::Invalid)?;
            state.next_edge = id;
            let mut lifecycle = Connection::default();
            let attempt = lifecycle.begin().ok_or(Error::Invalid)?;
            let _updated = lifecycle.update(attempt, ConnectionStatus::Authenticating);
            let _previous = state.edges.insert(
                id,
                Edge {
                    lifecycle,
                    outbound,
                    fingerprint,
                    progress: None,
                    cancel,
                },
            );
            Ok(id)
        })
    }

    pub(super) fn progress(
        &self,
        generation: u32,
        id: u64,
        device: Option<&PublicDevice>,
        progress: Progress,
    ) -> Result<()> {
        self.access(|state| {
            if !state.current(generation) {
                return Err(Error::Cancelled);
            }
            let edge = state.edges.get_mut(&id).ok_or(Error::Cancelled)?;
            if let Some(device) = device {
                if edge
                    .fingerprint
                    .as_ref()
                    .is_some_and(|expected| *expected != device.fingerprint)
                {
                    return Err(Error::Forbidden);
                }
                edge.fingerprint = Some(device.fingerprint.clone());
            }
            let lifecycle = peer_status(&PeerProgress {
                accepted: progress.accepted,
                synchronizing: progress.synchronizing,
                rounds: progress.rounds,
                unavailable: progress.unavailable,
                outgoing: Some(PeerCheck {
                    pass: progress.outgoing.pass,
                    complete: progress.outgoing.complete,
                    unavailable: progress.outgoing.unavailable,
                }),
            });
            let _updated = edge
                .lifecycle
                .update(edge.lifecycle.generation(), lifecycle);
            let previous = edge
                .progress
                .as_ref()
                .map(|p| (p.records, p.blobs))
                .unwrap_or_default();
            if previous != (progress.records, progress.blobs) {
                state.durable_changes = state.durable_changes.saturating_add(1);
            }
            edge.progress = Some(progress);
            let outbound = edge.outbound;
            let own_cancel = edge.cancel.clone();
            if let Some(device) = device {
                if let Some(peer) = state.peers.get_mut(&device.fingerprint) {
                    let _updated = peer.update(peer.generation(), lifecycle);
                }
                let preferred = self.identity.fingerprint < device.fingerprint;
                for (other_id, other) in &state.edges {
                    if *other_id != id && other.fingerprint.as_ref() == Some(&device.fingerprint) {
                        if other.outbound == outbound || outbound != preferred {
                            own_cancel.cancel();
                        } else {
                            other.cancel.cancel();
                        }
                    }
                }
            }
            state.last_error = None;
            Ok(())
        })
    }

    pub(super) fn remove_edge(&self, id: u64) {
        let _removed = self.access(|state| {
            let _edge = state.edges.remove(&id);
            Ok(())
        });
    }

    pub(super) fn failure(&self, generation: u32, error: Error) {
        let _updated = self.access(|state| {
            if state.current(generation) {
                state.last_error = Some(error);
            }
            Ok(())
        });
    }
}
