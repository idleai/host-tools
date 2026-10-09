//! Durable transfer of standalone metadata to one explicitly approved daemon.

use std::{path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use idle_protocol::v1::{identity::ContributorIdentity, standalone::ChangeNotice};
use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    clock::Clock,
    persistence::{MAX_STATE_BYTES, Persistence},
    workspace_config::RepositoryFiles,
};

use super::{
    Authority, Principal,
    state::{STATE_KEY, State},
};

/// Maximum bytes in one transfer chunk; the complete package remains bounded.
pub const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

/// Host and checkout approved through the daemon owner's private connection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTarget {
    /// Persistent daemon installation identity.
    pub host_id: String,
    /// Exact daemon checkout binding.
    pub checkout_id: String,
}

/// Stable transfer identity and digest, safe to retain outside the private payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReceipt {
    /// Original transfer identity, unchanged on retry.
    pub transfer_id: String,
    /// Destination that accepted ownership.
    pub target: RuntimeTarget,
    /// BLAKE3 digest of the complete transferred package.
    pub package_hash: String,
}

/// A bounded slice of the private package; never log its content.
#[derive(Deserialize, Serialize)]
pub struct RuntimeChunk {
    /// Stable identity and digest for every chunk.
    pub receipt: RuntimeReceipt,
    /// Total decoded bytes in the package.
    pub total: usize,
    /// Decoded byte offset of this chunk.
    pub offset: usize,
    /// Base64 encoded bytes. JSON preserves all original integer values.
    pub content: String,
}

impl std::fmt::Debug for RuntimeChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeChunk")
            .field("receipt", &self.receipt)
            .field("offset", &self.offset)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

/// Trusted daemon startup binding, supplied over its private helper pipe.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDestination {
    /// Approved daemon and checkout.
    pub target: RuntimeTarget,
    /// Workspace from the daemon's registry.
    pub workspace_id: String,
    /// Repository from the daemon's registry.
    pub repository_id: String,
    /// Logical chain from the daemon's registry.
    pub chain_id: String,
    /// Canonical checkout on the daemon host.
    pub checkout_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Transfer {
    pub id: String,
    pub target: RuntimeTarget,
    pub contributor: ContributorIdentity,
    pub accepted: Option<RuntimeReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Owner {
    pub receipt: RuntimeReceipt,
    pub contributor: ContributorIdentity,
}

#[derive(Deserialize, Serialize)]
struct Package {
    version: u16,
    transfer_id: String,
    target: RuntimeTarget,
    contributor: ContributorIdentity,
    state: State,
}

impl Authority {
    /// Freeze new writes before exporting. A failed or lost reply stays frozen.
    /// Repeating the same destination returns the original package identity.
    /// # Errors
    /// Rejects non-owners, another destination, adoption and failed storage.
    pub fn prepare_runtime_transfer(
        &mut self,
        principal: &Principal,
        target: RuntimeTarget,
        configuration_revision: &str,
    ) -> Result<RuntimeReceipt> {
        self.healthy()?;
        if principal.contributor.contributor_id != self.state.owner
            || self.state.handoff.is_some()
            || self.state.runtime_owner.is_some()
        {
            return Err(Error::Forbidden);
        }
        for id in [&target.host_id, &target.checkout_id] {
            super::validation::id(id).map_err(|_error| Error::Invalid)?;
        }
        if let Some(transfer) = &self.state.runtime_transfer {
            if transfer.target != target || transfer.contributor != principal.contributor {
                return Err(Error::Conflict);
            }
        } else {
            self.refresh_repository()?;
            if self.workspace_configuration()?.revision != configuration_revision {
                return Err(Error::Conflict);
            }
            let mut next = self.state.clone();
            next.version = 2;
            if next.control.lease.take().is_some() {
                next.record_change(ChangeNotice::Control)?;
            }
            next.runtime_transfer = Some(Box::new(Transfer {
                id: uuid::Uuid::new_v4().to_string(),
                target,
                contributor: principal.contributor.clone(),
                accepted: None,
            }));
            if serde_json::to_vec(&next)?.len() > MAX_STATE_BYTES.saturating_sub(4096) {
                return Err(Error::Busy);
            }
            self.commit(next)?;
            self.presence.clear();
        }
        let (receipt, _bytes) = self.transfer_bytes(principal)?;
        Ok(receipt)
    }

    /// Read the same frozen package after restart, without changing retry results.
    /// # Errors
    /// Rejects another contributor, missing transfers and invalid offsets.
    pub fn runtime_transfer_chunk(
        &self,
        principal: &Principal,
        offset: usize,
    ) -> Result<RuntimeChunk> {
        let (receipt, bytes) = self.transfer_bytes(principal)?;
        if offset >= bytes.len() {
            return Err(Error::Invalid);
        }
        let end = offset.saturating_add(TRANSFER_CHUNK_BYTES).min(bytes.len());
        Ok(RuntimeChunk {
            receipt,
            total: bytes.len(),
            offset,
            content: STANDARD.encode(bytes.get(offset..end).ok_or(Error::Invalid)?),
        })
    }

    /// Persist the exact daemon acknowledgement. This never unfreezes the source.
    /// # Errors
    /// Rejects mismatched acknowledgements or failed durable writes.
    pub fn complete_runtime_transfer(
        &mut self,
        principal: &Principal,
        receipt: RuntimeReceipt,
    ) -> Result<()> {
        let (expected, _bytes) = self.transfer_bytes(principal)?;
        if receipt != expected {
            return Err(Error::Conflict);
        }
        let mut next = self.state.clone();
        let transfer = next.runtime_transfer.as_mut().ok_or(Error::Invalid)?;
        if transfer.accepted.as_ref() == Some(&receipt) {
            return Ok(());
        }
        transfer.accepted = Some(receipt);
        // The package clock is frozen too, so acknowledgement cannot change its digest.
        let bytes = serde_json::to_vec(&next)?;
        self.commit_serialized(next, bytes)
    }

    fn transfer_bytes(&self, principal: &Principal) -> Result<(RuntimeReceipt, Vec<u8>)> {
        self.healthy()?;
        let transfer = self.state.runtime_transfer.as_ref().ok_or(Error::Invalid)?;
        if transfer.contributor != principal.contributor {
            return Err(Error::Forbidden);
        }
        let mut state = self.state.clone();
        state.runtime_transfer = None;
        let package = Package {
            version: 1,
            transfer_id: transfer.id.clone(),
            target: transfer.target.clone(),
            contributor: transfer.contributor.clone(),
            state,
        };
        let bytes = serde_json::to_vec(&package)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Error::Busy);
        }
        let receipt = RuntimeReceipt {
            transfer_id: transfer.id.clone(),
            target: transfer.target.clone(),
            package_hash: blake3::hash(&bytes).to_hex().to_string(),
        };
        Ok((receipt, bytes))
    }

    /// Atomically accept a frozen standalone authority; identical retries cannot
    /// overwrite subsequent daemon writes. Checkout definitions must match first.
    /// # Errors
    /// Rejects foreign bindings, changed payloads, divergent files and other owners.
    pub fn import_runtime(
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
        destination: &RuntimeDestination,
        client_id: &str,
        bytes: &[u8],
    ) -> Result<Self> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Error::Busy);
        }
        let package: Package = serde_json::from_slice(bytes)?;
        let mut state = package.state;
        state.validate()?;
        if package.version != 1
            || package.target != destination.target
            || package.contributor.contributor_id.0 != client_id
            || state.owner != package.contributor.contributor_id
            || state.handoff.is_some()
            || state.runtime_transfer.is_some()
            || state.runtime_owner.is_some()
            || state.control.lease.is_some()
        {
            return Err(Error::Forbidden);
        }
        let receipt = RuntimeReceipt {
            transfer_id: package.transfer_id,
            target: package.target,
            package_hash: blake3::hash(bytes).to_hex().to_string(),
        };
        if let Some(previous) = storage.load(STATE_KEY)? {
            let previous: State = serde_json::from_slice(&previous)?;
            if previous.runtime_owner.as_ref().map(|owner| &owner.receipt) != Some(&receipt) {
                return Err(Error::Conflict);
            }
            return Self::open_runtime(storage, clock, destination);
        }
        check_destination(&state, destination)?;
        let repository = RepositoryFiles::open(&destination.checkout_root)?;
        if state.repository_files.as_deref() != Some(&repository.observe()?) {
            return Err(Error::Conflict);
        }
        state.version = 2;
        state.runtime_owner = Some(Box::new(Owner {
            receipt,
            contributor: package.contributor,
        }));
        storage.compare_exchange(STATE_KEY, None, Some(&serde_json::to_vec(&state)?))?;
        Self::open_runtime(storage, clock, destination)
    }

    /// Restore an imported authority under the same daemon and workspace binding.
    /// # Errors
    /// Rejects absent or foreign state, changed paths and repository failures.
    pub fn open_runtime(
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
        destination: &RuntimeDestination,
    ) -> Result<Self> {
        let saved: State =
            serde_json::from_slice(&storage.load(STATE_KEY)?.ok_or(Error::Invalid)?)?;
        saved.validate()?;
        check_destination(&saved, destination)?;
        if saved
            .runtime_owner
            .as_ref()
            .map(|owner| &owner.receipt.target)
            != Some(&destination.target)
        {
            return Err(Error::Conflict);
        }
        let repository = RepositoryFiles::open(&destination.checkout_root)?;
        super::repository::recover(storage.as_ref(), &repository)?;
        let mut authority = Self::open(storage, clock, None)?;
        authority.repository = Some(repository);
        authority.refresh_repository()?;
        Ok(authority)
    }

    /// Retrieve the immutable receipt and authenticated owner for this daemon.
    /// # Errors
    /// Rejects an authority that has not accepted a runtime transfer.
    pub fn runtime_owner(&self) -> Result<(RuntimeReceipt, ContributorIdentity)> {
        self.healthy()?;
        let owner = self.state.runtime_owner.as_ref().ok_or(Error::Invalid)?;
        Ok((owner.receipt.clone(), owner.contributor.clone()))
    }

    /// Find the frozen route before opening a client-side coordinator for writes.
    /// # Errors
    /// Rejects another contributor or unavailable private state.
    pub fn runtime_transfer_status(&self, principal: &Principal) -> Result<Option<RuntimeReceipt>> {
        if self.state.runtime_transfer.is_none() {
            self.healthy()?;
            return Ok(None);
        }
        self.transfer_bytes(principal)
            .map(|(receipt, _bytes)| Some(receipt))
    }
}

fn check_destination(state: &State, destination: &RuntimeDestination) -> Result<()> {
    if state.workspace.value.id.0 != destination.workspace_id
        || state.workspace.value.chain.0 != destination.chain_id
        || super::mutation::repository_id(&state.workspace.value).map(|id| id.0.as_str())
            != Some(destination.repository_id.as_str())
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
