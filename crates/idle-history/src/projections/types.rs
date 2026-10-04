//! Native/WASM shell representation of the shared projection inputs.

use serde::{Deserialize, Serialize};

/// Presentation destination, not a controller taxonomy or an engine record kind.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, facet::Facet,
)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProjectionKind {
    /// Recorded workspace activity.
    Activity,
    /// Provider-supplied tasks.
    Task,
    /// Provider-supplied errors.
    Error,
    /// Items the provider has placed in triage.
    Triage,
    /// Requests the provider has marked as needing human input.
    NeedInput,
}

impl ProjectionKind {
    /// The five required destinations in a replacement snapshot.
    pub const ALL: [Self; 5] = [
        Self::Activity,
        Self::Task,
        Self::Error,
        Self::Triage,
        Self::NeedInput,
    ];
}

/// Full history address within the enclosing snapshot's logical chain.
///
/// At least one of observation/item is required. If both are present, the item
/// is the logical identity of that observation, not a note target or link endpoint.
/// Use separate references for targets, endpoints and summary coverage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionReference {
    /// Full 256-bit observation identity in lowercase hexadecimal, never a prefix.
    pub observation: Option<String>,
    /// Full 256-bit logical item identity, independent of the observation ID.
    pub item: Option<String>,
    /// Full digest of an exact stored representation, only with an observation.
    pub record_hash: Option<String>,
}

/// Provider's explicit assessment; clocks and operation-ID order cannot imply it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FreshnessStatus {
    /// Provider has established that its declared coverage is current.
    Current,
    /// Provider knows that its derivation requires rebuilding.
    Stale,
    /// No currentness claim is available.
    Unknown,
}

/// Freshness of one result and all of its rows, preserved across client filtering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionFreshness {
    /// Explicit provider assessment.
    pub status: FreshnessStatus,
    /// When this result was computed, if supplied; not an ordering watermark.
    pub generated_at_ms: Option<u64>,
    /// Opaque provider checkpoint; clients never order it or use it as a page cursor.
    pub checkpoint: Option<String>,
}

/// Scope completeness, independent of freshness or the number of returned rows.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProjectionAvailability {
    /// Provider returned all results in the requested scope without known gaps.
    Complete,
    /// Bounded results, unresolved records/content, or incomplete derivation.
    Partial,
    /// The adapter or required controller mapping is unavailable.
    Unavailable,
}

/// A presentable limitation with any exact address known to the provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionGap {
    /// Missing, conflicted or otherwise unresolved history address, when known.
    pub reference: Option<ProjectionReference>,
    /// Provider-supplied explanation; never interpreted as a status code.
    pub message: String,
}

/// One supplied presentation row. Status and labels have no app-core semantics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionRow {
    /// Stable provider key, unique within its destination; never a shortened ID.
    pub key: String,
    /// Supplied heading.
    pub title: String,
    /// Optional supplied display text; drill-down uses the retained references.
    pub summary: Option<String>,
    /// Optional public HTTPS source page, distinct from the exact history records.
    #[serde(default)]
    pub url: Option<String>,
    /// Opaque provider status; app-core only filters for exact equality.
    pub status: Option<String>,
    /// Opaque supplied labels; app-core never infers them from recorded text.
    pub labels: Vec<String>,
    /// Records that produced the row; each source must name an observation.
    /// Retain its logical item and original encoding digest whenever available.
    pub sources: Vec<ProjectionReference>,
    /// Targets, Link endpoints, summary coverage or other supplied history links.
    pub related: Vec<ProjectionReference>,
}

/// One destination's input; completeness and freshness are always explicit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionInput {
    /// Destination view.
    pub kind: ProjectionKind,
    /// Currentness applies to the entire supplied result.
    pub freshness: ProjectionFreshness,
    /// Distinguishes known empty results from partial or unavailable results.
    pub availability: ProjectionAvailability,
    /// Supplied scope total, possibly unknown for partial/unavailable results.
    pub total: Option<u64>,
    /// Rows in provider order; clients do not sort by observation identity.
    pub rows: Vec<ProjectionRow>,
    /// Explicit limitations, including bounds, conflicts and missing content.
    pub gaps: Vec<ProjectionGap>,
}

/// Versioned replacement input shared by Evo, standalone/managed hosts and clients.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants"
)]
pub struct ProjectionSnapshot {
    /// Required shared input version, independent of controller payload versions.
    pub version: u16,
    /// Authorized workspace scope; a chain alone does not establish visibility.
    pub workspace_id: String,
    /// Exactly one logical chain; every history reference is relative to it.
    pub chain: String,
    /// Exactly one input for each destination, including unavailable destinations.
    pub inputs: Vec<ProjectionInput>,
}
