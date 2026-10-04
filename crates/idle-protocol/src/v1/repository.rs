//! Repository facts and recorded sessions, independent of a live agent runtime.
//!
//! Git and GitHub identities never imply Idle membership, permission or online status.
//! Counts and wall times are bounded JSON integers; full history identities remain
//! strings. Each read replaces the previous snapshot within exactly one binding.

use serde::{Deserialize, Serialize};
#[cfg(test)]
mod tests;
mod validation;

/// One explicit host-installed repository and logical chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct RepositoryScope {
    /// Workspace visibility boundary.
    pub workspace_id: String,
    /// Host-installed repository identity, including checkout isolation.
    pub repository_id: String,
    /// Logical history chain.
    pub chain: String,
}

/// Whether the named read covers its declared scope.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ReadState {
    /// All results within the stated query scope were read.
    Complete,
    /// The read reached a limit or one part failed.
    Partial,
    /// No usable result is available.
    Unavailable,
    /// This repository has no applicable source for the read.
    NotApplicable,
}

/// Scope, bounds and currentness of a single source read.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct ReadReport {
    /// Stable source category such as `git.authors` or `github.issues`.
    pub topic: String,
    /// Completeness within the declared scope.
    pub state: ReadState,
    /// Human-readable scope, bound or failure reason.
    pub message: String,
    /// Epoch milliseconds when this read was attempted; not a durable cursor.
    pub checked_at_ms: u64,
    /// Earliest retry supplied by the service, if known.
    pub retry_at_ms: Option<u64>,
    /// Public HTTPS source location without credentials.
    pub source_url: Option<String>,
}

/// Changed paths counted from Git's porcelain output; rename pairs count once.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct WorktreeStatus {
    /// Paths with index changes.
    pub staged: u32,
    /// Paths with working-tree changes.
    pub unstaged: u32,
    /// Untracked paths or directories according to Git's normal status mode.
    pub untracked: u32,
    /// Unmerged paths.
    pub conflicted: u32,
}

/// The selected checkout, read locally without fetching or changing Git state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct GitCheckout {
    /// Resolved repository top-level directory.
    pub root: String,
    /// Checkout-specific Git directory, distinct in linked worktrees.
    pub git_directory: String,
    /// Shared Git directory, when linked worktrees share objects and refs.
    pub common_directory: String,
    /// Current branch; absent for a detached HEAD.
    pub branch: Option<String>,
    /// Full commit ID; absent on an unborn branch.
    pub head: Option<String>,
    /// Chosen branch remote or `origin`, if configured.
    pub remote_name: Option<String>,
    /// Sanitized remote location with user information and query removed.
    pub remote: Option<String>,
    /// Worktree counts, absent if the bounded status read failed.
    pub status: Option<WorktreeStatus>,
}

/// A recorded Git author in the declared bounded commit history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct GitAuthor {
    /// Author name recorded in commits, without identity merging.
    pub name: String,
    /// Author email recorded in commits.
    pub email: String,
    /// Commits in this read, not a repository-wide total.
    pub commits: u32,
}

/// GitHub repository metadata; no Idle permissions are derived from it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct GithubRepository {
    /// Canonical owner and repository name supplied by GitHub.
    pub full_name: String,
    /// HTTPS repository page.
    pub url: String,
    /// Supplied short description.
    pub description: Option<String>,
    /// Default branch supplied by GitHub.
    pub default_branch: String,
    /// Repository visibility supplied by GitHub.
    pub visibility: String,
}

/// A GitHub account listed by a contributor or collaborator endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct GithubPerson {
    /// Stable numeric GitHub account identity represented as text.
    pub id: String,
    /// Supplied account login.
    pub login: String,
    /// HTTPS account page.
    pub url: String,
    /// Supplied contribution count, distinct from live activity.
    pub contributions: Option<u32>,
    /// GitHub repository role, never an Idle access grant.
    pub role: Option<String>,
}

/// Full address of a single accepted stored representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct SourceRecord {
    /// Full 256-bit observation identity.
    pub observation: String,
    /// Full logical item identity.
    pub item: String,
    /// Full digest of the exact stored encoding.
    pub record_hash: String,
}

/// A recorded session item, without inferring current execution or ordering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct RecordedSession {
    /// Full logical session identity used by the history filter.
    pub id: String,
    /// Distinct recorded labels; conflicting or changing titles remain visible.
    pub labels: Vec<String>,
    /// Recorded lifecycle observations, never a claim that a runtime is alive.
    pub actions: Vec<String>,
    /// Original provider names, or the direct recorder identity.
    pub sources: Vec<String>,
    /// Exact accepted session observations within this bounded read.
    pub records: Vec<SourceRecord>,
}

/// Atomic repository replacement scoped to one host-installed binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "reflection", derive(facet::Facet))]
#[cfg_attr(
    feature = "reflection",
    expect(
        clippy::unsafe_derive_deserialize,
        reason = "Facet reflection adds no safety invariants"
    )
)]
pub struct RepositorySnapshot {
    /// Exact scope requested by the client and installed by the host.
    pub scope: RepositoryScope,
    /// Epoch milliseconds of this snapshot attempt.
    pub checked_at_ms: u64,
    /// Read-only local checkout details, if available.
    pub checkout: Option<GitCheckout>,
    /// GitHub metadata, if the remote and current credentials support it.
    pub github: Option<GithubRepository>,
    /// Account used for this read; absent for public unauthenticated access.
    pub account: Option<String>,
    /// Git authors within the read's declared bounds.
    pub git_authors: Vec<GitAuthor>,
    /// GitHub contribution history, distinct from collaboration access.
    pub contributors: Vec<GithubPerson>,
    /// Accessible GitHub collaborator entries, without Idle grants.
    pub collaborators: Vec<GithubPerson>,
    /// Local and imported recorded session items.
    pub sessions: Vec<RecordedSession>,
    /// Source availability and explicit bounds, including known empty results.
    pub reports: Vec<ReadReport>,
}
