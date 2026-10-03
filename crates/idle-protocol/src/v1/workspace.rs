//! Workspace-to-chain binding and repository attachments in either mode.

use serde::{Deserialize, Serialize};

use super::identity::{ChainRef, RepositoryId, WorkspaceId};

/// Repository metadata; a repository may be attached to multiple workspaces.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Repository {
    /// Stable repository identity, independent of checkout paths.
    pub id: RepositoryId,
    /// Display label.
    pub name: String,
    /// Credential-free canonical Git locator, if published.
    pub remote: Option<String>,
}

/// Coordination mode and its repository attachments.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinationMode {
    /// OSS coordination for exactly one repository, without managed sign-in.
    Standalone {
        /// Repository supplying standalone context.
        repository: Repository,
    },
    /// Managed metadata for zero or more attached repositories.
    Managed {
        /// Repositories whose history is bound to this workspace's chain.
        repositories: Vec<Repository>,
    },
}

/// One workspace binds to exactly one logical chain.
///
/// Changing coordination mode preserves workspace, chain, repository and session
/// identities. The engine receives only `chain`, never this metadata object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Workspace {
    /// Workspace identity; must match the request/event scope.
    pub id: WorkspaceId,
    /// Presentation label.
    pub name: String,
    /// Immutable logical chain binding.
    pub chain: ChainRef,
    /// Coordination source and repository bindings.
    pub mode: CoordinationMode,
}
