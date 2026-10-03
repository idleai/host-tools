//! Native engine calls and peer-v5 work; no alternate replication codec.

use std::path::PathBuf;

use editchain_sync::{DeviceIdentity, Membership, Progress, PublicDevice, Replica, SecurePeer};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::{Error, Result};

/// Explicit outgoing consent choice, retaining the existing meaning of `keep`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeChoice {
    /// Retain the existing active boundary exactly.
    Keep,
    /// Explicitly share all retained records.
    All,
    /// Select a new engine arrival boundary for future outgoing records.
    FromNow,
}

/// Serializable copy of the engine's existing scope report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharingScope {
    /// Bound collaboration space.
    pub space: String,
    /// `all`, `from_now` or the preserved `legacy_from_now` boundary.
    pub mode: String,
    /// Display-only selection time; the engine filters by its durable boundary.
    pub cutoff_ms: Option<u64>,
    /// Consent generation; earlier workers must reconnect.
    pub revision: u64,
    /// False after an interrupted change; explicit reselection is required.
    pub active: bool,
    /// Retained legacy exclusions.
    pub legacy_excluded_records: usize,
}

impl From<editchain_sync::SharingScope> for SharingScope {
    fn from(value: editchain_sync::SharingScope) -> Self {
        Self {
            space: value.space,
            mode: value.mode.into(),
            cutoff_ms: value.cutoff_ms,
            revision: value.revision,
            active: value.active,
            legacy_excluded_records: value.legacy_excluded_records,
        }
    }
}

/// Paths supplied by the file-owning host; stable across connection/restart cycles.
#[derive(Clone, Debug)]
pub struct Engine {
    /// Existing local chain directory, never inferred from discovery metadata.
    pub chain: PathBuf,
    /// Private, persistent device key directory.
    pub device_directory: PathBuf,
}

impl Engine {
    /// Load the existing native identity or create it once in private storage.
    ///
    /// # Errors
    /// Reports corrupt credentials and native storage failures without replacing identity.
    pub async fn identity(&self) -> Result<PublicDevice> {
        let path = self.device_directory.clone();
        blocking(move || Ok(DeviceIdentity::load_or_create(&path)?.public())).await
    }

    /// Read the authoritative engine consent, including interrupted changes.
    ///
    /// # Errors
    /// Returns engine binding/storage failures.
    pub async fn scope(&self) -> Result<Option<SharingScope>> {
        let path = self.chain.clone();
        blocking(move || Ok(Replica::sharing_scope(&path)?.map(SharingScope::from))).await
    }

    /// Apply an explicit scope choice after the caller has drained all old workers.
    ///
    /// # Errors
    /// Rejects rebinding and attempts to keep an absent or inactive boundary.
    pub async fn configure(&self, space: &str, choice: ScopeChoice) -> Result<SharingScope> {
        if !crate::invitation::valid_id(space) {
            return Err(Error::Invalid);
        }
        let path = self.chain.clone();
        let space = space.to_owned();
        blocking(move || {
            let previous = Replica::sharing_scope(&path)?;
            if previous.as_ref().is_some_and(|scope| scope.space != space) {
                return Err(Error::Conflict);
            }
            if choice == ScopeChoice::Keep {
                let scope = previous.ok_or(Error::Invalid)?;
                if !scope.active {
                    return Err(Error::Forbidden);
                }
                return Ok(scope.into());
            }
            let replica = Replica::open(&path, &space, false)?;
            Ok(replica
                .set_sharing_scope(choice == ScopeChoice::All)?
                .into())
        })
        .await
    }

    /// Approve only the exact certificate verified through the invitation channel.
    ///
    /// # Errors
    /// Rejects certificate mismatch, capacity or native persistence failures.
    pub async fn approve(&self, space: &str, device: &PublicDevice) -> Result<()> {
        crate::invitation::verify_device(device)?;
        let path = self.chain.clone();
        let space = space.to_owned();
        let certificate = device.certificate.clone();
        blocking(move || {
            let _approved = Membership::open(&path, &space)?.approve(&certificate)?;
            Ok(())
        })
        .await
    }

    /// Revoke in native storage; existing `SecurePeer` workers recheck every turn.
    ///
    /// # Errors
    /// Returns native binding or persistence failures.
    pub async fn revoke(&self, space: &str, fingerprint: &str) -> Result<()> {
        let path = self.chain.clone();
        let space = space.to_owned();
        let fingerprint = fingerprint.to_owned();
        blocking(move || Ok(Membership::open(&path, &space)?.revoke(&fingerprint)?)).await
    }

    /// Read currently approved device certificates from the engine.
    ///
    /// # Errors
    /// Reports native binding/storage failures.
    pub async fn devices(&self, space: &str) -> Result<Vec<PublicDevice>> {
        let path = self.chain.clone();
        let space = space.to_owned();
        blocking(move || Ok(Membership::open(&path, &space)?.devices()?)).await
    }

    pub(crate) async fn open_peer(
        &self,
        space: String,
        remote: Option<String>,
    ) -> Result<NativePeer> {
        let engine = self.clone();
        let (sender, mut receiver) = mpsc::channel::<Work>(1);
        let (ready, opened) = oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let opened = (|| {
                let identity = DeviceIdentity::load_or_create(&engine.device_directory)?;
                Ok::<_, Error>(SecurePeer::open(
                    &engine.chain,
                    &space,
                    &identity,
                    remote.as_deref(),
                )?)
            })();
            let mut peer = match opened {
                Ok(peer) => {
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    peer
                }
                Err(error) => {
                    let _sent = ready.send(Err(error));
                    return;
                }
            };
            while let Some(work) = receiver.blocking_recv() {
                let result = peer
                    .turn(&work.bytes, work.tick)
                    .map(|bytes| Turn {
                        bytes,
                        device: peer.device().cloned(),
                        progress: peer.progress().clone(),
                    })
                    .map_err(Error::from);
                let failed = result.is_err();
                if work.reply.send(result).is_err() || failed {
                    break;
                }
            }
            let _closed = peer.close();
        });
        opened.await.map_err(|_error| Error::Storage)??;
        Ok(NativePeer { sender, task })
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_error| Error::Storage)?
}

#[derive(Debug)]
pub(crate) struct NativePeer {
    sender: mpsc::Sender<Work>,
    task: JoinHandle<()>,
}

#[derive(Debug)]
struct Work {
    bytes: Vec<u8>,
    tick: bool,
    reply: oneshot::Sender<Result<Turn>>,
}

#[derive(Debug)]
pub(crate) struct Turn {
    pub bytes: Vec<u8>,
    pub device: Option<PublicDevice>,
    pub progress: Progress,
}

impl NativePeer {
    pub(crate) async fn turn(&mut self, bytes: Vec<u8>, tick: bool) -> Result<Turn> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(Work { bytes, tick, reply })
            .await
            .map_err(|_error| Error::Transport)?;
        response.await.map_err(|_error| Error::Transport)?
    }

    pub(crate) async fn close(self) -> Result<()> {
        drop(self.sender);
        self.task.await.map_err(|_error| Error::Storage)
    }
}
