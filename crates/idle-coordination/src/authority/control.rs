use idle_protocol::v1::{
    api::ErrorCode,
    control::{ControlCommand, ControlFence, ControlHolder, ControlLease, ControlValidation},
    grants::{ComputePermission, GrantScope},
    identity::{ControlEpoch, Timestamp},
    sessions::{RuntimeBinding, SessionKind},
};

use crate::{Error, Result};

use super::{
    Authority, Principal,
    state::State,
    validation::{Checked, failure, require},
};

impl State {
    pub(super) fn validate_holder(
        &self,
        principal: &Principal,
        holder: &ControlHolder,
        now: u64,
    ) -> Checked<()> {
        require(
            principal.runtime.as_ref()
                == Some(&RuntimeBinding {
                    host_id: holder.host_id.clone(),
                    runtime_id: holder.runtime_id.clone(),
                }),
            ErrorCode::Unauthenticated,
        )?;
        let session = self
            .sessions
            .get(&holder.session_id.0)
            .ok_or_else(|| failure(ErrorCode::NotFound))?;
        require(
            session.value.kind == SessionKind::Control
                && session.value.owner == principal.contributor.contributor_id
                && session.value.runtime.host_id == holder.host_id
                && session.value.runtime.runtime_id == holder.runtime_id,
            ErrorCode::Forbidden,
        )?;
        require(
            self.allowed(
                &principal.contributor.contributor_id,
                &GrantScope::Compute {
                    host_id: holder.host_id.clone(),
                    permissions: vec![ComputePermission::Execute],
                },
                now,
            ),
            ErrorCode::Forbidden,
        )
    }

    pub(super) fn current(&self, principal: &Principal, fence: &ControlFence, now: u64) -> bool {
        self.handoff.is_none()
            && self.validate_holder(principal, &fence.holder, now).is_ok()
            && self
                .control
                .lease
                .as_ref()
                .is_some_and(|lease| lease.fence == *fence && lease.expires_at.0 > now)
    }

    pub(super) fn apply_control(
        &mut self,
        principal: &Principal,
        command: &ControlCommand,
        now: u64,
    ) -> Checked<()> {
        match command {
            ControlCommand::Acquire {
                holder,
                expected_epoch,
                lease_duration_ms,
            } => {
                self.validate_holder(principal, holder, now)?;
                require(
                    self.control.last_epoch == *expected_epoch
                        && self
                            .control
                            .lease
                            .as_ref()
                            .is_none_or(|lease| lease.expires_at.0 <= now),
                    ErrorCode::StaleControl,
                )?;
                let expiry = expires(now, lease_duration_ms.get())?;
                let epoch = ControlEpoch(
                    self.control
                        .last_epoch
                        .0
                        .checked_add(1)
                        .ok_or_else(|| failure(ErrorCode::Conflict))?,
                );
                self.control.last_epoch = epoch;
                self.control.lease = Some(ControlLease {
                    fence: ControlFence {
                        workspace_id: self.workspace.value.id.clone(),
                        epoch,
                        holder: holder.clone(),
                    },
                    acquired_at: Timestamp(now),
                    expires_at: Timestamp(expiry),
                });
            }
            ControlCommand::Renew {
                fence,
                lease_duration_ms,
            } => {
                require(self.current(principal, fence, now), ErrorCode::StaleControl)?;
                let expiry = expires(now, lease_duration_ms.get())?;
                let lease = self
                    .control
                    .lease
                    .as_mut()
                    .ok_or_else(|| failure(ErrorCode::StaleControl))?;
                lease.expires_at = Timestamp(lease.expires_at.0.max(expiry));
            }
            ControlCommand::Release { fence } => {
                require(self.current(principal, fence, now), ErrorCode::StaleControl)?;
                self.control.lease = None;
            }
        }
        Ok(())
    }
}

fn expires(now: u64, duration: u32) -> Checked<u64> {
    require((1..=60_000).contains(&duration), ErrorCode::InvalidRequest)?;
    now.checked_add(u64::from(duration))
        .ok_or_else(|| failure(ErrorCode::InvalidRequest))
}

impl Authority {
    /// Validate exact controller identity, epoch and expiry against current state.
    /// This is advisory until the runtime rechecks it at execution/append time.
    ///
    /// # Errors
    /// Fails closed for unavailable storage or an unauthenticated runtime.
    pub fn validate_control(
        &self,
        principal: &Principal,
        fence: &ControlFence,
    ) -> Result<ControlValidation> {
        self.healthy()?;
        let now = self.now()?;
        if principal.runtime.is_none() {
            return Err(Error::Forbidden);
        }
        if self.state.current(principal, fence, now) {
            let lease = self.state.control.lease.clone().ok_or(Error::Invalid)?;
            Ok(ControlValidation::Current {
                lease,
                checked_at: Timestamp(now),
            })
        } else {
            Ok(ControlValidation::Stale {
                ownership: self.state.control.clone(),
                checked_at: Timestamp(now),
            })
        }
    }
}
