//! Versioned requests, responses and transport-independent failure semantics.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::{
    ApiVersion, Change,
    control::{ControlCommand, ControlFence, ControlOwnership, ControlValidation},
    events::{Event, Recovery, RecoveryCursor, RecoverySnapshot},
    grants::GrantCommand,
    identity::{RequestContext, RequestKey, Timestamp, WorkspaceId},
    membership::MembershipCommand,
    resources::ResourceCommand,
    sessions::{InputRef, RuntimeInputUpdate, Session, SubmitInput},
    workspace::Workspace,
};

/// Request envelope shared by standalone adapters, managed adapters and Evo.
///
/// Mutations with the same key and semantic content replay the original durable
/// result. Changed content under that key is an idempotency conflict. Queries use
/// the key for correlation but may observe newer state when repeated.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Request<T> {
    /// Required supported wire version.
    pub api_version: ApiVersion,
    /// Authenticated attribution, retry identity and first-receipt deadline.
    pub context: RequestContext,
    /// Required for side effects originating from Control, including state writes.
    /// This is validated against current ownership, not merely deserialized.
    pub control_fence: Option<ControlFence>,
    /// Typed operation payload.
    pub body: T,
}

/// Response envelope; correlation never implies that execution succeeded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Response<T> {
    /// Required supported wire version.
    pub api_version: ApiVersion,
    /// Original workspace/contributor/request key.
    pub request: RequestKey,
    /// Explicit success payload or protocol failure.
    pub result: ApiResult<T>,
}

/// Stable tagged result representation, independent of HTTP status codes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum ApiResult<T> {
    /// Operation-specific result; inspect its acknowledgement stage.
    Success(T),
    /// Explicit failure; inspect retry advice rather than changing request IDs.
    Failure(ApiError),
}

/// Machine-readable error reason. Unknown codes require protocol negotiation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Invalid payload, identity binding or domain invariant.
    InvalidRequest,
    /// Unsupported wire version.
    UnsupportedVersion,
    /// Operation unavailable from this provider/runtime.
    UnsupportedOperation,
    /// Missing or invalid authenticated connection.
    Unauthenticated,
    /// Current membership, grant or policy forbids this action.
    Forbidden,
    /// Resource is absent or not visible in the requested workspace.
    NotFound,
    /// Current entity state conflicts with the proposed action.
    Conflict,
    /// Optimistic metadata precondition failed.
    StaleRevision,
    /// Fencing owner/epoch/expiry no longer authorizes this action.
    StaleControl,
    /// Same deduplication key was used for different semantic content.
    IdempotencyConflict,
    /// First-receipt deadline elapsed or the old result is no longer retained.
    RequestExpired,
    /// Cursor belongs to a different workspace/audience or is ahead of the stream.
    CursorScopeMismatch,
    /// Temporary provider or runtime unavailability.
    Unavailable,
    /// Bounded capacity requires waiting before retry.
    RateLimited,
}

/// How to recover from failure without creating a second logical mutation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryAdvice {
    /// This request must not be resubmitted automatically.
    Never,
    /// Retry unchanged, retaining the original key and deadline.
    SameRequest {
        /// Earliest authority-clock retry time, if supplied.
        not_before: Option<Timestamp>,
    },
    /// Outcome is uncertain; reconcile by the original key before taking action.
    QueryStatus,
}

/// Serializable failure with no provider SDK types or secret diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiError {
    /// Stable machine-readable classification.
    pub code: ErrorCode,
    /// Safe human-readable context, not an authorization decision.
    pub message: String,
    /// Explicit retry/reconciliation advice.
    pub retry: RetryAdvice,
}

/// Coordination operations, with runtime input routing explicitly identified.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Command {
    /// Create/update workspace metadata, including repository attachments or adoption.
    /// The chain and existing identities are immutable across updates.
    PutWorkspace(Change<Workspace>),
    /// Invite, update or revoke workspace members.
    Membership(MembershipCommand),
    /// Register/update directory metadata; never a claim that a runtime started.
    PutSession(Change<Session>),
    /// Publish or detach workspace resource bindings.
    Resource(ResourceCommand),
    /// Issue or revoke one independently scoped resource grant.
    Grant(GrantCommand),
    /// Acquire, renew or release Control ownership.
    Control(ControlCommand),
    /// Route unchanged attributed input to Evo; coordination returns receipt only.
    SubmitInput(SubmitInput),
}

/// Durable coordination receipt, not Evo acceptance, order or completion.
///
/// Issued only after the request and its forwarding/retry record are durable.
/// `received_at` is the original receipt time even when this receipt is replayed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BackendReceipt {
    /// Original contributor and retry identity.
    pub request: RequestKey,
    /// Time the coordination provider durably recorded the submission.
    pub received_at: Timestamp,
    /// Inclusive minimum retention of the original receipt/result; at least the
    /// request's first-receipt deadline. Later unknown retries fail closed.
    pub retry_until: Timestamp,
}

/// Metadata committed atomically with its retained events and deduplication result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommitReceipt {
    /// Original mutation identity and receipt retention guarantee.
    pub receipt: BackendReceipt,
    /// Authority-clock commit time.
    pub committed_at: Timestamp,
    /// Boundary containing the mutation's events. Recover through it before
    /// advancing a local cursor; this receipt alone cannot update the local view.
    pub through: RecoveryCursor,
}

/// Distinct acknowledgement stages; no variant infers runtime success.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CommandResult {
    /// Input was durably received for routing only.
    Received(BackendReceipt),
    /// Metadata mutation and its notification events were durably committed.
    Committed(CommitReceipt),
    /// Control mutation committed; the returned lease still needs live fencing.
    Control {
        /// Durable mutation receipt.
        commit: CommitReceipt,
        /// Resulting ownership state, including the epoch high watermark.
        ownership: ControlOwnership,
    },
}

/// Read/recovery operations; all nested scopes must match the request context.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Query {
    /// Obtain a consistent authorized metadata snapshot and recovery boundary.
    Snapshot,
    /// Recover durable events strictly after the supplied cursor.
    CatchUp {
        /// Workspace/audience/generation-bound exclusive recovery cursor.
        after: RecoveryCursor,
        /// Requested positive page size, bounded by provider policy.
        limit: NonZeroU32,
    },
    /// Resolve an earlier mutation without allocating a new retry key.
    RequestStatus(RequestKey),
    /// Get the latest runtime-confirmed input fact; unavailable hosts stay explicit.
    InputStatus(InputRef),
    /// Read current Control ownership and its durable epoch high watermark.
    ControlOwnership,
    /// Validate a holder using the authority's clock; writes must recheck atomically.
    ValidateControl(ControlFence),
}

/// Recoverable mutation result, without implying absence means safe re-execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RequestStatus {
    /// Retained original coordination result.
    Recorded(Box<CommandResult>),
    /// No retained result is available. This is not proof that execution never ran.
    Unknown,
}

/// Typed query results; command receipts and runtime facts remain distinct.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum QueryResult {
    /// Atomic directory state and the subsequent catch-up boundary.
    Snapshot(Box<RecoverySnapshot>),
    /// Ordered retained events or explicit snapshot requirement.
    CatchUp(Recovery),
    /// Recovered coordination acknowledgement for the original mutation.
    RequestStatus(RequestStatus),
    /// Latest authoritative runtime fact, which may also arrive through events.
    InputStatus(RuntimeInputUpdate),
    /// Current Control ownership.
    ControlOwnership(ControlOwnership),
    /// Authority-clock validation result.
    ControlValidation(ControlValidation),
}

/// Direct runtime report; relays may retain it as an `InputUpdated` event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RuntimeReport {
    /// Required wire version.
    pub api_version: ApiVersion,
    /// Exact workspace scope, matching the input request and session binding.
    pub workspace_id: WorkspaceId,
    /// Original producer time, preserved by relays.
    pub produced_at: Timestamp,
    /// Authenticated runtime fact; the transport authenticates its runtime ID.
    pub update: RuntimeInputUpdate,
}

/// Direct or forwarded runtime submission, retaining the original request context.
///
/// Forwarders extract the unchanged `SubmitInput` body from the coordination
/// command. Evo deduplicates its semantic input, context and fence using the same
/// request key. Recipients must authenticate the delegation channel before
/// trusting a forwarded receipt, and its key must match `context.key()`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RuntimeSubmission {
    /// Required wire version.
    pub api_version: ApiVersion,
    /// Original contributor, retry key and first-receipt deadline, unchanged.
    pub context: RequestContext,
    /// Original Control fencing, when applicable; revalidated by Evo.
    pub control_fence: Option<ControlFence>,
    /// Exact input originally submitted by this contributor.
    pub body: SubmitInput,
    /// Durable coordination receipt for forwarded input. Direct input uses `None`
    /// and applies the first-receipt deadline at Evo itself. Never a bearer token.
    pub coordination_receipt: Option<BackendReceipt>,
}

/// Schema root covering each top-level wire shape without an extra wire wrapper.
///
/// Endpoints should decode their concrete type, such as `Request<Command>` or
/// `Response<QueryResult>`. This union is for schema export and inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum WireMessage {
    /// Command endpoint request.
    CommandRequest(Request<Command>),
    /// Query endpoint request.
    QueryRequest(Request<Query>),
    /// Command endpoint response.
    CommandResponse(Response<CommandResult>),
    /// Query endpoint response.
    QueryResponse(Response<QueryResult>),
    /// Direct/forwarded attributed runtime submission.
    RuntimeSubmission(RuntimeSubmission),
    /// Direct runtime acknowledgement, distinct from a coordination receipt.
    RuntimeResponse(Response<RuntimeInputUpdate>),
    /// Direct runtime input acknowledgement/status.
    RuntimeReport(RuntimeReport),
    /// Retained coordination event.
    Event(Event),
}
