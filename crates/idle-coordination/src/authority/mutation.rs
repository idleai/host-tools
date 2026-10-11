use idle_protocol::v1::{
    Change, Record, WriteCondition,
    api::{ErrorCode, Request},
    configuration::ConfigurationValue,
    grants::{ComputePermission, Grant, GrantCommand, GrantScope, GrantStatus, SessionPermission},
    identity::{ContributorId, RepositoryId, Timestamp},
    membership::{MembershipStatus, Role},
    resources::ProviderKind,
    standalone::{ChangeNotice, Mutation, MutationValue},
    workspace::{CoordinationMode, Workspace},
};

use super::{
    Principal,
    access::nonempty,
    state::{MAX_ENTITIES, State},
    validation::{self, Checked, failure, require},
};

impl State {
    pub(super) fn apply(
        &mut self,
        principal: &Principal,
        request: &Request<Mutation>,
        now: u64,
    ) -> Checked<(MutationValue, ChangeNotice)> {
        let actor = &principal.contributor.contributor_id;
        let _role = self.member(actor)?;
        require(
            self.handoff.is_none() && self.runtime_transfer.is_none(),
            ErrorCode::Conflict,
        )?;
        if let Some(fence) = &request.control_fence {
            require(self.current(principal, fence, now), ErrorCode::StaleControl)?;
        }
        match &request.body {
            Mutation::Workspace(change) => {
                self.administrator(actor)?;
                validation::workspace(&change.value)?;
                require(
                    change.value.id == self.workspace.value.id
                        && change.value.chain == self.workspace.value.chain
                        && repository_id(&change.value) == repository_id(&self.workspace.value),
                    ErrorCode::Conflict,
                )?;
                self.workspace = validation::record(
                    Some(&self.workspace),
                    &change.expected,
                    change.value.clone(),
                )?;
                Ok((
                    MutationValue::Workspace(self.workspace.clone()),
                    ChangeNotice::Workspace,
                ))
            }
            Mutation::Configuration(write) => {
                self.administrator(actor)?;
                validation::configuration(&write.change.value)?;
                let current = self.configuration.get(&write.document);
                if let Some(record) = current {
                    require(
                        record.value.schema_version == 1,
                        ErrorCode::UnsupportedVersion,
                    )?;
                }
                let mut record = validation::record(
                    current,
                    &write.change.expected,
                    write.change.value.clone(),
                )?;
                if self.repository_files.is_some() {
                    record.revision.0 = record.revision.0.max(self.sequence.saturating_add(1));
                }
                let _previous = self.configuration.insert(write.document, record.clone());
                Ok((
                    MutationValue::Configuration(record),
                    ChangeNotice::Configuration(write.document),
                ))
            }
            Mutation::View(change) => {
                self.administrator(actor)?;
                validation::id(&change.value.id)?;
                validation::label(&change.value.title)?;
                validation::configuration(&ConfigurationValue {
                    schema_version: change.value.schema_version,
                    json: change.value.json.clone(),
                })?;
                capacity(&self.views, &change.value.id)?;
                let mut record = validation::record(
                    self.views.get(&change.value.id),
                    &change.expected,
                    change.value.clone(),
                )?;
                if self.repository_files.is_some() {
                    record.revision.0 = record.revision.0.max(self.sequence.saturating_add(1));
                }
                let _previous = self.views.insert(change.value.id.clone(), record.clone());
                Ok((MutationValue::View(record), ChangeNotice::Views))
            }
            Mutation::Session(change) => {
                self.validate_owner(actor, &change.value.owner)?;
                validation::id(&change.value.id.0)?;
                validation::id(&change.value.runtime.runtime_id.0)?;
                validation::label(&change.value.title)?;
                require(
                    self.allowed(
                        actor,
                        &GrantScope::Compute {
                            host_id: change.value.runtime.host_id.clone(),
                            permissions: vec![ComputePermission::Connect],
                        },
                        now,
                    ),
                    ErrorCode::Forbidden,
                )?;
                if let Some(parent) = &change.value.parent {
                    require(
                        *parent != change.value.id
                            && self.allowed(
                                actor,
                                &GrantScope::Session {
                                    session_id: parent.clone(),
                                    permissions: vec![SessionPermission::Observe],
                                },
                                now,
                            ),
                        ErrorCode::InvalidRequest,
                    )?;
                }
                if let Some(existing) = self.sessions.get(&change.value.id.0) {
                    require(
                        existing.value.owner == change.value.owner
                            && existing.value.kind == change.value.kind
                            && existing.value.parent == change.value.parent,
                        ErrorCode::Conflict,
                    )?;
                }
                capacity(&self.sessions, &change.value.id.0)?;
                let record = validation::record(
                    self.sessions.get(&change.value.id.0),
                    &change.expected,
                    change.value.clone(),
                )?;
                let _previous = self
                    .sessions
                    .insert(change.value.id.0.clone(), record.clone());
                Ok((MutationValue::Session(record), ChangeNotice::Directory))
            }
            Mutation::Host(change) => {
                self.validate_owner(actor, &change.value.owner)?;
                validation::id(&change.value.id.0)?;
                validation::label(&change.value.name)?;
                routes(&change.value.routes)?;
                require(
                    change.value.health.valid_until > change.value.health.observed_at
                        && change.value.health.valid_until.0 <= now.saturating_add(300_000),
                    ErrorCode::InvalidRequest,
                )?;
                if let Some(existing) = self.hosts.get(&change.value.id.0) {
                    require(
                        existing.value.owner == change.value.owner,
                        ErrorCode::Conflict,
                    )?;
                }
                capacity(&self.hosts, &change.value.id.0)?;
                let mut record = validation::record(
                    self.hosts.get(&change.value.id.0),
                    &change.expected,
                    change.value.clone(),
                )?;
                if self
                    .resource_revisions
                    .hosts
                    .contains_key(&change.value.id.0)
                {
                    record.revision = self
                        .advance_host_revision(&change.value.id.0)
                        .map_err(|_error| failure(ErrorCode::Conflict))?;
                }
                let _previous = self.hosts.insert(change.value.id.0.clone(), record.clone());
                Ok((MutationValue::Host(record), ChangeNotice::Directory))
            }
            Mutation::Provider(change) => {
                self.validate_owner(actor, &change.value.owner)?;
                validation::id(&change.value.id.0)?;
                validation::label(&change.value.name)?;
                routes(&change.value.routes)?;
                if let ProviderKind::Local {
                    host_id,
                    runtime_id,
                } = &change.value.kind
                {
                    require(
                        self.hosts
                            .get(&host_id.0)
                            .is_some_and(|host| host.value.owner == *actor),
                        ErrorCode::Forbidden,
                    )?;
                    validation::id(&runtime_id.0)?;
                }
                require(
                    change.value.health.valid_until > change.value.health.observed_at
                        && change.value.health.valid_until.0 <= now.saturating_add(300_000),
                    ErrorCode::InvalidRequest,
                )?;
                if let Some(existing) = self.providers.get(&change.value.id.0) {
                    require(
                        existing.value.owner == change.value.owner
                            && existing.value.kind == change.value.kind,
                        ErrorCode::Conflict,
                    )?;
                }
                capacity(&self.providers, &change.value.id.0)?;
                let mut record = validation::record(
                    self.providers.get(&change.value.id.0),
                    &change.expected,
                    change.value.clone(),
                )?;
                if self
                    .resource_revisions
                    .providers
                    .contains_key(&change.value.id.0)
                {
                    record.revision = self
                        .advance_provider_revision(&change.value.id.0)
                        .map_err(|_error| failure(ErrorCode::Conflict))?;
                }
                let _previous = self
                    .providers
                    .insert(change.value.id.0.clone(), record.clone());
                Ok((MutationValue::Provider(record), ChangeNotice::Directory))
            }
            Mutation::Membership(change) => {
                require(
                    *actor == self.owner
                        && change.value.contributor_id != self.owner
                        && change.value.role != Role::Owner,
                    ErrorCode::Forbidden,
                )?;
                validation::id(&change.value.contributor_id.0)?;
                capacity(&self.memberships, &change.value.contributor_id.0)?;
                let current = self.memberships.get(&change.value.contributor_id.0);
                // A revoked identity must not silently reactivate its old grants.
                require(
                    current.is_none_or(|old| {
                        old.value.status != MembershipStatus::Revoked
                            || change.value.status == MembershipStatus::Revoked
                    }),
                    ErrorCode::Forbidden,
                )?;
                let record = validation::record(current, &change.expected, change.value.clone())?;
                let _previous = self
                    .memberships
                    .insert(change.value.contributor_id.0.clone(), record.clone());
                Ok((MutationValue::Membership(record), ChangeNotice::Access))
            }
            Mutation::Grant(command) => {
                let record = self.apply_grant(actor, command, now)?;
                Ok((MutationValue::Grant(record), ChangeNotice::Access))
            }
            Mutation::Control(command) => {
                self.apply_control(principal, command, now)?;
                Ok((
                    MutationValue::Control(self.control.clone()),
                    ChangeNotice::Control,
                ))
            }
        }
    }

    fn validate_owner(&self, actor: &ContributorId, owner: &ContributorId) -> Checked<()> {
        require(
            actor == owner && self.member(actor)? != Role::Viewer,
            ErrorCode::Forbidden,
        )
    }

    fn apply_grant(
        &mut self,
        actor: &ContributorId,
        command: &GrantCommand,
        now: u64,
    ) -> Checked<Record<Grant>> {
        let (id, change) = match command {
            GrantCommand::Issue {
                grant_id,
                grantee,
                scope,
                expires_at,
            } => {
                validation::id(&grant_id.0)?;
                let _member = self.member(grantee)?;
                require(
                    self.resource_owner(scope) == Some(actor) && nonempty(scope),
                    ErrorCode::Forbidden,
                )?;
                require(
                    expires_at.is_none_or(|deadline| deadline.0 > now),
                    ErrorCode::InvalidRequest,
                )?;
                (
                    grant_id,
                    Change {
                        expected: WriteCondition::Absent,
                        value: Grant {
                            id: grant_id.clone(),
                            grantee: grantee.clone(),
                            granted_by: actor.clone(),
                            scope: scope.clone(),
                            expires_at: *expires_at,
                            status: GrantStatus::Active,
                        },
                    },
                )
            }
            GrantCommand::Revoke {
                grant_id,
                expected_revision,
            } => {
                let previous = self
                    .grants
                    .get(&grant_id.0)
                    .ok_or_else(|| failure(ErrorCode::NotFound))?;
                require(
                    previous.value.granted_by == *actor || self.administrator(actor).is_ok(),
                    ErrorCode::Forbidden,
                )?;
                let mut value = previous.value.clone();
                require(value.status == GrantStatus::Active, ErrorCode::Conflict)?;
                value.status = GrantStatus::Revoked {
                    revoked_at: Timestamp(now),
                    revoked_by: actor.clone(),
                };
                (
                    grant_id,
                    Change {
                        expected: WriteCondition::Revision(*expected_revision),
                        value,
                    },
                )
            }
        };
        capacity(&self.grants, &id.0)?;
        let record = validation::record(self.grants.get(&id.0), &change.expected, change.value)?;
        let _previous = self.grants.insert(id.0.clone(), record.clone());
        Ok(record)
    }
}

fn capacity<T>(map: &std::collections::BTreeMap<String, T>, key: &str) -> Checked<()> {
    require(
        map.contains_key(key) || map.len() < MAX_ENTITIES,
        ErrorCode::RateLimited,
    )
}

fn routes(routes: &[idle_protocol::v1::resources::ConnectionRoute]) -> Checked<()> {
    require(routes.len() <= 8, ErrorCode::InvalidRequest)?;
    for route in routes {
        validation::id(&route.protocol)?;
        validation::id(&route.reference)?;
        // References are lookup IDs, never URLs, invitations or bearer grants.
        require(
            !route.reference.contains([':', '/', '?', '@']),
            ErrorCode::InvalidRequest,
        )?;
    }
    Ok(())
}

pub(super) fn repository_id(workspace: &Workspace) -> Option<&RepositoryId> {
    match &workspace.mode {
        CoordinationMode::Standalone { repository } => Some(&repository.id),
        CoordinationMode::Managed { repositories } => {
            repositories.first().map(|repository| &repository.id)
        }
    }
}
