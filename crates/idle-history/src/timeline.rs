//! Indexed Activity windows shared by native hosts and application reducers.
//!
//! Occurrences identify individual activities, even when several activities
//! update one logical item. Cursors belong to a complete derived revision.

mod rows;
pub use rows::*;

use serde::{Deserialize, Serialize};

use crate::query::{Filter, RecordRef};

/// Timeline contract with exact recorded and live Git destinations.
pub const VERSION: u32 = 2;
/// Ordinary window size.
pub const DEFAULT_LIMIT: u32 = 200;
/// Largest allowed window or match page.
pub const MAX_LIMIT: u32 = 500;
/// Maximum retained client row summaries.
pub const MAX_CACHED_ROWS: usize = 2_000;
/// Maximum serialized timeline data retained by a client.
pub const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;

/// Physical namespace installed by the native host.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Serialize,
    facet::Facet,
)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
#[facet(rename = "TimelineSource")]
pub enum Source {
    /// Accepted current records and their recorded migration mappings.
    #[default]
    Current,
    /// Explicitly bound retained records or a migration archive.
    Retained,
}

/// An exact record in an explicitly bound source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineAddress")]
pub struct Address {
    /// Current or retained namespace; never a filesystem path.
    pub source: Source,
    /// Full operation ID and digest of its original stored encoding.
    pub record: RecordRef,
}

/// An exact native destination, without assigning operation IDs to Git objects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize, facet::Facet)]
#[repr(u8)]
#[facet(rename = "TimelineTarget")]
pub enum Target {
    /// Accepted current or retained operation bytes.
    Record(Address),
    /// Immutable commit in the host's explicitly bound repository.
    Commit {
        /// Full repository identity as decimal text.
        repository: String,
        /// Complete SHA-1 or SHA-256 commit hash.
        oid: String,
    },
}

impl Target {
    /// Operation address when this destination refers to a stored operation.
    #[must_use]
    pub const fn record(&self) -> Option<&Address> {
        match self {
            Self::Record(address) => Some(address),
            Self::Commit { .. } => None,
        }
    }

    /// Validate full identities at the client boundary.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let hex = |value: &str| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        match self {
            Self::Record(address) => {
                address.record.operation.len() == 64
                    && address.record.hash.len() == 64
                    && hex(&address.record.operation)
                    && hex(&address.record.hash)
            }
            Self::Commit { repository, oid } => {
                repository.parse::<u64>().is_ok() && matches!(oid.len(), 40 | 64) && hex(oid)
            }
        }
    }
}

/// Exclusive position in one complete timeline revision and view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineCursor")]
pub struct Cursor {
    /// Opaque snapshot identity.
    pub revision: String,
    /// Digest of filters and disclosure choices used for this cursor.
    pub view: String,
    /// Absolute offset in that view, increasing toward older activities.
    pub offset: u64,
}

/// One explicit disclosure choice. Omitted groups use their recorded status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineDisclosure")]
pub struct Disclosure {
    /// Stable group identity.
    pub group: String,
    /// Whether its constituent activities are visible.
    pub expanded: bool,
}

/// Scope and disclosure state applied before paging.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineView")]
pub struct View {
    /// Recorded kinds, author, recorder, session, or path.
    pub filter: Filter,
    /// Manual or temporary choices, independent for each client composition.
    pub disclosures: Vec<Disclosure>,
}

/// Starting point for a bounded window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[repr(u8)]
#[facet(rename = "TimelinePosition")]
pub enum Position {
    /// Newest projected activities.
    Latest,
    /// Window containing this exact occurrence, opening its group if needed.
    Seek(String),
    /// Continue at a cursor returned by this view and snapshot.
    Page(Cursor),
    /// Reconcile around an occurrence after accepting new recorded data.
    Refresh(Option<String>),
}

/// One bounded query against a versioned Activity index.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineRequest")]
pub struct Request {
    /// Explicit supported wire version.
    pub version: u32,
    /// Query within the bound repository and chain.
    pub action: Action,
}

/// Timeline reads and resumable index construction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[repr(u8)]
#[facet(rename = "TimelineAction")]
pub enum Action {
    /// Query or begin constructing a complete snapshot.
    Window {
        /// Exact scope and disclosure choices.
        view: View,
        /// Latest, seek, page, or refresh position.
        position: Position,
        /// Maximum returned rows, in 1..=500.
        limit: u32,
    },
    /// Literal, case-insensitive search over indexed recorded text.
    Find {
        /// Scope and current disclosure choices.
        view: View,
        /// Literal text; no regular expressions.
        text: String,
        /// Continue a prior match page in the same revision.
        cursor: Option<Cursor>,
        /// Maximum returned matches, in 1..=500.
        limit: u32,
    },
    /// Inspect a group's exact members through bounded pages.
    Members {
        /// Snapshot identity from the group row.
        revision: String,
        /// Stable group identity.
        group: String,
        /// Offset within the group.
        offset: u64,
        /// Maximum returned members, in 1..=500.
        limit: u32,
    },
    /// Advance one bounded indexing batch; clients can stop between batches.
    Advance {
        /// Build identity returned by the host.
        build: String,
    },
    /// Retire a pending build while retaining the last complete snapshot.
    Cancel {
        /// Build identity returned by the host.
        build: String,
    },
}

/// A supported timeline response; progress never masquerades as latest history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[repr(u8)]
#[facet(rename = "TimelineResponse")]
pub enum Response {
    /// Complete revision with a bounded visible window.
    Window(Window),
    /// Indexed matches, each anchored to its actual occurrence.
    Found(Matches),
    /// Progress toward a complete replacement revision.
    Building(Progress),
    /// The requested build was cancelled.
    Cancelled,
    /// The requested cursor expired; seek the retained occurrence again.
    Stale,
}

/// Progress reported between cancellable native indexing batches.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineProgress")]
pub struct Progress {
    /// Opaque resumable build identity.
    pub build: String,
    /// Human-readable stage, without implementation paths.
    pub stage: String,
    /// Records processed in this stage.
    pub processed: u64,
    /// Known stage total, when available.
    pub total: Option<u64>,
    /// Last complete snapshot remains available until this build is published.
    pub previous_revision: Option<String>,
}

/// Rows, grouping and graph coverage from one atomically published revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineWindow")]
pub struct Window {
    /// Supported timeline wire version.
    pub version: u32,
    /// Opaque revision shared by every row and route.
    pub revision: String,
    /// Ordered rows, newest first.
    pub rows: Vec<Row>,
    /// Window before this one, toward newer activities.
    pub newer: Option<Cursor>,
    /// Window after this one, toward older activities.
    pub older: Option<Cursor>,
    /// Offset of the first returned row in the requested view.
    pub offset: u64,
    /// Exact projected activity count in this scope before folding.
    pub activities: u64,
    /// Number of visible rows with these disclosure choices.
    pub visible_rows: u64,
    /// Highest occupied abstract lane, stable across windows in this revision.
    pub max_lane: u32,
    /// Recorded data that prevents a complete relationship or content result.
    pub gaps: Vec<String>,
    /// Replacement build, if the previous complete snapshot is being served.
    pub rebuilding: Option<Progress>,
}

/// One matching occurrence, which can be inside a folded group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineMatch")]
pub struct Match {
    /// Stable exact occurrence identity to seek.
    pub occurrence: String,
    /// Containing group, if any.
    pub group: Option<String>,
    /// Exact native operation address.
    pub address: Target,
    /// Bounded matching recorded text.
    pub preview: String,
}

/// A bounded match page with an exact count from the searchable index.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet reflection adds no safety invariants to these wire fields"
)]
#[facet(rename = "TimelineMatches")]
pub struct Matches {
    /// Snapshot identity shared by all hits.
    pub revision: String,
    /// Matches in timeline order.
    pub matches: Vec<Match>,
    /// Exact total matching activities.
    pub total: u64,
    /// Continue at this match position.
    pub next: Option<Cursor>,
    /// Activities whose complete recorded text is unavailable.
    pub unavailable: u64,
}
