//! Compact semantic rows and abstract routing. Full content stays native.

use serde::{Deserialize, Serialize};

use super::{Address, Target};
use crate::query::{ActivityKind, OpenTarget};

/// A displayed occurrence or a safe connected group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineRow")]
pub struct Row {
    /// Stable occurrence identity, independent of the logical item.
    pub occurrence: String,
    /// Logical item updated by this occurrence.
    pub item: String,
    /// Exact recorded operation or live Git commit for native activation.
    pub address: Target,
    /// Constituent exact records, including linked raw inputs and folded members.
    pub records: Vec<Address>,
    /// Recorded category.
    pub kind: ActivityKind,
    /// Concise activity label.
    pub title: String,
    /// Bounded recorded preview or file path.
    pub preview: String,
    /// Recorded actor label, or its complete identity when unnamed.
    pub author: String,
    /// Recorded session label, when known.
    pub session: String,
    /// Concise recorded classifications.
    pub tags: Vec<String>,
    /// Unmodified recorded Unix time in milliseconds, when supplied.
    pub timestamp: Option<u64>,
    /// Native activation for this occurrence.
    pub open: OpenTarget,
    /// Why activation falls back to operation JSON, when applicable.
    pub unavailable: Option<String>,
    /// Group summary or membership.
    pub group: Option<Group>,
    /// Exact semantic attachments; unresolved targets remain explicit.
    pub relationships: Vec<Relationship>,
    /// Coalesced coverage, including rails with both endpoints outside a window.
    pub graph: Geometry,
}

/// Safe connected task interval with explicit entry and exit attachments.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineGroup")]
pub struct Group {
    /// Stable recorded task and path identity.
    pub id: String,
    /// Total projected member activities, including the representative.
    pub count: u64,
    /// Recorded task prompt or human work caption with its known status.
    #[serde(default)]
    pub summary: String,
    /// Whether recorded execution is still live.
    pub live: bool,
    /// Whether this row is the group header.
    pub header: bool,
    /// Effective disclosure state in this view.
    pub expanded: bool,
    /// First activity in causal order.
    pub entry: String,
    /// Last activity in causal order and native representative.
    pub exit: String,
}

/// Relationship semantics are separate from immutable physical parent records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[repr(u8)]
#[facet(rename = "TimelineRelationshipKind")]
pub enum RelationshipKind {
    /// Recorded causal continuation.
    Parent,
    /// Exact activation to a child execution's first activity.
    Spawn,
    /// Child execution terminal to its parent's completion activity.
    Completion,
    /// Explicit fork boundary.
    Fork,
    /// Repository-qualified commit or recorded commit parent.
    Git,
}

/// One semantic attachment, resolved without time or label guesses.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineRelationship")]
pub struct Relationship {
    /// Type of the recorded attachment.
    pub kind: RelationshipKind,
    /// Older endpoint occurrence, when established.
    pub parent: Option<String>,
    /// Exact record or Git object supplying the relationship.
    pub source: Target,
    /// Explicit reason an endpoint could not be established.
    pub unresolved: Option<String>,
}

/// One shared transition between two lane columns.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineTransition")]
pub struct Transition(
    /// Lane at the beginning of the transition.
    pub u32,
    /// Lane at the end of the transition.
    pub u32,
);

/// Shared row coverage in lane units; pixels and SVG belong to the renderer.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineGeometry")]
pub struct Geometry {
    /// Lane carrying this activity's node.
    pub lane: u32,
    /// Occupied rails entering the upper row boundary.
    pub above: Vec<u32>,
    /// Occupied rails leaving the lower row boundary.
    pub below: Vec<u32>,
    /// Lane transitions at the node, drawn once per shared segment.
    pub transitions: Vec<Transition>,
    /// Muted subsets of the corresponding rails and transitions.
    pub muted_above: Vec<u32>,
    /// Muted lower rails.
    pub muted_below: Vec<u32>,
    /// Muted lane transitions.
    pub muted_transitions: Vec<Transition>,
    /// Exact known parent occurrence identities.
    pub parents: Vec<String>,
    /// Whether filtered or folded rows lie immediately below this row.
    pub clipped_below: bool,
}
