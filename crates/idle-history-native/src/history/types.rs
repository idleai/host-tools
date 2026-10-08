use editchain_engine::queries::{ContentField, ContentReference};
use idle_history::{binding::RepositoryChainBinding, query::RecordRef};
use serde::{Deserialize, Serialize};

pub use idle_history::timeline::Source;

/// Complete history intent; paths to storage never come from a webview.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Workspace-local repository and logical chain selection.
    pub binding: RepositoryChainBinding,
    /// Current records or explicitly retained inputs.
    #[serde(default)]
    pub source: Source,
    /// Exact stored representation, including its digest.
    pub record: RecordRef,
    /// Requested native action.
    pub target: Target,
}

/// Actions corresponding to app-core's native history targets, plus field reads.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Target {
    /// Decoded selected operation formatted as a read-only JSON document.
    OperationJson,
    /// Exact encoded record, including a specifically selected conflict variant.
    Record,
    /// Exact Original payload, following only the recorded Original reference.
    Original,
    /// Complete recorded resulting file snapshot.
    File,
    /// Complete recorded before and after snapshots.
    Diff,
    /// A schema field anchored to its record and complete content reference.
    Content {
        /// Exact engine field; never a rendered preview's location.
        field: ContentField,
        /// Recorded external address and length, or `None` for inline bytes.
        reference: Option<ContentReference>,
    },
}

/// One byte-exact read-only document for the native editor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Document {
    /// Display name only; never resolved as a filesystem path.
    pub name: String,
    /// Actual record supplying these bytes (which can be a linked Original).
    pub record: RecordRef,
    /// `None` identifies the original encoded record itself.
    pub field: Option<ContentField>,
    /// Complete external content identity and declared length when recorded.
    pub reference: Option<ContentReference>,
    /// Shared lossless JSON codec: UTF-8 text or binary byte array.
    #[serde(with = "editchain_engine::text_bytes")]
    pub bytes: Vec<u8>,
}

/// A single document or an ordered pair of complete diff sides.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Preview {
    /// Exact request retained for host-side result correlation.
    pub request: Request,
    /// One document, or before/after in that order for `Diff`.
    pub documents: Vec<Document>,
}

/// Stable failures; absent bytes are never represented by an empty document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    /// Caller selected a different workspace, repository, chain or source.
    BindingMismatch,
    /// ID, digest or content reference is incomplete or malformed.
    InvalidReference,
    /// No retained record has this identity.
    MissingRecord,
    /// A different encoded representation exists under this identity.
    RecordMismatch,
    /// An old identity maps to one or more converted records.
    MigratedAlias,
    /// The operation identity has multiple recorded representations.
    Conflicted,
    /// No value was recorded for this field.
    NotRecorded,
    /// Referenced content has not arrived.
    MissingContent,
    /// Stored content fails address or recorded-length validation.
    CorruptContent,
    /// Local or truncated content addresses cannot be resolved.
    UnresolvableContent,
    /// The action requires another record kind or an installed source.
    Unavailable,
    /// Storage or index access failed.
    Storage,
    /// The complete response exceeds the native transport's bound.
    TooLarge,
}

/// Presentable failure with exact alternative records for explicit selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Failure {
    /// Stable classification.
    pub code: FailureCode,
    /// User-presentable explanation without recorded payloads.
    pub message: String,
    /// Converted targets or conflicting representations; never selected implicitly.
    pub candidates: Vec<RecordRef>,
}

impl Failure {
    pub(crate) fn new(code: FailureCode, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
            candidates: Vec::new(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl From<std::io::Error> for Failure {
    fn from(_error: std::io::Error) -> Self {
        Self::new(
            FailureCode::Storage,
            "Unable to read the bound history source.",
        )
    }
}
