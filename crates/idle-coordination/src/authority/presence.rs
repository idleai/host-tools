use idle_protocol::v1::{
    grants::{ComputePermission, GrantScope},
    standalone::Presence,
};

use crate::{Error, Result};

use super::{Authority, Principal, mutation::repository_id};

impl Authority {
    /// Publish bounded, expiring presence for the authenticated connection.
    /// Presence is intentionally absent after service restart.
    ///
    /// # Errors
    /// Rejects another contributor/repository, hidden hosts, stale times or paths.
    pub fn publish_presence(&mut self, principal: &Principal, entry: Presence) -> Result<()> {
        self.healthy()?;
        let actor = &principal.contributor.contributor_id;
        let now = self.now()?;
        let _role = self
            .state
            .member(actor)
            .map_err(|_error| Error::Forbidden)?;
        if self.state.handoff.is_some()
            || entry.contributor_id != *actor
            || Some(&entry.repository_id) != repository_id(&self.state.workspace.value)
            || entry.connection_id.is_empty()
            || entry.connection_id.len() > 256
            || entry.observed_at.0 > now.saturating_add(5_000)
            || entry.observed_at >= entry.valid_until
            || entry.valid_until.0 <= now
            || entry.valid_until.0 > now.saturating_add(120_000)
            || entry
                .branch
                .as_ref()
                .is_some_and(|text| text.len() > 256 || text.chars().any(char::is_control))
            || entry
                .summary
                .as_ref()
                .is_some_and(|text| text.len() > 1024 || text.chars().any(char::is_control))
            || entry.file.as_ref().is_some_and(|path| !relative_file(path))
        {
            return Err(Error::Invalid);
        }
        if let Some(host_id) = &entry.host_id
            && !self.state.allowed(
                actor,
                &GrantScope::Compute {
                    host_id: host_id.clone(),
                    permissions: vec![ComputePermission::Connect],
                },
                now,
            )
        {
            return Err(Error::Forbidden);
        }
        self.presence.retain(|_, value| value.valid_until.0 > now);
        if let Some(previous) = self.presence.get(&entry.connection_id) {
            if previous.contributor_id != *actor {
                return Err(Error::Forbidden);
            }
            if entry.observed_at < previous.observed_at {
                return Err(Error::Conflict);
            }
        } else if self.presence.len() >= 256 {
            return Err(Error::Busy);
        }
        let _previous = self.presence.insert(entry.connection_id.clone(), entry);
        Ok(())
    }

    /// Remove only presence owned by this authenticated contributor.
    ///
    /// # Errors
    /// Rejects removal of another contributor's connection.
    pub fn remove_presence(&mut self, principal: &Principal, connection: &str) -> Result<()> {
        self.healthy()?;
        if self
            .presence
            .get(connection)
            .is_some_and(|entry| entry.contributor_id != principal.contributor.contributor_id)
        {
            return Err(Error::Forbidden);
        }
        let _removed = self.presence.remove(connection);
        Ok(())
    }

    /// Return fresh, authorized presence without exposing hidden host bindings.
    ///
    /// # Errors
    /// Rejects revoked audience membership or unavailable service state.
    pub fn presence(&self, principal: &Principal) -> Result<Vec<Presence>> {
        self.healthy()?;
        let actor = &principal.contributor.contributor_id;
        let _role = self
            .state
            .member(actor)
            .map_err(|_error| Error::Forbidden)?;
        let now = self.now()?;
        Ok(self
            .presence
            .values()
            .filter(|entry| {
                entry.valid_until.0 > now && self.state.member(&entry.contributor_id).is_ok()
            })
            .cloned()
            .map(|mut entry| {
                if entry.host_id.as_ref().is_some_and(|host| {
                    !self.state.allowed(
                        actor,
                        &GrantScope::Compute {
                            host_id: host.clone(),
                            permissions: vec![ComputePermission::Connect],
                        },
                        now,
                    )
                }) {
                    entry.host_id = None;
                }
                entry
            })
            .collect())
    }
}

fn relative_file(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
