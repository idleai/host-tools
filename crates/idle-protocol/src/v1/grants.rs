//! Explicit, independently revocable session, compute and provider permissions.

use serde::{Deserialize, Serialize};

use super::identity::{ContributorId, GrantId, HostId, ProviderId, Revision, SessionId, Timestamp};

/// Permission within an existing session; none permit host file/process access.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionPermission {
    /// Observe the authorized session's runtime history and state.
    Observe,
    /// Submit attributed input to the runtime.
    SubmitInput,
    /// Manage session participation, subject to workspace policy.
    Invite,
}

/// Host permission checked by Evo at the corresponding execution boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ComputePermission {
    /// Establish a host connection without permission to execute operations.
    Connect,
    /// Read files within the runtime's permitted sandbox.
    ReadFiles,
    /// Write files within the runtime's permitted sandbox.
    WriteFiles,
    /// Start or control permitted processes.
    Execute,
    /// Install and serve local models on the host.
    ManageModels,
}

/// Model-provider access, independent of provider credentials and SSO.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProviderPermission {
    /// Use authorized models exposed by this provider.
    UseModels,
    /// Manage provider publication/configuration metadata.
    Manage,
}

/// A grant can address exactly one resource kind within the enclosing workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantScope {
    /// Session participation does not imply a compute or provider grant.
    Session {
        /// Session being shared.
        session_id: SessionId,
        /// Explicit permitted session actions.
        permissions: Vec<SessionPermission>,
    },
    /// Host access does not imply membership in a session.
    Compute {
        /// Host being shared.
        host_id: HostId,
        /// Explicit permitted host actions.
        permissions: Vec<ComputePermission>,
    },
    /// Provider access does not imply permission to administer its host.
    Provider {
        /// Provider being shared.
        provider_id: ProviderId,
        /// Explicit permitted provider actions.
        permissions: Vec<ProviderPermission>,
    },
}

/// Grant state; expiry is checked even if no expiry event has arrived.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantStatus {
    /// Issued, subject to expiry, current membership and current runtime policy.
    Active,
    /// Irrevocably withdrawn; issue a new grant identity to restore access.
    Revoked {
        /// Authority-confirmed revocation time.
        revoked_at: Timestamp,
        /// Authenticated revoking contributor.
        revoked_by: ContributorId,
    },
}

/// Authorization metadata, never a bearer credential or an execution decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Grant {
    /// Stable, non-reusable grant identity.
    pub id: GrantId,
    /// Contributor receiving the permission.
    pub grantee: ContributorId,
    /// Authenticated issuing contributor.
    pub granted_by: ContributorId,
    /// Resource and explicit permissions.
    pub scope: GrantScope,
    /// Optional authority-clock expiry.
    pub expires_at: Option<Timestamp>,
    /// Current revocation state.
    pub status: GrantStatus,
}

/// Sharing mutations, independent from workspace membership and runtime input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantCommand {
    /// Issue a new explicit grant after verifying issuer authority.
    Issue {
        /// New, never previously used grant identity.
        grant_id: GrantId,
        /// Intended recipient.
        grantee: ContributorId,
        /// Resource and permissions to share.
        scope: GrantScope,
        /// Optional grant deadline.
        expires_at: Option<Timestamp>,
    },
    /// Revoke a grant, including its use on already-open connections.
    Revoke {
        /// Grant to revoke.
        grant_id: GrantId,
        /// Required current grant revision.
        expected_revision: Revision,
    },
}
