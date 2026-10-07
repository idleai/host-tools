use idle_protocol::v1::{
    configuration::ConfigurationDocument,
    events::RecoveryCursor,
    grants::{ComputePermission, GrantScope, ProviderPermission, SessionPermission},
    identity::{ContributorId, EventPosition},
    standalone::{RepositoryEvent, RepositoryRecovery, RepositorySnapshot},
};

use crate::{Error, Result};

use super::{Authority, Principal, state::State};

impl State {
    pub(super) fn cursor(&self, contributor: &ContributorId) -> RecoveryCursor {
        RecoveryCursor {
            workspace_id: self.workspace.value.id.clone(),
            contributor_id: contributor.clone(),
            stream_id: self.stream.clone(),
            position: EventPosition(self.sequence),
        }
    }

    pub(super) fn snapshot(&self, contributor: &ContributorId, now: u64) -> RepositorySnapshot {
        let mut snapshot = self.all_records(contributor);
        snapshot.sessions.retain(|session| {
            self.allowed(
                contributor,
                &GrantScope::Session {
                    session_id: session.value.id.clone(),
                    permissions: vec![SessionPermission::Observe],
                },
                now,
            )
        });
        snapshot.hosts.retain(|host| {
            self.allowed(
                contributor,
                &GrantScope::Compute {
                    host_id: host.value.id.clone(),
                    permissions: vec![ComputePermission::Connect],
                },
                now,
            )
        });
        snapshot.providers.retain(|provider| {
            self.allowed(
                contributor,
                &GrantScope::Provider {
                    provider_id: provider.value.id.clone(),
                    permissions: vec![ProviderPermission::UseModels],
                },
                now,
            )
        });
        let admin = self.administrator(contributor).is_ok();
        snapshot.grants.retain(|grant| {
            admin || grant.value.grantee == *contributor || grant.value.granted_by == *contributor
        });
        snapshot
    }

    pub(super) fn all_records(&self, contributor: &ContributorId) -> RepositorySnapshot {
        RepositorySnapshot {
            as_of: self.cursor(contributor),
            workspace: self.workspace.clone(),
            memberships: self.memberships.values().cloned().collect(),
            sessions: self.sessions.values().cloned().collect(),
            hosts: self.hosts.values().cloned().collect(),
            providers: self.providers.values().cloned().collect(),
            grants: self.grants.values().cloned().collect(),
            control: self.control.clone(),
            settings: self
                .configuration
                .get(&ConfigurationDocument::Settings)
                .cloned(),
            agent_rules: self
                .configuration
                .get(&ConfigurationDocument::AgentRules)
                .cloned(),
            views: self.views.values().cloned().collect(),
        }
    }
}

impl Authority {
    /// Obtain an atomic authorized snapshot; this does not imply runtime availability.
    ///
    /// # Errors
    /// Rejects revoked members or an unavailable authority.
    pub fn snapshot(&self, principal: &Principal) -> Result<RepositorySnapshot> {
        self.healthy()?;
        let contributor = &principal.contributor.contributor_id;
        let _role = self
            .state
            .member(contributor)
            .map_err(|_error| Error::Forbidden)?;
        let mut snapshot = self.state.snapshot(contributor, self.now()?);
        if let Some(files) = &self.state.repository_files {
            super::repository::configured_resources(&mut snapshot, &files.configuration);
        }
        Ok(snapshot)
    }

    /// Recover ordered notifications, or explicitly request a replacement snapshot.
    ///
    /// # Errors
    /// Rejects another workspace/audience, future positions and invalid page sizes.
    pub fn catch_up(
        &self,
        principal: &Principal,
        after: &RecoveryCursor,
        limit: usize,
    ) -> Result<RepositoryRecovery> {
        self.healthy()?;
        let contributor = &principal.contributor.contributor_id;
        let _role = self
            .state
            .member(contributor)
            .map_err(|_error| Error::Forbidden)?;
        if after.workspace_id != self.state.workspace.value.id
            || after.contributor_id != *contributor
            || after.position.0 > self.state.sequence
            || !(1..=256).contains(&limit)
        {
            return Err(Error::Invalid);
        }
        if after.stream_id != self.state.stream
            || self
                .state
                .notices
                .front()
                .is_some_and(|(sequence, _)| after.position.0.saturating_add(1) < *sequence)
        {
            return Ok(RepositoryRecovery::SnapshotRequired);
        }
        let mut through = after.clone();
        let events: Vec<_> = self
            .state
            .notices
            .iter()
            .filter(|(sequence, _)| *sequence > after.position.0)
            .take(limit)
            .map(|(sequence, notice)| {
                through.position = EventPosition(*sequence);
                RepositoryEvent {
                    cursor: through.clone(),
                    notice: notice.clone(),
                }
            })
            .collect();
        let has_more = through.position.0 < self.state.sequence;
        Ok(RepositoryRecovery::Events {
            events,
            through,
            has_more,
        })
    }
}
