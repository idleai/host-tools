//! Durable, one-way handoff to an injected managed coordination provider.

use async_trait::async_trait;
use editchain_sync::PublicDevice;
use idle_protocol::v1::{
    api::{ApiResult, Request},
    identity::{ChainRef, ControlEpoch, WorkspaceId},
    standalone::{ChangeNotice, Mutation, MutationResult, RepositorySnapshot},
    workspace::CoordinationMode,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    engine::{Engine, SharingScope},
};

use super::{Authority, Principal};

/// Original request results the destination must retain for unchanged retries.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RetainedRequest {
    /// Exact original request, including deadline and authenticated contributor.
    pub request: Request<Mutation>,
    /// Original committed result or definitive refusal.
    pub result: ApiResult<MutationResult>,
}

/// Sharing consent summary read from the local engine. The complete enforcement
/// ledger remains in the same chain directory; this summary cannot recreate it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HistoryConsent {
    /// Outgoing mode, cutoff, revision and active flag reported by the engine.
    /// `None` preserves a repository that has never configured history sharing.
    pub scope: Option<SharingScope>,
    /// Previously approved certificates; no credential or connect token.
    pub devices: Vec<PublicDevice>,
}

impl HistoryConsent {
    /// Read the authoritative consent boundary and exact approved certificates.
    ///
    /// # Errors
    /// Reports native storage failures without changing absent or inactive consent.
    pub async fn from_engine(engine: &Engine) -> Result<Self> {
        let scope = engine.scope().await?;
        let devices = if let Some(scope) = &scope {
            engine.devices(&scope.space).await?
        } else {
            Vec::new()
        };
        let result = Self { scope, devices };
        result.validate()?;
        Ok(result)
    }

    pub(super) fn validate(&self) -> Result<()> {
        let Some(scope) = &self.scope else {
            return if self.devices.is_empty() {
                Ok(())
            } else {
                Err(Error::Invalid)
            };
        };
        if !crate::invitation::valid_id(&scope.space)
            || self.devices.len() > 32
            || !matches!(scope.mode.as_str(), "all" | "from_now" | "legacy_from_now")
            || (scope.mode == "from_now") != scope.cutoff_ms.is_some()
        {
            return Err(Error::Forbidden);
        }
        let mut fingerprints = std::collections::BTreeSet::new();
        for device in &self.devices {
            crate::invitation::verify_device(device)?;
            if !fingerprints.insert(&device.fingerprint) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
}

/// Frozen repository handoff. The target must preserve every supplied identity.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdoptionPackage {
    /// Transfer format, independent of the API and engine wire formats.
    pub version: u16,
    /// Stable retry identity for the transfer.
    pub transfer_id: String,
    /// Explicit host-configured destination identity, without credentials.
    pub target: String,
    /// Full repository state; the controller is unassigned with an advanced epoch.
    pub snapshot: RepositorySnapshot,
    /// Original sharing approvals and outgoing history boundary.
    pub history: HistoryConsent,
    /// Original request outcomes, including their original deadlines.
    pub retained_requests: Vec<RetainedRequest>,
}

/// Acknowledgement returned by an authenticated, injected managed adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionReceipt {
    /// Exact transfer identity, unchanged across retries.
    pub transfer_id: String,
    /// Destination that durably accepted the package.
    pub target: String,
    /// BLAKE3 of the exact serialized package accepted by the adapter.
    pub package_hash: String,
    /// Preserved workspace identity.
    pub workspace_id: WorkspaceId,
    /// Preserved logical chain reference.
    pub chain: ChainRef,
    /// Destination's retained watermark, at least the transferred value.
    pub last_epoch: ControlEpoch,
}

/// Managed integration seam. Implementations must authenticate the destination,
/// import atomically, deduplicate `transfer_id`, preserve scope and record IDs,
/// and refuse any preexisting conflicting workspace/chain/session binding.
#[async_trait]
pub trait ManagedAdoption: std::fmt::Debug + Send + Sync {
    /// Durably import the frozen package, or reconcile the same earlier import.
    ///
    /// # Errors
    /// Report uncertain transport outcomes as failures; never invent acceptance.
    async fn import(
        &self,
        package: &AdoptionPackage,
        cancel: &CancellationToken,
    ) -> Result<AdoptionReceipt>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Handoff {
    pub package: AdoptionPackage,
    pub receipt: Option<AdoptionReceipt>,
}

impl AdoptionPackage {
    pub(super) fn validate_receipt(&self, receipt: &AdoptionReceipt) -> Result<()> {
        if receipt.transfer_id != self.transfer_id
            || receipt.target != self.target
            || receipt.package_hash != self.hash()?
            || receipt.workspace_id != self.snapshot.workspace.value.id
            || receipt.chain != self.snapshot.workspace.value.chain
            || receipt.last_epoch < self.snapshot.control.last_epoch
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    /// Exact transfer digest used to bind the managed acknowledgement.
    ///
    /// # Errors
    /// Returns serialization failures.
    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

impl Authority {
    /// Durably freeze local mutations and retire the old controller before export.
    /// An interrupted/uncertain handoff stays frozen across restart. It cannot be
    /// automatically rolled back because the target may already have committed.
    ///
    /// # Errors
    /// Rejects non-owners, changed transfer parameters, inactive scope or failed persistence.
    pub fn prepare_adoption(
        &mut self,
        principal: &Principal,
        target: &str,
        history: HistoryConsent,
    ) -> Result<AdoptionPackage> {
        self.healthy()?;
        if self.state.runtime_transfer.is_some() || self.state.runtime_owner.is_some() {
            return Err(Error::Conflict);
        }
        if principal.contributor.contributor_id != self.state.owner {
            return Err(Error::Forbidden);
        }
        super::validation::id(target).map_err(|_error| Error::Invalid)?;
        history.validate()?;
        if let Some(handoff) = &self.state.handoff {
            if handoff.package.target != target
                || serde_json::to_vec(&handoff.package.history)? != serde_json::to_vec(&history)?
            {
                return Err(Error::Conflict);
            }
            return Ok(handoff.package.clone());
        }
        let mut next = self.state.clone();
        next.control.last_epoch.0 = next
            .control
            .last_epoch
            .0
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        next.control.lease = None;
        next.record_change(ChangeNotice::Access)?;
        let package = AdoptionPackage {
            version: 1,
            transfer_id: uuid::Uuid::new_v4().to_string(),
            target: target.into(),
            snapshot: next.all_records(&next.owner),
            history,
            retained_requests: next
                .receipts
                .iter()
                .map(|stored| RetainedRequest {
                    request: stored.request.clone(),
                    result: stored.result.clone(),
                })
                .collect(),
        };
        next.handoff = Some(Handoff {
            package: package.clone(),
            receipt: None,
        });
        self.commit(next)?;
        self.presence.clear();
        Ok(package)
    }

    /// Complete or retry the frozen transfer through an authenticated managed adapter.
    /// The native service has no implicit Offstage credentials or invented route.
    ///
    /// # Errors
    /// Remains frozen on cancellation, uncertain outcomes or mismatched acknowledgements.
    pub async fn finish_adoption(
        &mut self,
        principal: &Principal,
        adapter: &dyn ManagedAdoption,
        cancel: &CancellationToken,
    ) -> Result<AdoptionReceipt> {
        self.healthy()?;
        if principal.contributor.contributor_id != self.state.owner {
            return Err(Error::Forbidden);
        }
        let handoff = self.state.handoff.clone().ok_or(Error::Invalid)?;
        if let Some(receipt) = handoff.receipt {
            return Ok(receipt);
        }
        let package = &handoff.package;
        let receipt = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(Error::Cancelled),
            result = adapter.import(package, cancel) => result?,
        };
        package.validate_receipt(&receipt)?;
        let mut next = self.state.clone();
        let CoordinationMode::Standalone { repository } = &next.workspace.value.mode else {
            return Err(Error::Conflict);
        };
        next.workspace.value.mode = CoordinationMode::Managed {
            repositories: vec![repository.clone()],
        };
        next.workspace.revision.0 = next
            .workspace
            .revision
            .0
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        next.control.last_epoch = receipt.last_epoch;
        next.handoff = Some(Handoff {
            package: handoff.package,
            receipt: Some(receipt.clone()),
        });
        next.record_change(ChangeNotice::Access)?;
        self.commit(next)?;
        Ok(receipt)
    }

    /// Read the durable pending package for an explicit retry after restart.
    ///
    /// # Errors
    /// Rejects another audience or unavailable storage.
    pub fn pending_adoption(&self, principal: &Principal) -> Result<Option<AdoptionPackage>> {
        self.healthy()?;
        if principal.contributor.contributor_id != self.state.owner {
            return Err(Error::Forbidden);
        }
        Ok(self
            .state
            .handoff
            .as_ref()
            .map(|handoff| handoff.package.clone()))
    }
}
