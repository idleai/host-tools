//! Stable process outcomes shared by all commands.

use std::{fmt, io};

pub(crate) type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug)]
pub(crate) struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn input(message: impl Into<String>) -> Self {
        Self::new(2, message)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl From<editchain_cli_support::error::Failure> for Failure {
    fn from(error: editchain_cli_support::error::Failure) -> Self {
        Self::new(error.code, error.message)
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        editchain_cli_support::error::Failure::from(error).into()
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        editchain_cli_support::error::Failure::from(error).into()
    }
}

impl From<idle_history_import::ImportError> for Failure {
    fn from(error: idle_history_import::ImportError) -> Self {
        match error {
            idle_history_import::ImportError::Io(error) => error.into(),
            idle_history_import::ImportError::Cancelled { .. } => Self::new(130, error.to_string()),
            idle_history_import::ImportError::Json(_)
            | idle_history_import::ImportError::ResourceLimit { .. }
            | idle_history_import::ImportError::SourceGenerationChanged { .. }
            | idle_history_import::ImportError::UuidCollision { .. }
            | idle_history_import::ImportError::ProjectionProtocol { .. } => {
                Self::input(error.to_string())
            }
            idle_history_import::ImportError::CursorStore(_)
            | idle_history_import::ImportError::OpSink(_)
            | idle_history_import::ImportError::BlobSink(_)
            | idle_history_import::ImportError::HelperSpawn { .. }
            | idle_history_import::ImportError::HelperFailed { .. } => {
                Self::new(1, error.to_string())
            }
        }
    }
}

impl From<Box<dyn std::error::Error>> for Failure {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        let error = match error.downcast::<io::Error>() {
            Ok(error) => return (*error).into(),
            Err(error) => error,
        };
        match error.downcast::<idle_history_import::ImportError>() {
            Ok(error) => (*error).into(),
            Err(error) => Self::new(1, error.to_string()),
        }
    }
}
