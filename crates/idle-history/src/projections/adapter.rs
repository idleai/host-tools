//! Lossless conversions between shared JSON inputs and native shell payloads.

use idle_protocol::v1::{
    ApiVersion,
    identity::{ProjectionCount, Timestamp},
    projections as protocol,
};

use super::{
    FreshnessStatus, ProjectionAvailability, ProjectionFreshness, ProjectionGap, ProjectionInput,
    ProjectionKind, ProjectionReference, ProjectionRow, ProjectionSnapshot,
};
use crate::query::Error;

pub(super) fn error(message: impl std::fmt::Display) -> Error {
    Error {
        message: message.to_string(),
    }
}

macro_rules! map_enum {
    ($name:ident { $($variant:ident),+ }) => {
        impl From<protocol::$name> for $name {
            fn from(value: protocol::$name) -> Self {
                match value { $(protocol::$name::$variant => Self::$variant),+ }
            }
        }
        impl From<$name> for protocol::$name {
            fn from(value: $name) -> Self {
                match value { $($name::$variant => Self::$variant),+ }
            }
        }
    };
}

map_enum!(ProjectionKind {
    Activity,
    Task,
    Error,
    Triage,
    NeedInput
});
map_enum!(FreshnessStatus {
    Current,
    Stale,
    Unknown
});
map_enum!(ProjectionAvailability {
    Complete,
    Partial,
    Unavailable
});

macro_rules! map_fields {
    ($name:ident { $($field:ident),+ }) => {
        impl From<protocol::$name> for $name {
            fn from(value: protocol::$name) -> Self {
                Self { $($field: value.$field),+ }
            }
        }
        impl From<$name> for protocol::$name {
            fn from(value: $name) -> Self {
                Self { $($field: value.$field),+ }
            }
        }
    };
}

map_fields!(ProjectionReference {
    observation,
    item,
    record_hash
});

impl From<protocol::ProjectionFreshness> for ProjectionFreshness {
    fn from(value: protocol::ProjectionFreshness) -> Self {
        Self {
            status: value.status.into(),
            generated_at_ms: value.generated_at.map(|time| time.0),
            checkpoint: value.checkpoint,
        }
    }
}

impl From<ProjectionFreshness> for protocol::ProjectionFreshness {
    fn from(value: ProjectionFreshness) -> Self {
        Self {
            status: value.status.into(),
            generated_at: value.generated_at_ms.map(Timestamp),
            checkpoint: value.checkpoint,
        }
    }
}

impl From<protocol::ProjectionGap> for ProjectionGap {
    fn from(value: protocol::ProjectionGap) -> Self {
        Self {
            reference: value.reference.map(Into::into),
            message: value.message,
        }
    }
}

impl From<ProjectionGap> for protocol::ProjectionGap {
    fn from(value: ProjectionGap) -> Self {
        Self {
            reference: value.reference.map(Into::into),
            message: value.message,
        }
    }
}

impl From<protocol::ProjectionRow> for ProjectionRow {
    fn from(value: protocol::ProjectionRow) -> Self {
        Self {
            key: value.key,
            title: value.title,
            summary: value.summary,
            url: value.url,
            status: value.status,
            labels: value.labels,
            sources: value.sources.into_iter().map(Into::into).collect(),
            related: value.related.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ProjectionRow> for protocol::ProjectionRow {
    fn from(value: ProjectionRow) -> Self {
        Self {
            key: value.key,
            title: value.title,
            summary: value.summary,
            url: value.url,
            status: value.status,
            labels: value.labels,
            sources: value.sources.into_iter().map(Into::into).collect(),
            related: value.related.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<protocol::ProjectionInput> for ProjectionInput {
    fn from(value: protocol::ProjectionInput) -> Self {
        Self {
            kind: value.kind.into(),
            freshness: value.freshness.into(),
            availability: value.availability.into(),
            total: value.total.map(|count| count.0),
            rows: value.rows.into_iter().map(Into::into).collect(),
            gaps: value.gaps.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ProjectionInput> for protocol::ProjectionInput {
    fn from(value: ProjectionInput) -> Self {
        Self {
            kind: value.kind.into(),
            freshness: value.freshness.into(),
            availability: value.availability.into(),
            total: value.total.map(ProjectionCount),
            rows: value.rows.into_iter().map(Into::into).collect(),
            gaps: value.gaps.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<protocol::ProjectionSnapshot> for ProjectionSnapshot {
    type Error = Error;

    fn try_from(value: protocol::ProjectionSnapshot) -> Result<Self, Self::Error> {
        value.validate().map_err(error)?;
        Ok(Self {
            version: 1,
            workspace_id: value.workspace_id.0,
            chain: value.chain.0,
            inputs: value.inputs.into_iter().map(Into::into).collect(),
        })
    }
}

impl TryFrom<ProjectionSnapshot> for protocol::ProjectionSnapshot {
    type Error = Error;

    fn try_from(value: ProjectionSnapshot) -> Result<Self, Self::Error> {
        if value.version != 1 {
            return Err(error("Unsupported projection input version"));
        }
        let snapshot = Self {
            version: ApiVersion::V1,
            workspace_id: value.workspace_id.as_str().into(),
            chain: value.chain.as_str().into(),
            inputs: value.inputs.into_iter().map(Into::into).collect(),
        };
        snapshot.validate().map_err(error)?;
        Ok(snapshot)
    }
}
