//! Runtime-owned session publication and current runner authorization.

use idle_protocol::v1::{
    ApiVersion, Change, WriteCondition,
    api::Request,
    grants::{ComputePermission, GrantScope, SessionPermission},
    identity::{ContributorId, RequestContext, SessionId, Timestamp},
    resources::{Availability, ComputeHost, ConnectionRoute, Health, HostCapability},
    sessions::{RuntimeBinding, Session, SessionKind},
    standalone::{AccessCheck, Mutation},
};
use serde::Serialize;

use crate::{Error, Result};

use super::{Authority, Principal, validation};

#[derive(Debug, Serialize)]
pub(crate) struct RunnerAuthorization {
    contributor_id: ContributorId,
    session: Session,
    configuration_revision: String,
    agent_rules: Option<String>,
}

impl Authority {
    pub(crate) fn register_runner(
        &mut self,
        principal: &Principal,
        session_id: SessionId,
        parent: Option<SessionId>,
    ) -> Result<()> {
        let runtime = self.runner_runtime(principal)?;
        self.refresh_repository()?;
        let now = self.now()?;
        let host = self.state.hosts.get(&runtime.host_id.0);
        let session = self.state.sessions.get(&session_id.0);
        let operations = [
            Mutation::Host(Change {
                expected: host.map_or(WriteCondition::Absent, |record| {
                    WriteCondition::Revision(record.revision)
                }),
                value: ComputeHost {
                    id: runtime.host_id.clone(),
                    owner: principal.contributor.contributor_id.clone(),
                    name: host
                        .map_or_else(|| "Codex compute".into(), |host| host.value.name.clone()),
                    capabilities: vec![HostCapability::Sessions],
                    health: Health {
                        availability: Availability::Available,
                        observed_at: Timestamp(now),
                        valid_until: Timestamp(now.saturating_add(300_000)),
                    },
                    routes: vec![ConnectionRoute {
                        protocol: "codex-idle-runtime".into(),
                        reference: self.runtime_owner()?.0.target.checkout_id,
                    }],
                },
            }),
            Mutation::Session(Change {
                expected: session.map_or(WriteCondition::Absent, |record| {
                    WriteCondition::Revision(record.revision)
                }),
                value: Session {
                    id: session_id,
                    owner: principal.contributor.contributor_id.clone(),
                    title: session.map_or_else(
                        || "Workspace runner".into(),
                        |session| session.value.title.clone(),
                    ),
                    kind: SessionKind::Runner,
                    runtime,
                    parent,
                },
            }),
        ];
        let mut next = self.state.clone();
        // Reuse native metadata validation and revision checks in one durable commit.
        // Publication is not an acknowledgement of thread creation or execution.
        for body in operations {
            let request = Request {
                api_version: ApiVersion::V1,
                context: RequestContext {
                    workspace_id: next.workspace.value.id.clone(),
                    contributor: principal.contributor.clone(),
                    request_id: uuid::Uuid::new_v4().to_string().as_str().into(),
                    expires_at: Timestamp(now.saturating_add(60_000)),
                },
                control_fence: None,
                body,
            };
            let (_value, notice) = next
                .apply(principal, &request, now)
                .map_err(|_error| Error::Forbidden)?;
            next.record_change(notice)?;
        }
        self.commit(next)
    }

    pub(crate) fn authorize_runner(
        &mut self,
        principal: &Principal,
        contributor_id: ContributorId,
        session_id: &SessionId,
        write: bool,
    ) -> Result<RunnerAuthorization> {
        let runtime = self.runner_runtime(principal)?;
        self.refresh_repository()?;
        let session = self
            .state
            .sessions
            .get(&session_id.0)
            .ok_or(Error::Forbidden)?;
        if session.value.kind != SessionKind::Runner || session.value.runtime != runtime {
            return Err(Error::Forbidden);
        }
        let mut permissions = vec![
            ComputePermission::Connect,
            ComputePermission::ReadFiles,
            ComputePermission::Execute,
        ];
        if write {
            permissions.push(ComputePermission::WriteFiles);
        }
        for scope in [
            GrantScope::Session {
                session_id: session_id.clone(),
                permissions: vec![SessionPermission::Observe, SessionPermission::SubmitInput],
            },
            GrantScope::Compute {
                host_id: runtime.host_id,
                permissions,
            },
        ] {
            if !self.check_access(
                principal,
                &AccessCheck {
                    contributor_id: contributor_id.clone(),
                    scope,
                },
            )? {
                return Err(Error::Forbidden);
            }
        }
        let configuration = self.workspace_configuration()?;
        Ok(RunnerAuthorization {
            contributor_id,
            session: session.value.clone(),
            configuration_revision: configuration.revision.clone(),
            agent_rules: configuration.agent_rules.clone(),
        })
    }

    fn runner_runtime(&self, principal: &Principal) -> Result<RuntimeBinding> {
        self.healthy()?;
        let (receipt, owner) = self.runtime_owner()?;
        let runtime = principal.runtime.as_ref().ok_or(Error::Forbidden)?;
        if principal.contributor != owner || runtime.host_id.0 != receipt.target.host_id {
            return Err(Error::Forbidden);
        }
        validation::id(&runtime.runtime_id.0).map_err(|_error| Error::Invalid)?;
        Ok(runtime.clone())
    }
}
