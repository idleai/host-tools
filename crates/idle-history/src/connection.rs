//! Shared join, recovery and connection status, independent of host transports.

use serde::{Deserialize, Serialize};

/// A connection is ready only after its replacement read or inventory check.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[repr(u8)]
pub enum ConnectionStatus {
    /// No active join or retry.
    #[default]
    Stopped,
    /// Opening an authorized transport.
    Connecting,
    /// Transport is open; the remote identity has not been accepted yet.
    Authenticating,
    /// Replacing cached state or checking the shared inventory.
    Reconciling,
    /// Records have arrived but referenced content is still missing.
    MissingContent,
    /// Current at the last completed reconciliation.
    Live,
    /// Waiting for the host's retry timer.
    Waiting,
    /// A fresh invitation or authorization is required.
    Expired,
    /// Recovery failed and requires an explicit retry.
    Failed,
}

impl ConnectionStatus {
    /// Shared status text retained by the transitional extension presentation.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Connecting => "Connecting",
            Self::Authenticating => "Authenticating",
            Self::Reconciling => "Catching up",
            Self::MissingContent => "Waiting for content",
            Self::Live => "Live",
            Self::Waiting => "Waiting to reconnect",
            Self::Expired => "Invitation expired",
            Self::Failed => "Failed",
        }
    }
}

/// Shared join lifetime. Approval and credential persistence belong to the host.
#[derive(Clone, Debug, Default)]
pub struct JoinState {
    generation: u32,
    enabled: bool,
    exhausted: bool,
}

impl JoinState {
    /// Token captured before starting an asynchronous join or approval check.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Whether the client has enabled this approved sharing session.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether a pending join still belongs to this context.
    #[must_use]
    pub const fn is_current(&self, generation: u32) -> bool {
        !self.exhausted && self.generation == generation
    }

    /// Enable only the join that still owns its context.
    pub fn enable(&mut self, generation: u32) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.enabled = true;
        true
    }

    /// Stop or suspend this session and invalidate pending approval/join results.
    pub fn retire(&mut self) {
        self.enabled = false;
        if let Some(next) = self.generation.checked_add(1) {
            self.generation = next;
        } else {
            self.exhausted = true;
        }
    }
}

/// Transport-independent lifecycle shared by Crux and the extension adapter.
#[derive(Clone, Debug, Default)]
pub struct Connection {
    generation: u32,
    attempts: u32,
    status: ConnectionStatus,
}

impl Connection {
    /// Start a new attempt, retiring callbacks from all earlier attempts.
    /// Returns `None` on generation exhaustion instead of reusing a token.
    pub fn begin(&mut self) -> Option<u32> {
        self.generation = self.generation.checked_add(1)?;
        self.status = ConnectionStatus::Connecting;
        Some(self.generation)
    }

    /// Retire a join and its retry timer without changing sharing consent.
    pub fn stop(&mut self) {
        self.status = ConnectionStatus::Stopped;
        self.attempts = 0;
    }

    /// Apply a callback only to the attempt that owns it.
    /// Repeated failure callbacks cannot increase the retry delay twice.
    pub fn update(&mut self, generation: u32, status: ConnectionStatus) -> bool {
        if generation != self.generation || self.status == ConnectionStatus::Stopped {
            return false;
        }
        if status == ConnectionStatus::Waiting && self.status != ConnectionStatus::Waiting {
            self.attempts = self.attempts.saturating_add(1);
        }
        if status == ConnectionStatus::Live {
            self.attempts = 0;
        }
        self.status = status;
        true
    }

    /// Bounded retry delay. The host owns timers and optional jitter.
    #[must_use]
    pub fn retry_delay_ms(&self) -> u32 {
        1000_u32
            .saturating_mul(2_u32.saturating_pow(self.attempts.saturating_sub(1).min(5)))
            .min(30_000)
    }

    /// Current attempt identity; never an operation or delivery cursor.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Current shared status.
    #[must_use]
    pub const fn status(&self) -> ConnectionStatus {
        self.status
    }
}

/// Fields of a native inventory check needed by shared status.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PeerCheck {
    /// Native check pass identity, unrelated to an operation-ID cursor.
    pub pass: u64,
    /// The native worker completed its inventory check.
    pub complete: bool,
    /// Referenced content still unavailable.
    pub unavailable: u64,
}

/// Native progress used by all shared peer presentations.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PeerProgress {
    /// Authenticated connection acceptance, not completion of catch-up.
    pub accepted: bool,
    /// Incoming inventory work is active.
    pub synchronizing: bool,
    /// Completed inventory passes on this connection.
    pub rounds: u64,
    /// Unavailable content in the incoming direction.
    pub unavailable: u64,
    /// Outgoing inventory check when reported by the worker.
    pub outgoing: Option<PeerCheck>,
}

/// Convert native peer progress into shared status without inventing completion.
#[must_use]
pub fn peer_status(progress: &PeerProgress) -> ConnectionStatus {
    if !progress.accepted {
        ConnectionStatus::Authenticating
    } else if progress.synchronizing
        || progress.rounds == 0
        || progress
            .outgoing
            .as_ref()
            .is_some_and(|check| check.pass > 0 && !check.complete)
    {
        ConnectionStatus::Reconciling
    } else if progress.unavailable > 0
        || progress
            .outgoing
            .as_ref()
            .is_some_and(|check| check.unavailable > 0)
    {
        ConnectionStatus::MissingContent
    } else {
        ConnectionStatus::Live
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, ConnectionStatus, JoinState, PeerProgress, peer_status};

    #[test]
    fn late_join_and_replayed_failure_do_not_change_current_attempt() {
        let mut state = Connection::default();
        let first = state.begin();
        state.stop();
        let second = state.begin();
        assert_ne!(first, second, "new join owns different callbacks");
        assert!(
            !state.update(first.unwrap_or_default(), ConnectionStatus::Live),
            "late join is retired"
        );
        assert!(
            state.update(second.unwrap_or_default(), ConnectionStatus::Waiting),
            "current failure"
        );
        assert!(
            state.update(second.unwrap_or_default(), ConnectionStatus::Waiting),
            "duplicate failure"
        );
        assert_eq!(
            state.retry_delay_ms(),
            1000,
            "duplicate failure cannot increase backoff"
        );
        assert_eq!(
            peer_status(&PeerProgress {
                accepted: true,
                ..PeerProgress::default()
            }),
            ConnectionStatus::Reconciling,
            "acceptance alone is not live"
        );
        assert_eq!(
            peer_status(&PeerProgress {
                accepted: true,
                rounds: 1,
                unavailable: 1,
                ..PeerProgress::default()
            }),
            ConnectionStatus::MissingContent,
            "late blobs stay explicit"
        );
    }

    #[test]
    fn stopped_join_cannot_enable_a_new_context() {
        let mut state = JoinState::default();
        let old = state.generation();
        state.retire();
        assert!(!state.enable(old), "late approval is discarded");
        assert!(!state.enabled(), "stopped sharing stays stopped");
        assert!(
            state.enable(state.generation()),
            "new approval can enable sharing"
        );
    }
}
