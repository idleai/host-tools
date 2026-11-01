//! Durable session requests, kept with the authority that checks their grants.

mod inputs;

use idle_protocol::v1::{
    ApiVersion,
    api::{Request, RuntimeSubmission},
    grants::{ComputePermission, GrantCommand, GrantScope},
    identity::{ContributorIdentity, ExternalIdentity, RequestContext, RequestKey, SessionId},
    sessions::RuntimeInputUpdate,
    standalone::{AccessCheck, ChangeNotice, Mutation},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Error, Result,
    runtime::sessions::{Create, Participant, RequestBody, Work},
};

use super::{Authority, Principal, validation};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Journal {
    creates: Vec<Creation>,
    inputs: Vec<Input>,
    #[serde(default)]
    grants: Vec<Request<GrantCommand>>,
}

impl Journal {
    pub(super) fn retains(&self, key: &RequestKey) -> bool {
        self.grants
            .iter()
            .any(|request| request.context.key() == *key)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Creation {
    request: Request<Create>,
    session_id: SessionId,
    ready: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Input {
    submission: RuntimeSubmission,
    update: RuntimeInputUpdate,
    claim: Option<String>,
    turn_id: Option<String>,
    participant: Participant,
}

impl Authority {
    pub(super) fn runner_owner(
        &self,
        client_id: &str,
        session_id: &SessionId,
    ) -> Result<idle_protocol::v1::identity::ContributorId> {
        if let Some(session) = self.state.sessions.get(&session_id.0) {
            if session.value.owner.0 != client_id {
                return Err(Error::Forbidden);
            }
            return Ok(session.value.owner.clone());
        }
        if let Some(creation) = self
            .state
            .session_journal
            .creates
            .iter()
            .find(|entry| entry.session_id == *session_id)
        {
            if creation.request.context.contributor.contributor_id.0 != client_id {
                return Err(Error::Forbidden);
            }
            return Ok(creation.request.context.contributor.contributor_id.clone());
        }
        if self.runtime_owner()?.1.contributor_id.0 != client_id {
            return Err(Error::Forbidden);
        }
        Ok(client_id.into())
    }

    pub(crate) fn session_call(
        &mut self,
        runtime: &Principal,
        client_id: &str,
        request: RequestBody,
        participant: Participant,
    ) -> Result<Value> {
        let _runtime = self.runner_runtime(runtime)?;
        let principal = self.session_principal(client_id)?;
        match request {
            RequestBody::Snapshot => self.session_snapshot(&principal),
            RequestBody::Create { request } => self.session_create(runtime, &principal, request),
            RequestBody::Submit { request } => {
                self.session_submit(runtime, &principal, request, participant)
            }
            RequestBody::Grant { request } => self.session_grant(&principal, request),
            RequestBody::RequestStatus { key } => self.session_status(&principal, &key),
            RequestBody::InputStatus { input } => {
                self.observe_runner(
                    runtime,
                    principal.contributor.contributor_id.clone(),
                    &input.session_id,
                )?;
                Ok(self
                    .state
                    .session_journal
                    .inputs
                    .iter()
                    .find(|entry| entry.update.input == input)
                    .map_or_else(
                        || json!({"kind":"unknown"}),
                        |entry| json!({"kind":"input","update":entry.update}),
                    ))
            }
        }
    }

    pub(super) fn session_principal(&self, client_id: &str) -> Result<Principal> {
        validation::id(client_id).map_err(|_error| Error::Invalid)?;
        let contributor_id = client_id.into();
        let _role = self
            .state
            .member(&contributor_id)
            .map_err(|_error| Error::Forbidden)?;
        Ok(Principal {
            contributor: ContributorIdentity {
                contributor_id,
                authenticated_as: ExternalIdentity {
                    issuer: "idle-runtime-grant".into(),
                    subject: client_id.into(),
                },
            },
            runtime: None,
        })
    }

    fn session_context(&self, principal: &Principal, context: &RequestContext) -> Result<()> {
        if context.workspace_id != self.state.workspace.value.id
            || context.contributor != principal.contributor
        {
            return Err(Error::Forbidden);
        }
        validation::id(&context.request_id.0).map_err(|_error| Error::Invalid)
    }

    fn session_deadline(&self, context: &RequestContext) -> Result<()> {
        let now = self.now()?;
        if context.expires_at.0 <= now
            || context.expires_at.0 > now.saturating_add(super::state::MAX_DEADLINE_MS)
        {
            return Err(Error::Expired);
        }
        Ok(())
    }

    fn session_snapshot(&self, principal: &Principal) -> Result<Value> {
        let snapshot = self.snapshot(principal)?;
        let inputs: Vec<_> = self.state.session_journal.inputs.iter().filter(|input| {
            snapshot.sessions.iter().any(|session| session.value.id == input.update.input.session_id)
        }).map(|input| json!({"update": input.update, "text": input.submission.body.text, "turn_id": input.turn_id})).collect();
        Ok(
            json!({"snapshot":snapshot,"inputs":inputs,"contributor":principal.contributor,"now_ms":self.now()?,"host_id":self.runtime_owner()?.0.target.host_id}),
        )
    }

    fn session_create(
        &mut self,
        runtime: &Principal,
        principal: &Principal,
        request: Request<Create>,
    ) -> Result<Value> {
        self.session_context(principal, &request.context)?;
        let binding = self.runner_runtime(runtime)?;
        if request.api_version != ApiVersion::V1
            || request.control_fence.is_some()
            || request.body.host_id != binding.host_id
            || (principal.contributor.contributor_id != runtime.contributor.contributor_id
                && !self.check_access(
                    runtime,
                    &AccessCheck {
                        contributor_id: principal.contributor.contributor_id.clone(),
                        scope: GrantScope::Compute {
                            host_id: binding.host_id,
                            permissions: vec![
                                ComputePermission::Connect,
                                ComputePermission::Execute,
                            ],
                        },
                    },
                )?)
        {
            return Err(Error::Forbidden);
        }
        let key = request.context.key();
        if let Some(previous) = self
            .state
            .session_journal
            .creates
            .iter()
            .find(|entry| entry.request.context.key() == key)
        {
            return if previous.request == request {
                Ok(self.creation_result(previous))
            } else {
                Err(Error::Conflict)
            };
        }
        self.unused_session_key(&key)?;
        self.session_deadline(&request.context)?;
        validation::label(&request.body.title).map_err(|_error| Error::Invalid)?;
        if self.state.session_journal.creates.len() >= 512 {
            return Err(Error::Busy);
        }
        let creation = Creation {
            request,
            session_id: uuid::Uuid::new_v4().to_string().as_str().into(),
            ready: false,
        };
        let mut next = self.state.clone();
        next.session_journal.creates.push(creation.clone());
        self.commit(next)?;
        Ok(self.creation_result(&creation))
    }

    fn creation_result(&self, creation: &Creation) -> Value {
        json!({"kind":"created","request":creation.request.context.key(),"session_id":creation.session_id,
            "ready":creation.ready,"session":self.state.sessions.get(&creation.session_id.0)})
    }

    fn unused_session_key(&self, key: &RequestKey) -> Result<()> {
        if self
            .state
            .session_journal
            .inputs
            .iter()
            .any(|entry| entry.submission.context.key() == *key)
            || self
                .state
                .session_journal
                .creates
                .iter()
                .any(|entry| entry.request.context.key() == *key)
            || self
                .state
                .receipts
                .iter()
                .any(|entry| entry.request.context.key() == *key)
            || self.state.session_journal.retains(key)
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    fn session_grant(
        &mut self,
        principal: &Principal,
        request: Request<GrantCommand>,
    ) -> Result<Value> {
        self.session_context(principal, &request.context)?;
        let scope = match &request.body {
            GrantCommand::Issue { scope, .. } => Some(scope),
            GrantCommand::Revoke { grant_id, .. } => self
                .state
                .grants
                .get(&grant_id.0)
                .map(|grant| &grant.value.scope),
        };
        if !matches!(scope, Some(GrantScope::Session { .. })) {
            return Err(Error::Forbidden);
        }
        if let Some(previous) = self
            .state
            .session_journal
            .grants
            .iter()
            .find(|entry| entry.context.key() == request.context.key())
        {
            if previous != &request {
                return Err(Error::Conflict);
            }
        } else {
            self.unused_session_key(&request.context.key())?;
            self.session_deadline(&request.context)?;
            if self.state.session_journal.grants.len() >= 512 {
                return Err(Error::Busy);
            }
            let mut next = self.state.clone();
            next.session_journal.grants.push(request.clone());
            self.commit(next)?;
        }
        let result = self.execute(
            principal,
            Request {
                api_version: request.api_version,
                context: request.context,
                control_fence: request.control_fence,
                body: Mutation::Grant(request.body),
            },
        )?;
        Ok(json!({"kind":"grant","response":result}))
    }

    fn session_status(&self, principal: &Principal, key: &RequestKey) -> Result<Value> {
        if key.workspace_id != self.state.workspace.value.id
            || key.contributor_id != principal.contributor.contributor_id
        {
            return Err(Error::Forbidden);
        }
        if let Some(creation) = self
            .state
            .session_journal
            .creates
            .iter()
            .find(|entry| entry.request.context.key() == *key)
        {
            return Ok(self.creation_result(creation));
        }
        if let Some(input) = self
            .state
            .session_journal
            .inputs
            .iter()
            .find(|entry| entry.submission.context.key() == *key)
        {
            return Ok(json!({"kind":"input","update":input.update}));
        }
        Ok(json!({"kind":"grant","response":self.request_status(principal,key)?}))
    }

    pub(crate) fn session_work(&mut self, runtime: &Principal, work: Work) -> Result<Value> {
        let _runtime = self.runner_runtime(runtime)?;
        match work {
            Work::DirectInput { session_id } => {
                let session = self
                    .state
                    .sessions
                    .get(&session_id.0)
                    .ok_or(Error::Invalid)?;
                let shared = self.state.session_journal.creates.iter().any(|entry| entry.session_id == session_id)
                    || self.state.session_journal.inputs.iter().any(|entry| entry.update.input.session_id == session_id)
                    || self.state.grants.values().any(|record| {
                        record.value.grantee != session.value.owner
                            && matches!(&record.value.scope, GrantScope::Session { session_id: id, .. } if *id == session_id)
                    });
                Ok(json!(!shared))
            }
            Work::Created { session_id } => self.session_created(&session_id),
            Work::Claim { session_id } => self.session_claim(runtime, &session_id),
            work @ (Work::Started { .. } | Work::Completed { .. } | Work::Release { .. }) => {
                self.session_progress(runtime, work)
            }
        }
    }

    fn session_created(&mut self, session_id: &SessionId) -> Result<Value> {
        let mut next = self.state.clone();
        let creation = next
            .session_journal
            .creates
            .iter_mut()
            .find(|entry| entry.session_id == *session_id)
            .ok_or(Error::Invalid)?;
        let session = next.sessions.get_mut(&session_id.0).ok_or(Error::Invalid)?;
        if session.value.owner != creation.request.context.contributor.contributor_id {
            return Err(Error::Forbidden);
        }
        if creation.ready {
            return Ok(self.creation_result(creation));
        }
        creation.ready = true;
        session.value.title.clone_from(&creation.request.body.title);
        session.revision.0 = session.revision.0.checked_add(1).ok_or(Error::Invalid)?;
        let saved = creation.clone();
        next.record_change(ChangeNotice::Directory)?;
        self.commit(next)?;
        Ok(self.creation_result(&saved))
    }
}
