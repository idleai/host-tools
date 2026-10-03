//! Session directory and runtime-authored input acceptance, order and completion.

use serde::{Deserialize, Serialize};

use super::{
    api::ApiError,
    identity::{
        ContributorId, ContributorIdentity, HostId, InputOrder, InputRevision, RequestKey,
        RuntimeId, SessionId, Timestamp,
    },
};

/// Current routing binding; moving the runtime does not create a new session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RuntimeBinding {
    /// Host executing the session.
    pub host_id: HostId,
    /// Runtime authorized to report this session's input state.
    pub runtime_id: RuntimeId,
}

/// Session purpose, independent of ownership and individual contributors.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// Ordinary human-directed or autonomous runner.
    Runner,
    /// Ambient Control session; execution also requires current ownership fencing.
    Control,
}

/// Directory metadata; registration is not runtime creation or execution success.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Session {
    /// Reconnectable identity, preserved across coordination adoption.
    pub id: SessionId,
    /// Owner who may share the session, never substituted for an input author.
    pub owner: ContributorId,
    /// Display label.
    pub title: String,
    /// Runner or Control purpose.
    pub kind: SessionKind,
    /// Authorized runtime and host binding.
    pub runtime: RuntimeBinding,
    /// Optional parent session, preserving subagent relationships.
    pub parent: Option<SessionId>,
}

/// Attributed text input. Identity and retry key live in the request envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SubmitInput {
    /// Target existing session in the request's workspace.
    pub session_id: SessionId,
    /// Exact submitted text, preserved across forwarding and retries.
    pub text: String,
}

/// Stable lookup for an input across retries, reconnects and runtime relocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InputRef {
    /// Directory session to which the input was submitted.
    pub session_id: SessionId,
    /// Original contributor/workspace/request identity.
    pub request: RequestKey,
}

/// Runtime-assigned delivery facts; a backend receipt has none of these fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Delivery {
    /// Time Evo durably accepted the input.
    pub accepted_at: Timestamp,
    /// Session-wide total input order, stable on replay and across restarts.
    pub order: InputOrder,
    /// Time Evo durably assigned the order.
    pub ordered_at: Timestamp,
}

/// Terminal runtime result; completion is not implied by receipt or ordering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Completion {
    /// Runtime confirmed successful processing.
    Succeeded,
    /// Runtime confirmed failed processing, possibly after partial execution.
    Failed(ApiError),
    /// Runtime confirmed cancellation, possibly after partial execution.
    Cancelled,
}

/// Runtime-authored input lifecycle, never inferred from transport delivery.
///
/// Accepted input may later be ordered, run and complete. Rejection happens before
/// acceptance. Observers may miss intermediate states and use `InputRevision` to
/// discard delayed updates; terminal states cannot revert. Order is not a promise
/// of execution success or of completion order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeInputState {
    /// Evo declined the input; no delivery order has been assigned.
    Rejected {
        /// Runtime rejection time.
        rejected_at: Timestamp,
        /// Explicit rejection reason, including revoked access or stale fencing.
        error: ApiError,
    },
    /// Evo durably accepted the attributed input but has not reported order.
    Accepted {
        /// Runtime acceptance time.
        accepted_at: Timestamp,
    },
    /// Evo durably assigned a unique position among this session's inputs.
    Ordered {
        /// Runtime-authored acceptance/order facts.
        delivery: Delivery,
    },
    /// Evo confirmed processing has started.
    Running {
        /// Original, unchanged delivery facts.
        delivery: Delivery,
        /// Runtime start time.
        started_at: Timestamp,
    },
    /// Evo confirmed a terminal outcome, including failure or cancellation.
    Completed {
        /// Original, unchanged delivery facts.
        delivery: Delivery,
        /// Runtime completion time.
        completed_at: Timestamp,
        /// Explicit terminal result.
        outcome: Completion,
    },
}

/// Recoverable runtime fact. Relays preserve attribution, revision and order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RuntimeInputUpdate {
    /// Original input identity.
    pub input: InputRef,
    /// Verified original contributor, matching `input.request.contributor_id`.
    pub contributor: ContributorIdentity,
    /// Authenticated runtime producing this fact, matching the session binding.
    pub runtime_id: RuntimeId,
    /// Monotonic revision for this input, independent of event cursor positions.
    pub revision: InputRevision,
    /// Full confirmed input state at this revision.
    pub state: RuntimeInputState,
}
