use idle_protocol::v1::{
    api::ErrorCode,
    grants::{GrantScope, GrantStatus},
    identity::ContributorId,
    membership::{MembershipStatus, Role},
    standalone::AccessCheck,
};

use crate::{Error, Result};

use super::{
    Authority, Principal,
    state::State,
    validation::{Checked, require},
};

impl State {
    pub(super) fn member(&self, contributor: &ContributorId) -> Checked<Role> {
        let member = self
            .memberships
            .get(&contributor.0)
            .filter(|record| record.value.status == MembershipStatus::Active)
            .ok_or_else(|| super::validation::failure(ErrorCode::Forbidden))?;
        Ok(member.value.role)
    }

    pub(super) fn administrator(&self, contributor: &ContributorId) -> Checked<()> {
        require(
            matches!(self.member(contributor)?, Role::Owner | Role::Admin),
            ErrorCode::Forbidden,
        )
    }

    pub(super) fn resource_owner(&self, scope: &GrantScope) -> Option<&ContributorId> {
        match scope {
            GrantScope::Session { session_id, .. } => self
                .sessions
                .get(&session_id.0)
                .map(|record| &record.value.owner),
            GrantScope::Compute { host_id, .. } => {
                self.hosts.get(&host_id.0).map(|record| &record.value.owner)
            }
            GrantScope::Provider { provider_id, .. } => self
                .providers
                .get(&provider_id.0)
                .map(|record| &record.value.owner),
        }
    }

    pub(super) fn allowed(
        &self,
        contributor: &ContributorId,
        scope: &GrantScope,
        now: u64,
    ) -> bool {
        if self.member(contributor).is_err() || !nonempty(scope) {
            return false;
        }
        if self.resource_owner(scope) == Some(contributor) {
            return true;
        }
        // Each permission must have a current grant; independent grants can cover
        // distinct actions on this same resource without authorizing other kinds.
        match scope {
            GrantScope::Session { session_id, permissions } => permissions.iter().all(|permission| self.grants.values().any(|record| {
                let grant = &record.value;
                self.active_grant(grant, contributor, now) && matches!(&grant.scope,
                    GrantScope::Session { session_id: id, permissions: granted } if id == session_id && granted.contains(permission))
            })),
            GrantScope::Compute { host_id, permissions } => permissions.iter().all(|permission| self.grants.values().any(|record| {
                let grant = &record.value;
                self.active_grant(grant, contributor, now) && matches!(&grant.scope,
                    GrantScope::Compute { host_id: id, permissions: granted } if id == host_id && granted.contains(permission))
            })),
            GrantScope::Provider { provider_id, permissions } => permissions.iter().all(|permission| self.grants.values().any(|record| {
                let grant = &record.value;
                self.active_grant(grant, contributor, now) && matches!(&grant.scope,
                    GrantScope::Provider { provider_id: id, permissions: granted } if id == provider_id && granted.contains(permission))
            })),
        }
    }

    fn active_grant(
        &self,
        grant: &idle_protocol::v1::grants::Grant,
        contributor: &ContributorId,
        now: u64,
    ) -> bool {
        grant.grantee == *contributor
            && grant.status == GrantStatus::Active
            && grant.expires_at.is_none_or(|deadline| deadline.0 > now)
            && self.member(&grant.granted_by).is_ok()
            && self.resource_owner(&grant.scope) == Some(&grant.granted_by)
    }
}

pub(super) fn nonempty(scope: &GrantScope) -> bool {
    match scope {
        GrantScope::Session { permissions, .. } => {
            !permissions.is_empty() && permissions.len() <= 3
        }
        GrantScope::Compute { permissions, .. } => {
            !permissions.is_empty() && permissions.len() <= 5
        }
        GrantScope::Provider { permissions, .. } => {
            !permissions.is_empty() && permissions.len() <= 2
        }
    }
}

impl Authority {
    /// Recheck membership, exact resource grants and expiry on an active connection.
    /// A runtime may check another contributor only for its registered host/session.
    /// f15/f17 must call this at the execution boundary and apply sandbox/rules too.
    ///
    /// # Errors
    /// Rejects foreign runtimes, frozen/adopted authority or unavailable state.
    pub fn check_access(&self, principal: &Principal, check: &AccessCheck) -> Result<bool> {
        self.healthy()?;
        if self.state.handoff.is_some() {
            return Err(Error::Forbidden);
        }
        let _role = self
            .state
            .member(&principal.contributor.contributor_id)
            .map_err(|_error| Error::Forbidden)?;
        if principal.contributor.contributor_id != check.contributor_id {
            let runtime = principal.runtime.as_ref().ok_or(Error::Forbidden)?;
            let owns_host = self
                .state
                .hosts
                .get(&runtime.host_id.0)
                .is_some_and(|host| host.value.owner == principal.contributor.contributor_id);
            let bound = match &check.scope {
                GrantScope::Session { session_id, .. } => self
                    .state
                    .sessions
                    .get(&session_id.0)
                    .is_some_and(|session| session.value.runtime == *runtime),
                GrantScope::Compute { host_id, .. } => *host_id == runtime.host_id,
                GrantScope::Provider { .. } => false,
            };
            if !owns_host || !bound {
                return Err(Error::Forbidden);
            }
        }
        Ok(self
            .state
            .allowed(&check.contributor_id, &check.scope, self.now()?))
    }
}
