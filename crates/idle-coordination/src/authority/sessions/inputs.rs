use idle_protocol::v1::{
    ApiVersion,
    api::{ErrorCode, RuntimeSubmission},
    identity::{InputOrder, InputRevision, SessionId, Timestamp},
    sessions::{Completion, Delivery, InputRef, RuntimeInputState, RuntimeInputUpdate},
    standalone::ChangeNotice,
};
use serde_json::{Value, json};

use crate::{
    Error, Result,
    runtime::sessions::{Participant, Work},
};

use super::{Authority, Input, Principal};

impl Authority {
    pub(super) fn session_submit(
        &mut self,
        runtime: &Principal,
        principal: &Principal,
        submission: RuntimeSubmission,
        participant: Participant,
    ) -> Result<Value> {
        self.session_context(principal, &submission.context)?;
        let key = submission.context.key();
        let session_id = submission.body.session_id.clone();
        let _allowed = self.authorize_runner(
            runtime,
            principal.contributor.contributor_id.clone(),
            &session_id,
            participant.write,
        )?;
        if submission.api_version != ApiVersion::V1
            || submission.control_fence.is_some()
            || submission.coordination_receipt.is_some()
        {
            return Err(Error::Forbidden);
        }
        if let Some(previous) = self
            .state
            .session_journal
            .inputs
            .iter()
            .find(|entry| entry.submission.context.key() == key)
        {
            return if previous.submission == submission {
                Ok(json!({"kind":"input","update":previous.update}))
            } else {
                Err(Error::Conflict)
            };
        }
        self.unused_session_key(&key)?;
        self.session_deadline(&submission.context)?;
        super::super::validation::id(&participant.grant_id).map_err(|_error| Error::Invalid)?;
        if submission.body.text.trim().is_empty() || submission.body.text.len() > 32 * 1024 {
            return Err(Error::Invalid);
        }
        if self.state.session_journal.inputs.len() >= 512 {
            return Err(Error::Busy);
        }
        let order = self
            .state
            .session_journal
            .inputs
            .iter()
            .filter(|entry| entry.update.input.session_id == session_id)
            .filter_map(|entry| delivery(&entry.update.state).map(|value| value.order.0))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        let now = Timestamp(self.now()?);
        let update = RuntimeInputUpdate {
            input: InputRef {
                session_id,
                request: key,
            },
            contributor: principal.contributor.clone(),
            runtime_id: self.runner_runtime(runtime)?.runtime_id,
            revision: InputRevision(1),
            state: RuntimeInputState::Ordered {
                delivery: Delivery {
                    accepted_at: now,
                    order: InputOrder(order),
                    ordered_at: now,
                },
            },
        };
        let mut next = self.state.clone();
        next.session_journal.inputs.push(Input {
            submission,
            update: update.clone(),
            claim: None,
            turn_id: None,
            participant,
        });
        // Retain complete prompts while reserving room for running and terminal
        // states. Refuse new work before accepted inputs outgrow snapshot frames.
        let reserved = serde_json::to_vec(&next.session_journal.inputs)?
            .len()
            .saturating_add(next.session_journal.inputs.len().saturating_mul(1024));
        if reserved > 192 * 1024 {
            return Err(Error::Busy);
        }
        next.record_change(ChangeNotice::Directory)?;
        self.commit(next)?;
        Ok(json!({"kind":"input","update":update}))
    }

    pub(super) fn session_claim(
        &mut self,
        runtime: &Principal,
        session_id: &SessionId,
    ) -> Result<Value> {
        let generation = self.runner_runtime(runtime)?.runtime_id;
        let mut next = self.state.clone();
        let now = Timestamp(self.now()?);
        let mut changed = false;
        for input in next
            .session_journal
            .inputs
            .iter_mut()
            .filter(|entry| entry.update.input.session_id == *session_id)
        {
            if terminal(&input.update.state) {
                continue;
            }
            if input
                .claim
                .as_ref()
                .is_some_and(|claim| *claim != generation.0)
            {
                finish(
                    input,
                    now,
                    Completion::Failed(super::super::validation::failure(ErrorCode::Unavailable)),
                )?;
                input.update.runtime_id = generation.clone();
                changed = true;
            }
        }
        if changed {
            next.record_change(ChangeNotice::Directory)?;
            self.commit(next)?;
        }
        if self.state.session_journal.inputs.iter().any(|entry| {
            entry.update.input.session_id == *session_id
                && entry.claim.is_some()
                && !terminal(&entry.update.state)
                && !matches!(entry.update.state, RuntimeInputState::Ordered { .. })
        }) {
            return Ok(Value::Null);
        }
        let position = self.state.session_journal.inputs.iter().position(|entry| {
            entry.update.input.session_id == *session_id && !terminal(&entry.update.state)
        });
        let Some(position) = position else {
            return Ok(Value::Null);
        };
        let author = self
            .state
            .session_journal
            .inputs
            .get(position)
            .ok_or(Error::Invalid)?
            .submission
            .context
            .contributor
            .contributor_id
            .clone();
        let write = self
            .state
            .session_journal
            .inputs
            .get(position)
            .ok_or(Error::Invalid)?
            .participant
            .write;
        let allowed = self
            .authorize_runner(runtime, author, session_id, write)
            .is_ok();
        let mut next = self.state.clone();
        let input = next
            .session_journal
            .inputs
            .get_mut(position)
            .ok_or(Error::Invalid)?;
        if allowed {
            input.claim = Some(generation.0);
        } else {
            finish(
                input,
                now,
                Completion::Failed(super::super::validation::failure(ErrorCode::Forbidden)),
            )?;
            input.update.runtime_id = generation;
        }
        let result = if allowed {
            claim_result(input)
        } else {
            Value::Null
        };
        next.record_change(ChangeNotice::Directory)?;
        self.commit(next)?;
        Ok(result)
    }

    pub(super) fn session_progress(&mut self, runtime: &Principal, work: Work) -> Result<Value> {
        let generation = self.runner_runtime(runtime)?.runtime_id;
        let reference = match &work {
            Work::Started { input, .. }
            | Work::Completed { input, .. }
            | Work::Release { input } => input,
            Work::DirectInput { .. } | Work::Created { .. } | Work::Claim { .. } => {
                return Err(Error::Invalid);
            }
        };
        let mut next = self.state.clone();
        let input = next
            .session_journal
            .inputs
            .iter_mut()
            .find(|entry| entry.update.input == *reference)
            .ok_or(Error::Invalid)?;
        if matches!(work, Work::Release { .. })
            && input.claim.is_none()
            && matches!(input.update.state, RuntimeInputState::Ordered { .. })
        {
            return Ok(json!(input.update));
        }
        if input.claim.as_deref() != Some(&generation.0) {
            return Err(Error::Forbidden);
        }
        if terminal(&input.update.state) {
            return Ok(json!(input.update));
        }
        let now = Timestamp(self.now()?);
        match work {
            Work::Started { turn_id, .. } => {
                super::super::validation::id(&turn_id).map_err(|_error| Error::Invalid)?;
                if let Some(previous) = &input.turn_id {
                    return if previous == &turn_id {
                        Ok(json!(input.update))
                    } else {
                        Err(Error::Conflict)
                    };
                }
                let delivery = delivery(&input.update.state).ok_or(Error::Invalid)?.clone();
                input.turn_id = Some(turn_id);
                input.update.runtime_id = generation;
                input.update.state = RuntimeInputState::Running {
                    delivery,
                    started_at: now,
                };
                input.update.revision.0 = input
                    .update
                    .revision
                    .0
                    .checked_add(1)
                    .ok_or(Error::Invalid)?;
            }
            Work::Completed { outcome, .. } => {
                finish(input, now, outcome)?;
                input.update.runtime_id = generation;
            }
            Work::Release { .. } => {
                if !matches!(input.update.state, RuntimeInputState::Ordered { .. }) {
                    return Err(Error::Conflict);
                }
                input.claim = None;
            }
            Work::DirectInput { .. } | Work::Created { .. } | Work::Claim { .. } => {
                return Err(Error::Invalid);
            }
        }
        let result = json!(input.update);
        next.record_change(ChangeNotice::Directory)?;
        self.commit(next)?;
        Ok(result)
    }
}

fn claim_result(input: &Input) -> Value {
    json!({"input":input.update.input,"text":input.submission.body.text,"contributor_id":input.submission.context.contributor.contributor_id,"grant_id":input.participant.grant_id})
}

fn terminal(state: &RuntimeInputState) -> bool {
    matches!(
        state,
        RuntimeInputState::Rejected { .. } | RuntimeInputState::Completed { .. }
    )
}

fn delivery(state: &RuntimeInputState) -> Option<&Delivery> {
    match state {
        RuntimeInputState::Ordered { delivery }
        | RuntimeInputState::Running { delivery, .. }
        | RuntimeInputState::Completed { delivery, .. } => Some(delivery),
        RuntimeInputState::Rejected { .. } | RuntimeInputState::Accepted { .. } => None,
    }
}

fn finish(input: &mut Input, now: Timestamp, outcome: Completion) -> Result<()> {
    let delivery = delivery(&input.update.state).ok_or(Error::Invalid)?.clone();
    input.update.state = RuntimeInputState::Completed {
        delivery,
        completed_at: now,
        outcome,
    };
    input.update.revision.0 = input
        .update
        .revision
        .0
        .checked_add(1)
        .ok_or(Error::Invalid)?;
    Ok(())
}
