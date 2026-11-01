//! Attributed session calls carried on an authenticated daemon connection.

use idle_protocol::v1::{
    api::{Request, RuntimeSubmission},
    grants::GrantCommand,
    identity::{HostId, RequestKey, SessionId},
    sessions::{Completion, InputRef},
};
use serde::{Deserialize, Serialize};

/// Transport grant selected by the daemon, separate from session participation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Participant {
    /// Native runner invitation used to accept this input.
    pub grant_id: String,
    /// Whether the saved runner permits filesystem writes.
    pub write: bool,
}

/// Immutable runner creation intent. Execution permissions remain separate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// Title supplied before allocating the runner.
    pub title: String,
    /// Compute host chosen by the contributor.
    pub host_id: HostId,
}

/// Client operations; the daemon supplies the authenticated contributor separately.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestBody {
    /// Authorized directory, retained input states and exact submitted text.
    Snapshot,
    /// Reserve or recover one native thread identity for an immutable request.
    Create {
        /// Original creation envelope.
        request: Request<Create>,
    },
    /// Durably accept and order one attributed input.
    Submit {
        /// Original prompt and request identity.
        request: RuntimeSubmission,
    },
    /// Issue or revoke session participation without expanding compute access.
    Grant {
        /// Original grant envelope, limited to session scopes.
        request: Request<GrantCommand>,
    },
    /// Recover the previous result without dispatching it again.
    RequestStatus {
        /// Original request identity.
        key: RequestKey,
    },
    /// Read one visible input without changing its author or dispatching it again.
    InputStatus {
        /// Original input and request identity.
        input: InputRef,
    },
}

/// Private daemon work; never accepted directly from the client-facing RPC.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Work {
    /// Check whether the legacy direct-input route is still valid for this thread.
    DirectInput {
        /// Native thread targeted by the original command client.
        session_id: SessionId,
    },
    /// Confirm native allocation before reporting successful creation.
    Created {
        /// Native thread reserved by the create request.
        session_id: SessionId,
    },
    /// Claim the oldest queued input. A claim is not reported as running.
    Claim {
        /// Loaded native thread.
        session_id: SessionId,
    },
    /// Record the native turn after Core confirmed it started.
    Started {
        /// Exact claimed input.
        input: InputRef,
        /// Native turn identity returned by Core.
        turn_id: String,
    },
    /// Record a terminal native outcome.
    Completed {
        /// Exact claimed input.
        input: InputRef,
        /// Runtime-confirmed outcome, including interruption.
        outcome: Completion,
    },
    /// Release an unstarted claim when Core reports that the thread is busy.
    Release {
        /// Exact claimed input.
        input: InputRef,
    },
}
