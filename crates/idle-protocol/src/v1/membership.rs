//! Workspace membership and invitation lifecycle, separate from resource grants.

use serde::{Deserialize, Serialize};

use super::identity::{ContributorId, ExternalIdentity, InvitationId, Revision, Timestamp};

/// Workspace metadata role; no role implicitly grants compute or model use.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Workspace owner, subject to the provider's ownership policy.
    Owner,
    /// Membership and configuration administrator.
    Admin,
    /// Workspace participant.
    Member,
    /// Read-only workspace observer.
    Viewer,
}

/// Membership status; revocation overrides all grants in this workspace.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MembershipStatus {
    /// Current member, subject to resource-specific grants.
    Active,
    /// Removed member, including on already-open connections.
    Revoked,
}

/// Membership in the enclosing workspace only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Membership {
    /// Member identity, not a host or session identity.
    pub contributor_id: ContributorId,
    /// Workspace metadata role.
    pub role: Role,
    /// Current membership state.
    pub status: MembershipStatus,
}

/// Invitation recipient, including someone not yet in the contributor directory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Invitee {
    /// An existing contributor.
    Contributor(ContributorId),
    /// A verified immutable external subject; no email or login-name matching.
    External(ExternalIdentity),
}

/// Durable invitation state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvitationStatus {
    /// Awaiting authenticated acceptance.
    Pending,
    /// Accepted by the authenticated matching recipient.
    Accepted {
        /// Resolved contributor granted membership.
        contributor_id: ContributorId,
    },
    /// Withdrawn by an authorized member.
    Revoked,
    /// Deadline elapsed before acceptance.
    Expired,
}

/// Invitation metadata; accepting it does not create compute/session grants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Invitation {
    /// Stable invitation identity.
    pub id: InvitationId,
    /// Actual authenticated inviter.
    pub invited_by: ContributorId,
    /// Intended recipient.
    pub invitee: Invitee,
    /// Proposed metadata role.
    pub role: Role,
    /// Authority-clock expiry.
    pub expires_at: Timestamp,
    /// Current state.
    pub status: InvitationStatus,
}

/// Authorized membership mutations, correlated by the enclosing request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MembershipCommand {
    /// Create an invitation; inviter comes from the authenticated request.
    Invite {
        /// New invitation identity.
        invitation_id: InvitationId,
        /// Intended recipient.
        invitee: Invitee,
        /// Proposed role.
        role: Role,
        /// Deadline for acceptance.
        expires_at: Timestamp,
    },
    /// Accept as the authenticated matching recipient.
    AcceptInvitation {
        /// Invitation to accept.
        invitation_id: InvitationId,
        /// Required invitation revision.
        expected_revision: Revision,
    },
    /// Withdraw a pending invitation.
    RevokeInvitation {
        /// Invitation to revoke.
        invitation_id: InvitationId,
        /// Required invitation revision.
        expected_revision: Revision,
    },
    /// Change a current member's metadata role.
    ChangeRole {
        /// Member to change.
        contributor_id: ContributorId,
        /// New role.
        role: Role,
        /// Required membership revision.
        expected_revision: Revision,
    },
    /// Revoke membership and invalidate workspace access on active connections.
    RevokeMember {
        /// Member to revoke.
        contributor_id: ContributorId,
        /// Required membership revision.
        expected_revision: Revision,
    },
}
