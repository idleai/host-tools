//! Workspace Control leases, ownership epochs and validation for execution/history writes.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::identity::{ControlEpoch, HostId, RuntimeId, SessionId, Timestamp, WorkspaceId};

/// Exact runtime authorized to act as the workspace's Control runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ControlHolder {
    /// Control runtime identity.
    pub runtime_id: RuntimeId,
    /// Host running Control.
    pub host_id: HostId,
    /// Control session identity.
    pub session_id: SessionId,
}

/// Fencing identity carried on every Control action and Control state write.
///
/// Not a bearer credential. Authorities must authenticate the holder and validate
/// current ownership at execution/append time, including on open connections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ControlFence {
    /// Exact workspace being controlled.
    pub workspace_id: WorkspaceId,
    /// Current ownership epoch, greater than zero.
    pub epoch: ControlEpoch,
    /// Exact owner of this epoch.
    pub holder: ControlHolder,
}

/// Authority-issued lease; renewal retains its epoch, reacquisition increases it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ControlLease {
    /// Ownership identity used for fencing.
    pub fence: ControlFence,
    /// Authority-clock initial acquisition time.
    pub acquired_at: Timestamp,
    /// Authority-clock exclusive validity boundary; expiry alone grants no successor.
    pub expires_at: Timestamp,
}

/// Durable ownership state, retaining the high watermark even while unassigned.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ControlOwnership {
    /// Workspace scope of both the watermark and any lease.
    pub workspace_id: WorkspaceId,
    /// Never decreases or resets on release, restart or coordination adoption.
    pub last_epoch: ControlEpoch,
    /// At most one current lease, whose epoch equals `last_epoch`.
    pub lease: Option<ControlLease>,
}

/// Atomic ownership mutations; all are retried with the original request key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Acquire only if no unexpired lease exists and the watermark still matches.
    Acquire {
        /// Proposed authenticated runtime holder.
        holder: ControlHolder,
        /// Last observed epoch; a successful acquisition assigns a strictly greater one.
        expected_epoch: ControlEpoch,
        /// Requested positive lease duration, bounded by provider policy.
        lease_duration_ms: NonZeroU32,
    },
    /// Extend the current unexpired lease without changing its epoch.
    Renew {
        /// Exact current ownership; stale holders cannot renew.
        fence: ControlFence,
        /// Requested positive extension, bounded by provider policy.
        lease_duration_ms: NonZeroU32,
    },
    /// Release only the matching current owner; retain the epoch high watermark.
    Release {
        /// Ownership being released.
        fence: ControlFence,
    },
}

/// Authority-clock validation result; advisory until rechecked at the write/action boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlValidation {
    /// Holder, workspace, epoch and expiry matched at validation time.
    Current {
        /// Validated lease, including its exclusive expiry.
        lease: ControlLease,
        /// Authority time at which validation was performed.
        checked_at: Timestamp,
    },
    /// Missing, expired or superseded ownership; no execution/write is authorized.
    Stale {
        /// Current ownership state for recovery.
        ownership: ControlOwnership,
        /// Authority time at which validation was performed.
        checked_at: Timestamp,
    },
}
