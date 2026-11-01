//! Immutable file descriptors advertised by a commit's first-parent diff.

use serde::{Deserialize, Serialize};

/// Exact file change; native resolution checks every field against the commit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineCommitFile")]
pub struct CommitFile {
    /// Full repository identity as decimal text.
    pub repository: String,
    /// Complete commit hash.
    pub oid: String,
    /// Current repository-relative path, or deleted path.
    pub path: String,
    /// Previous path of a renamed or copied file.
    pub old_path: Option<String>,
    /// First-parent blob, or absent for an addition.
    pub old_oid: Option<String>,
    /// Commit-side blob, or absent for a deletion.
    pub new_oid: Option<String>,
    /// First-parent entry mode.
    pub old_mode: Option<String>,
    /// Commit-side entry mode.
    pub new_mode: Option<String>,
    /// Git status letter: A, D, M, R, C, or T.
    pub status: String,
    /// Either side cannot be represented faithfully as UTF-8 text.
    pub binary: bool,
}

impl CommitFile {
    /// Validate bounded wire fields before asking the native host to resolve them.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let path = |value: &str| !value.is_empty() && value.len() <= 4_096 && !value.contains('\0');
        let oid = |value: &str| {
            matches!(value.len(), 40 | 64)
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let mode = |value: &str| matches!(value, "blob" | "exe" | "link" | "commit");
        (super::Target::Commit {
            repository: self.repository.clone(),
            oid: self.oid.clone(),
        })
        .is_valid()
            && path(&self.path)
            && self.old_path.as_deref().is_none_or(path)
            && self.old_oid.as_deref().is_none_or(oid)
            && self.new_oid.as_deref().is_none_or(oid)
            && self.old_mode.as_deref().is_none_or(mode)
            && self.new_mode.as_deref().is_none_or(mode)
            && matches!(self.status.as_str(), "A" | "D" | "M" | "R" | "C" | "T")
            && (self.old_oid.is_some() || self.new_oid.is_some())
    }
}
