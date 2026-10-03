//! Fixed errors suitable for service responses; SDK diagnostics stay private.

/// A coordination or transport operation could not complete.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, thiserror::Error,
)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    /// Invalid or unsupported input.
    #[error("invalid coordination data")]
    Invalid,
    /// Another writer changed the expected state.
    #[error("coordination state changed")]
    Conflict,
    /// The operation's lifetime ended.
    #[error("operation cancelled")]
    Cancelled,
    /// The bounded operation did not finish.
    #[error("operation timed out")]
    Timeout,
    /// Previously approved access no longer permits a connection.
    #[error("approved access expired; request a fresh invitation")]
    Expired,
    /// The authenticated participant lacks permission.
    #[error("access is not approved")]
    Forbidden,
    /// Connected components require a coordinated update.
    #[error("incompatible versions; update both peers and reconnect")]
    Version,
    /// Durable state was not confirmed; callers must reconcile before retrying.
    #[error("durable coordination storage is unavailable")]
    Storage,
    /// The relay or remote peer is unavailable.
    #[error("peer transport is unavailable")]
    Transport,
    /// Another authority or native writer owns a required resource.
    #[error("coordination resource is busy")]
    Busy,
}

/// Result returned by native coordination adapters.
pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        let kind = error.kind();
        if kind == std::io::ErrorKind::Unsupported {
            Self::Version
        } else if kind == std::io::ErrorKind::PermissionDenied {
            Self::Forbidden
        } else if kind == std::io::ErrorKind::WouldBlock {
            Self::Busy
        } else if matches!(
            kind,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData
        ) {
            Self::Invalid
        } else {
            Self::Storage
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(_error: serde_json::Error) -> Self {
        Self::Invalid
    }
}
