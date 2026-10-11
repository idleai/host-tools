//! One durable repository authority; replicated history is not a lease election.

mod access;
pub mod adoption;
mod control;
mod mutation;
mod presence;
mod recovery;
mod repository;
mod resources;
pub mod runtime_transfer;
mod state;
pub(crate) mod validation;

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use idle_protocol::v1::{
    ApiVersion,
    api::{ApiResult, ErrorCode, Request, Response},
    identity::{ContributorId, ContributorIdentity},
    sessions::RuntimeBinding,
    standalone::{ChangeNotice, Mutation, MutationResult, Presence},
    workspace::Workspace,
};

use crate::{
    Error, Result,
    clock::Clock,
    persistence::{MAX_STATE_BYTES, Persistence},
};

use self::{
    state::{MAX_DEADLINE_MS, MAX_RECEIPTS, STATE_KEY, State, StoredResult},
    validation::{Checked, failure, require},
};

/// Authenticated connection context, supplied by the host's credential adapter.
/// Never deserialize this from an untrusted request body.
#[derive(Clone, Debug)]
pub struct Principal {
    /// Exact authenticated contributor and external subject.
    pub contributor: ContributorIdentity,
    /// Runtime identity independently authenticated by an execution/host adapter.
    pub runtime: Option<RuntimeBinding>,
}

/// Initial repository binding. Existing storage never silently rebinds identities.
#[derive(Clone, Debug)]
pub struct Bootstrap {
    /// Exactly one repository with its existing logical chain reference.
    pub workspace: Workspace,
    /// Initial owner resolved by the local authenticated host.
    pub owner: ContributorId,
}

/// Durable repository metadata, independent of Node, app-core and UI lifetimes.
#[derive(Debug)]
pub struct Authority {
    storage: Arc<dyn Persistence>,
    clock: Arc<dyn Clock>,
    observed_time: AtomicU64,
    state: State,
    persisted: Vec<u8>,
    presence: BTreeMap<String, Presence>,
    faulted: bool,
    repository: Option<crate::workspace_config::RepositoryFiles>,
}

impl Authority {
    /// Load the authority, or initialize it from an explicit repository binding.
    /// Restart retires active controller leases while retaining their watermark.
    ///
    /// # Errors
    /// Rejects incompatible/corrupt state, rebinding, concurrent owners or failed writes.
    pub fn open(
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
        bootstrap: Option<Bootstrap>,
    ) -> Result<Self> {
        let previous = storage.load(STATE_KEY)?;
        let mut state: State = if let Some(bytes) = &previous {
            serde_json::from_slice(bytes)?
        } else {
            let initial = bootstrap.as_ref().ok_or(Error::Invalid)?;
            State::new(initial.workspace.clone(), initial.owner.clone())?
        };
        state.validate()?;
        if let Some(initial) = bootstrap
            && (state.owner != initial.owner
                || state.workspace.value.id != initial.workspace.id
                || state.workspace.value.chain != initial.workspace.chain
                || mutation::repository_id(&state.workspace.value)
                    != mutation::repository_id(&initial.workspace))
        {
            return Err(Error::Conflict);
        }
        if state.runtime_transfer.is_none() && state.control.lease.take().is_some() {
            state.record_change(ChangeNotice::Control)?;
        }
        if state.runtime_transfer.is_none() {
            state.clock_floor = state.clock_floor.max(clock.now_ms()?);
        }
        let persisted = serde_json::to_vec(&state)?;
        storage.compare_exchange(STATE_KEY, previous.as_deref(), Some(&persisted))?;
        Ok(Self {
            storage,
            clock,
            observed_time: AtomicU64::new(state.clock_floor),
            state,
            persisted,
            presence: BTreeMap::new(),
            faulted: false,
            repository: None,
        })
    }

    /// Atomically authorize, conditionally write and retain an immutable retry result.
    /// Definitive domain refusals are retained too. Storage failures are uncertain.
    ///
    /// # Errors
    /// Returns storage/clock failures or a frozen/managed routing conflict outside
    /// the typed protocol response. Existing retained results remain readable.
    pub fn execute(
        &mut self,
        principal: &Principal,
        request: Request<Mutation>,
    ) -> Result<Response<MutationResult>> {
        self.healthy()?;
        let now = self.now()?;
        let key = request.context.key();
        let result = if let Err(error) = self.authenticate(principal, &request) {
            ApiResult::Failure(error)
        } else {
            if !self
                .state
                .receipts
                .iter()
                .any(|stored| stored.request.context.key() == key)
            {
                self.refresh_repository()?;
            }
            self.execute_authenticated(principal, request, now)?
        };
        Ok(Response {
            api_version: ApiVersion::V1,
            request: key,
            result,
        })
    }

    fn authenticate(&self, principal: &Principal, request: &Request<Mutation>) -> Checked<()> {
        require(
            request.context.contributor == principal.contributor,
            ErrorCode::Unauthenticated,
        )?;
        require(
            request.context.workspace_id == self.state.workspace.value.id,
            ErrorCode::NotFound,
        )?;
        validation::id(&request.context.request_id.0)
    }

    fn execute_authenticated(
        &mut self,
        principal: &Principal,
        request: Request<Mutation>,
        now: u64,
    ) -> Result<ApiResult<MutationResult>> {
        if let Some(previous) = self
            .state
            .receipts
            .iter()
            .find(|stored| stored.request.context.key() == request.context.key())
        {
            return Ok(if previous.request == request {
                previous.result.clone()
            } else {
                ApiResult::Failure(failure(ErrorCode::IdempotencyConflict))
            });
        }
        if self.state.handoff.is_some() || self.state.runtime_transfer.is_some() {
            return Err(Error::Conflict);
        }
        if request.context.expires_at.0 <= now
            || request.context.expires_at.0 > now.saturating_add(MAX_DEADLINE_MS)
        {
            return Ok(ApiResult::Failure(failure(ErrorCode::RequestExpired)));
        }
        let mut next = self.state.clone();
        next.receipts
            .retain(|stored| stored.request.context.expires_at.0 > now);
        if next.receipts.len() >= MAX_RECEIPTS {
            return Ok(ApiResult::Failure(failure(ErrorCode::RateLimited)));
        }
        let original = next.clone();
        let mut file_write = None;
        let result = match next.apply(principal, &request, now) {
            Ok((value, notice)) => {
                if matches!(request.body, Mutation::Grant(_) | Mutation::Membership(_)) {
                    next.advance_resource_access()?;
                }
                file_write = self.prepare_file_write(&mut next, &request.body)?;
                next.record_change(notice)?;
                ApiResult::Success(MutationResult {
                    committed_at: idle_protocol::v1::identity::Timestamp(now),
                    through: next.cursor(&principal.contributor.contributor_id),
                    value,
                })
            }
            Err(error) => {
                next = original;
                ApiResult::Failure(error)
            }
        };
        next.clock_floor = now;
        next.receipts.push(StoredResult {
            request,
            result: result.clone(),
        });
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Ok(ApiResult::Failure(failure(ErrorCode::RateLimited)));
        }
        if let Some(write) = file_write {
            self.commit_repository(next, bytes, write)?;
        } else {
            self.commit_serialized(next, bytes)?;
        }
        Ok(result)
    }

    /// Resolve an earlier mutation using its original key, without re-execution.
    ///
    /// # Errors
    /// Rejects another audience/workspace or unavailable authority state.
    pub fn request_status(
        &self,
        principal: &Principal,
        key: &idle_protocol::v1::identity::RequestKey,
    ) -> Result<Option<ApiResult<MutationResult>>> {
        self.healthy()?;
        if key.workspace_id != self.state.workspace.value.id
            || key.contributor_id != principal.contributor.contributor_id
        {
            return Err(Error::Forbidden);
        }
        Ok(self
            .state
            .receipts
            .iter()
            .find(|stored| stored.request.context.key() == *key)
            .map(|stored| stored.result.clone()))
    }

    fn healthy(&self) -> Result<()> {
        if self.faulted {
            Err(Error::Storage)
        } else {
            Ok(())
        }
    }

    fn now(&self) -> Result<u64> {
        let now = self.clock.now_ms()?;
        Ok(self.observed_time.fetch_max(now, Ordering::SeqCst).max(now))
    }

    fn commit(&mut self, mut next: State) -> Result<()> {
        next.clock_floor = self.now()?;
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Error::Busy);
        }
        self.commit_serialized(next, bytes)
    }

    fn commit_serialized(&mut self, next: State, bytes: Vec<u8>) -> Result<()> {
        if let Err(error) =
            self.storage
                .compare_exchange(STATE_KEY, Some(&self.persisted), Some(&bytes))
        {
            self.faulted = true;
            return Err(error);
        }
        self.persisted = bytes;
        self.state = next;
        Ok(())
    }
}
