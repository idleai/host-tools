use idle_protocol::v1::{
    Record, WriteCondition,
    api::{ApiError, ErrorCode, RetryAdvice},
    configuration::ConfigurationValue,
    identity::Revision,
    workspace::{CoordinationMode, Workspace},
};

pub(super) type Checked<T> = Result<T, ApiError>;

pub(super) fn failure(code: ErrorCode) -> ApiError {
    let message = match code {
        ErrorCode::InvalidRequest => "Invalid repository request",
        ErrorCode::UnsupportedVersion => "Unsupported document or service version",
        ErrorCode::UnsupportedOperation => "Operation is unavailable from this provider",
        ErrorCode::Unauthenticated => "Request identity does not match the connection",
        ErrorCode::Forbidden => "Current access does not permit this operation",
        ErrorCode::NotFound => "Resource is unavailable in this repository",
        ErrorCode::Conflict => "Repository state conflicts with this operation",
        ErrorCode::StaleRevision => "The metadata revision changed",
        ErrorCode::StaleControl => "Controller ownership is no longer current",
        ErrorCode::IdempotencyConflict => "The request identity was used for different content",
        ErrorCode::RequestExpired => "The original request deadline elapsed",
        ErrorCode::CursorScopeMismatch => "Recovery cursor belongs to a different scope",
        ErrorCode::Unavailable => "Coordination is unavailable",
        ErrorCode::RateLimited => "Repository capacity has been reached",
    };
    ApiError {
        code,
        message: message.into(),
        retry: RetryAdvice::Never,
    }
}

pub(super) fn require(condition: bool, code: ErrorCode) -> Checked<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(code))
    }
}

pub(super) fn id(value: &str) -> Checked<()> {
    require(
        !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        ErrorCode::InvalidRequest,
    )
}

pub(super) fn label(value: &str) -> Checked<()> {
    require(
        !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
        ErrorCode::InvalidRequest,
    )
}

pub(super) fn workspace(value: &Workspace) -> Checked<()> {
    id(&value.id.0)?;
    id(&value.chain.0)?;
    label(&value.name)?;
    let repository = match &value.mode {
        CoordinationMode::Standalone { repository } => repository,
        CoordinationMode::Managed { .. } => return Err(failure(ErrorCode::UnsupportedOperation)),
    };
    id(&repository.id.0)?;
    label(&repository.name)?;
    if let Some(remote) = &repository.remote {
        require(
            remote.len() <= 4096 && !remote.chars().any(char::is_control),
            ErrorCode::InvalidRequest,
        )?;
        if let Ok(url) = url::Url::parse(remote) {
            require(
                url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && (url.username().is_empty()
                        || url.scheme() == "ssh" && url.username() == "git"),
                ErrorCode::InvalidRequest,
            )?;
        } else {
            require(
                remote.starts_with("git@") && remote.contains(':'),
                ErrorCode::InvalidRequest,
            )?;
        }
    }
    Ok(())
}

pub(super) fn configuration(value: &ConfigurationValue) -> Checked<()> {
    require(value.schema_version == 1, ErrorCode::UnsupportedVersion)?;
    require(value.json.len() <= 256 * 1024, ErrorCode::InvalidRequest)?;
    require(
        serde_json::from_str::<serde_json::Value>(&value.json).is_ok_and(|json| json.is_object()),
        ErrorCode::InvalidRequest,
    )
}

pub(super) fn record<T>(
    current: Option<&Record<T>>,
    expected: &WriteCondition,
    value: T,
) -> Checked<Record<T>> {
    let revision = match (current, expected) {
        (None, WriteCondition::Absent) => 1,
        (Some(current), WriteCondition::Revision(expected)) if current.revision == *expected => {
            current
                .revision
                .0
                .checked_add(1)
                .ok_or_else(|| failure(ErrorCode::Conflict))?
        }
        (None, WriteCondition::Revision(_))
        | (Some(_), WriteCondition::Absent | WriteCondition::Revision(_)) => {
            return Err(failure(ErrorCode::StaleRevision));
        }
    };
    Ok(Record {
        revision: Revision(revision),
        value,
    })
}
