//! Explicit repository and caller context shared by native services and applications.

use serde::{Deserialize, Serialize};

/// Explicit repository-to-chain resolution in one workspace, never a global map.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection helpers do not impose safety invariants on these fields"
)]
pub struct RepositoryChainBinding {
    /// Workspace through which the repository is selected.
    pub workspace_id: String,
    /// Reusable repository identity.
    pub repository_id: String,
    /// That workspace's immutable logical chain reference.
    pub chain: String,
}

/// Full authorization and history context. Returning to the same context starts
/// a new request lifetime; these strings never substitute for provider grants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct Context {
    /// Configured standalone or managed provider identity.
    pub provider: String,
    /// Logical workspace, preserved across provider adoption.
    pub workspace: String,
    /// Authenticated audience; never a host/device label.
    pub contributor: String,
    /// Explicit logical chain binding resolved by the host.
    pub chain: String,
}
