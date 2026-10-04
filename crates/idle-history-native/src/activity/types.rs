use editchain_engine::activity::TextRange;
use idle_history::{binding::RepositoryChainBinding, query::RecordRef};
use serde::{Deserialize, Serialize};

use crate::history::Source;

/// One explicit revision selected by the editor host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Selection {
    /// A retained file observation, including its original record digest.
    Record(RecordRef),
    /// A live capture occurrence; equal bytes never select another occurrence.
    Editor {
        /// Capture incarnation.
        session: String,
        /// Document incarnation in that capture.
        document: String,
        /// VS Code buffer version.
        version: u64,
    },
}

/// Native activity read, scoped to host-installed history storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Logical workspace, repository and chain.
    pub binding: RepositoryChainBinding,
    /// Current or separately retained records.
    pub source: Source,
    /// Exact file revision to inspect.
    pub selection: Selection,
}

/// Recorded classification, never a claim about comprehension or review.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndicatorKind {
    /// A recorded person author or a confirmed editor input receipt.
    Human,
    /// An explicitly recorded agent author.
    Ai,
    /// A tool or system author.
    Other,
    /// Missing, unsupported or contradictory attribution.
    Unknown,
    /// Recorded visibility.
    Exposure,
    /// Recorded read interval, without claiming comprehension.
    Read,
}

/// Exact source for existing native record and Original actions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRecord {
    /// Original encoded representation.
    pub record: RecordRef,
    /// Whether an Original link was recorded; its content can still be missing.
    pub original: bool,
}

/// File-level or precisely mapped range indicator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Indicator {
    /// Color and category for the native host.
    pub kind: IndicatorKind,
    /// Half-open UTF-16 coordinates; absent means a file observation only.
    pub range: Option<TextRange>,
    /// Bounded plain text, never trusted Markdown.
    pub label: String,
    /// Records supplying the assertion and author metadata.
    pub sources: Vec<SourceRecord>,
}

/// Complete result for one request, with explicit observation limits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Preview {
    /// Retained for asynchronous result correlation.
    pub request: Request,
    /// Observation establishing the selected snapshot.
    pub record: RecordRef,
    /// Recorded logical revision, if the producer supplied one.
    pub revision: Option<String>,
    /// Exact UTF-8 snapshot; the host must compare it with the displayed buffer.
    pub text: String,
    /// Positive recorded activity only; absent activity remains unknown.
    pub indicators: Vec<Indicator>,
    /// Missing data, known gaps and bounded-query limitations.
    pub issues: Vec<String>,
}
