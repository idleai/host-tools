//! Durable event delivery, scoped cursors and consistent snapshot recovery.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::{
    ApiVersion, Record,
    control::ControlOwnership,
    grants::Grant,
    identity::{
        Contributor, ContributorId, EventId, EventPosition, RequestKey, Revision, StreamId,
        Timestamp, WorkspaceId,
    },
    membership::{Invitation, Membership},
    resources::{ComputeHost, Model, ModelProvider, ResourceRef},
    sessions::{RuntimeInputUpdate, Session},
    workspace::Workspace,
};

/// Recovery position in a workspace, audience and stream generation.
///
/// Positions are ordered only inside the same scope. A provider change, retention
/// reset or visibility change may require a new stream generation and snapshot.
/// Timestamps are not cursors. Event positions may contain gaps after filtering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RecoveryCursor {
    /// Workspace whose retained events are addressed.
    pub workspace_id: WorkspaceId,
    /// Authenticated audience; a cursor is not transferable to another contributor.
    pub contributor_id: ContributorId,
    /// Authority-issued generation of this audience's visibility scope.
    pub stream_id: StreamId,
    /// Exclusive resume position: ask for events strictly after this position.
    pub position: EventPosition,
}

impl RecoveryCursor {
    /// Compare positions only when workspace, audience and generation all match.
    #[must_use]
    pub fn compare_position(&self, other: &Self) -> Option<Ordering> {
        if self.workspace_id == other.workspace_id
            && self.contributor_id == other.contributor_id
            && self.stream_id == other.stream_id
        {
            Some(self.position.cmp(&other.position))
        } else {
            None
        }
    }
}

/// Retained event body. Metadata revisions and runtime revisions have different owners.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventBody {
    /// Workspace metadata/bindings committed by the coordination authority.
    WorkspaceChanged(Record<Workspace>),
    /// Contributor identity links or display metadata changed.
    ContributorChanged(Record<Contributor>),
    /// Membership added, changed or revoked; revocation overrides resource grants.
    MembershipChanged(Record<Membership>),
    /// Invitation lifecycle advanced.
    InvitationChanged(Record<Invitation>),
    /// Session directory binding changed; this does not confirm runtime execution.
    SessionChanged(Record<Session>),
    /// Compute publication/health changed in this workspace.
    HostChanged(Record<ComputeHost>),
    /// Provider publication/health changed in this workspace.
    ProviderChanged(Record<ModelProvider>),
    /// Model publication/health changed in this workspace.
    ModelChanged(Record<Model>),
    /// Binding removed from this workspace, without deleting the resource globally.
    ResourceDetached {
        /// Removed binding identity.
        resource: ResourceRef,
        /// Tombstone revision preventing older publications from reappearing.
        revision: Revision,
    },
    /// Grant issued or revoked.
    GrantChanged(Record<Grant>),
    /// Durable Control assignment/renewal/release changed.
    ControlChanged(ControlOwnership),
    /// Runtime-authenticated fact, preserved unchanged by coordination relays.
    InputUpdated(RuntimeInputUpdate),
}

/// Versioned retained event; live delivery may duplicate, delay or reorder it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Event {
    /// Required wire version.
    pub api_version: ApiVersion,
    /// Stable event identity retained on replay.
    pub event_id: EventId,
    /// Scope and durable order assigned by coordination, not runtime input order.
    pub cursor: RecoveryCursor,
    /// Original producer time, retained even for late delivery/import.
    pub produced_at: Timestamp,
    /// Time the coordination authority durably recorded the event.
    pub recorded_at: Timestamp,
    /// Originating mutation when known; absent for unsolicited health/state changes.
    pub causation: Option<RequestKey>,
    /// Full entity state or explicit removal at this position.
    pub body: EventBody,
}

/// Atomic authorized directory view at an exclusive catch-up boundary.
///
/// Every listed metadata value reflects `as_of`. Runtime states are the last
/// confirmed facts recorded by that boundary, not proof a detached host stopped.
/// A snapshot replaces the prior scoped view; absent entities are removed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RecoverySnapshot {
    /// Resume retained delivery strictly after this consistent boundary.
    pub as_of: RecoveryCursor,
    /// Exactly one logical workspace/chain binding.
    pub workspace: Record<Workspace>,
    /// Authorized contributor directory.
    pub contributors: Vec<Record<Contributor>>,
    /// Visible current and revoked memberships.
    pub memberships: Vec<Record<Membership>>,
    /// Visible invitations.
    pub invitations: Vec<Record<Invitation>>,
    /// Reconnectable session directory.
    pub sessions: Vec<Record<Session>>,
    /// Workspace host bindings.
    pub hosts: Vec<Record<ComputeHost>>,
    /// Workspace provider bindings.
    pub providers: Vec<Record<ModelProvider>>,
    /// Published models available for authorized discovery.
    pub models: Vec<Record<Model>>,
    /// Current grant records, including revocations.
    pub grants: Vec<Record<Grant>>,
    /// Durable Control ownership and epoch high watermark.
    pub control: ControlOwnership,
    /// Last retained runtime-confirmed input states visible to this audience.
    pub inputs: Vec<RuntimeInputUpdate>,
}

/// Ordered, bounded catch-up response; delivery is at least once, not exactly once.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventPage {
    /// Original exclusive cursor, echoed for reconciliation.
    pub after: RecoveryCursor,
    /// Strictly increasing positions after `after`, all in its scope.
    pub events: Vec<Event>,
    /// Scanned-through boundary, possibly beyond visible events after filtering.
    pub through: RecoveryCursor,
    /// Another page was available when this page was read.
    pub has_more: bool,
}

/// Why the old cursor can no longer safely recover the current view.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    /// Retained events before the cursor are no longer available.
    RetentionExpired,
    /// Provider/stream generation changed, including managed adoption.
    StreamChanged,
    /// Current grants require a new authorized view and subscription scope.
    VisibilityChanged,
}

/// Catch-up outcome; unavailable history is never represented as an empty success.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Recovery {
    /// Retained ordered events, possibly an empty current page.
    Events(EventPage),
    /// Fetch a fresh snapshot before replacing the old view/cursor.
    SnapshotRequired(ResetReason),
}
