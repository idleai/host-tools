//! Public repository coordination data; no host or application implementation.

use serde::{Deserialize, Serialize};

use super::{
    Change, Record,
    api::{Request, Response},
    configuration::{ConfigurationDocument, ConfigurationValue, ConfigurationWrite},
    control::{ControlCommand, ControlOwnership},
    events::RecoveryCursor,
    grants::{Grant, GrantCommand, GrantScope},
    identity::{ContributorId, HostId, RepositoryId, Timestamp},
    membership::Membership,
    projections::ProjectionKind,
    resources::{ComputeHost, ModelProvider},
    sessions::Session,
    workspace::Workspace,
};

/// Saved view definition; derived results still come from history/controller APIs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ViewDefinition {
    /// Stable repository-scoped view identity.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Supplied projection destination.
    pub kind: ProjectionKind,
    /// Definition format version, currently one.
    pub schema_version: u32,
    /// Complete JSON object containing filters/layout; unknown fields survive.
    pub json: String,
}

/// Online status and active file/branch for an authenticated connection.
/// Timestamps express freshness, never order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Presence {
    /// Stable connection identity for replacement/disconnection.
    pub connection_id: String,
    /// Authenticated contributor; cannot be inferred from a host label.
    pub contributor_id: ContributorId,
    /// Repository in the selected workspace.
    pub repository_id: RepositoryId,
    /// Optional branch name.
    pub branch: Option<String>,
    /// Optional repository-relative file path.
    pub file: Option<String>,
    /// Host only when visible through current grants.
    pub host_id: Option<HostId>,
    /// Supplied work description.
    pub summary: Option<String>,
    /// Producer observation time, bounded by authority policy.
    pub observed_at: Timestamp,
    /// Exclusive freshness boundary, bounded by the authority.
    pub valid_until: Timestamp,
}

/// Repository mutations share the existing authenticated request envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Mutation {
    /// Update labels while retaining workspace/chain/repository identity.
    Workspace(Change<Workspace>),
    /// Independently conditional settings/rules replacement.
    Configuration(ConfigurationWrite),
    /// Store one versioned, rebuildable view definition.
    View(Change<ViewDefinition>),
    /// Register directory metadata without claiming runtime acceptance.
    Session(Change<Session>),
    /// Publish a host without implicitly granting compute access.
    Host(Change<ComputeHost>),
    /// Publish a model provider without distributing its credentials.
    Provider(Change<ModelProvider>),
    /// Owner-authorized membership update; revocations override all grants.
    Membership(Change<Membership>),
    /// Independently scoped sharing grants.
    Grant(GrantCommand),
    /// Durable, increasing controller ownership epochs.
    Control(ControlCommand),
}

/// Committed data returned for the original immutable request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MutationValue {
    /// Workspace record.
    Workspace(Record<Workspace>),
    /// Configuration record, matching the request's document scope.
    Configuration(Record<ConfigurationValue>),
    /// View definition record.
    View(Record<ViewDefinition>),
    /// Session directory record; no execution acknowledgement.
    Session(Record<Session>),
    /// Host directory record.
    Host(Record<ComputeHost>),
    /// Provider directory record.
    Provider(Record<ModelProvider>),
    /// Membership record.
    Membership(Record<Membership>),
    /// Grant record, including revocations.
    Grant(Record<Grant>),
    /// Current assignment and retained ownership watermark.
    Control(ControlOwnership),
}

/// Atomic mutation result; no variant implies Evo executed an operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MutationResult {
    /// Original authority-clock commit time.
    pub committed_at: Timestamp,
    /// Boundary through which the client must recover or replace its view.
    pub through: RecoveryCursor,
    /// Original committed record or ownership assignment.
    pub value: MutationValue,
}

/// Authenticated replacement snapshot with independent configuration revisions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RepositorySnapshot {
    /// Audience-bound recovery boundary.
    pub as_of: RecoveryCursor,
    /// Immutable workspace/chain binding and repository metadata.
    pub workspace: Record<Workspace>,
    /// Current and revoked metadata memberships.
    pub memberships: Vec<Record<Membership>>,
    /// Only sessions visible to this audience.
    pub sessions: Vec<Record<Session>>,
    /// Only hosts visible through ownership or grants.
    pub hosts: Vec<Record<ComputeHost>>,
    /// Only providers visible through ownership or grants.
    pub providers: Vec<Record<ModelProvider>>,
    /// Current and revoked grants visible to this audience.
    pub grants: Vec<Record<Grant>>,
    /// Controller ownership, always requiring live revalidation.
    pub control: ControlOwnership,
    /// Absent only when this document has never existed.
    pub settings: Option<Record<ConfigurationValue>>,
    /// Absent only when this document has never existed.
    pub agent_rules: Option<Record<ConfigurationValue>>,
    /// Saved projection/view definitions.
    pub views: Vec<Record<ViewDefinition>>,
}

/// Notification identifying which read to refresh; carries no private record data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ChangeNotice {
    /// Repository/workspace metadata changed.
    Workspace,
    /// A configuration document changed.
    Configuration(ConfigurationDocument),
    /// Saved view definitions changed.
    Views,
    /// Authorized session/resource discovery changed.
    Directory,
    /// Membership or grants changed; replace the authorized snapshot.
    Access,
    /// Controller assignment or renewal changed.
    Control,
}

/// One durable change notification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RepositoryEvent {
    /// Position in this audience's current visibility generation.
    pub cursor: RecoveryCursor,
    /// Read invalidated by this commit.
    pub notice: ChangeNotice,
}

/// Bounded recovery result. A reset always requires a fresh authorized snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RepositoryRecovery {
    /// Ordered notifications after the supplied exclusive cursor.
    Events {
        /// Strictly increasing changes in one visibility generation.
        events: Vec<RepositoryEvent>,
        /// Scanned boundary.
        through: RecoveryCursor,
        /// More changes remain.
        has_more: bool,
    },
    /// The generation changed or the retained history no longer covers the cursor.
    SnapshotRequired,
}

/// Authorization check shared with f15/f17's execution adapters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AccessCheck {
    /// Authenticated contributor requesting the operation.
    pub contributor_id: ContributorId,
    /// Exact resource and actions being checked, independently of other grants.
    pub scope: GrantScope,
}

/// Schema union for the additive standalone repository contracts. Native framing
/// identifies its endpoint before decoding these payloads, as with `api::WireMessage`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum RepositoryMessage {
    /// Authenticated, idempotent mutation.
    Request(Request<Mutation>),
    /// Original mutation outcome.
    Response(Response<MutationResult>),
    /// Authorized replacement snapshot.
    Snapshot(RepositorySnapshot),
    /// Ordered invalidations or an explicit reset.
    Recovery(RepositoryRecovery),
    /// Transient authenticated peer activity.
    Presence(Presence),
    /// Current independently scoped access check.
    Access(AccessCheck),
}

#[cfg(test)]
mod tests;
