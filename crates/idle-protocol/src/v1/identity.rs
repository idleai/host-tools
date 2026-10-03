//! Opaque identities, lossless counters and authenticated request attribution.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

macro_rules! identifiers {
    ($($(#[$meta:meta])* $name:ident;)+) => {
        $(
            $(#[$meta])*
            #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
            #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
            #[serde(transparent)]
            pub struct $name(pub String);

            impl From<&str> for $name {
                fn from(value: &str) -> Self {
                    Self(value.to_owned())
                }
            }
        )+
    };
}

identifiers! {
    /// Stable logical workspace identity, preserved when adopting managed coordination.
    WorkspaceId;
    /// Opaque engine-issued logical chain reference; not a storage URL or workspace ID.
    ChainRef;
    /// Repository identity, reusable across workspace attachments.
    RepositoryId;
    /// Actual human or service contributor; distinct from a session or host owner.
    ContributorId;
    /// Workspace invitation identity.
    InvitationId;
    /// Reconnectable session identity, preserved across provider changes.
    SessionId;
    /// Compute host identity, reusable across workspace bindings.
    HostId;
    /// Model provider identity, reusable across workspace bindings.
    ProviderId;
    /// Published model identity within its provider.
    ModelId;
    /// Runtime instance identity authenticated by its host adapter.
    RuntimeId;
    /// Workspace-scoped grant identity, never reused after revocation.
    GrantId;
    /// Caller-generated retry identity; reuse only for the same logical request.
    RequestId;
    /// Stable event identity retained across duplicate delivery.
    EventId;
    /// Recovery stream generation, changed after resets or visibility changes.
    StreamId;
}

/// Invalid non-canonical or out-of-range unsigned decimal wire value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidDecimal {
    /// Input is not an unsigned 64-bit integer; retains the parse error.
    InvalidInteger(std::num::ParseIntError),
    /// Integer text includes padding, signs or other non-canonical spelling.
    NonCanonical,
}

impl fmt::Display for InvalidDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a canonical unsigned 64-bit decimal string")
    }
}

impl Error for InvalidDecimal {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidInteger(source) => Some(source),
            Self::NonCanonical => None,
        }
    }
}

fn parse_decimal(value: &str) -> Result<u64, InvalidDecimal> {
    let number = value
        .parse::<u64>()
        .map_err(InvalidDecimal::InvalidInteger)?;
    if number.to_string() != value {
        return Err(InvalidDecimal::NonCanonical);
    }
    Ok(number)
}

#[cfg(feature = "schema")]
fn decimal_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // Shorter decimals plus each prefix below u64::MAX, then the maximum itself.
    // The additional pattern rejects trailing newlines even in regex engines
    // where `$` matches immediately before a final line terminator.
    schemars::json_schema!({
        "type": "string",
        "pattern": concat!(
            "^(0|[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|",
            "184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|",
            "184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|",
            "1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|",
            "1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|",
            "184467440737095[0-4][0-9]{4}|18446744073709550[0-9]{3}|",
            "18446744073709551[0-5][0-9]{2}|1844674407370955160[0-9]|",
            "1844674407370955161[0-4]|18446744073709551615)$"
        ),
        "not": {"pattern": "[^0-9]"}
    })
}

macro_rules! counters {
    ($($(#[$meta:meta])* $name:ident;)+) => {
        $(
            $(#[$meta])*
            #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
            #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
            #[cfg_attr(feature = "schema", schemars(schema_with = "decimal_schema"))]
            #[serde(try_from = "String", into = "String")]
            pub struct $name(pub u64);

            impl TryFrom<String> for $name {
                type Error = InvalidDecimal;

                fn try_from(value: String) -> Result<Self, Self::Error> {
                    parse_decimal(&value).map(Self)
                }
            }

            impl From<$name> for String {
                fn from(value: $name) -> Self {
                    value.0.to_string()
                }
            }
        )+
    };
}

counters! {
    /// Metadata revision assigned by the coordination authority.
    Revision;
    /// Monotonic fencing epoch; zero means ownership has never been assigned.
    ControlEpoch;
    /// Durable event position; zero is the empty-stream snapshot boundary.
    EventPosition;
    /// Runtime-assigned session input order, starting at one and never reset.
    InputOrder;
    /// Runtime-assigned per-input state revision, starting at one and never reset.
    InputRevision;
    /// Milliseconds since the Unix epoch; clocks do not define event/input ordering.
    Timestamp;
    /// Provider-supplied count of rows in a projection, independent of paging.
    ProjectionCount;
}

/// Identity in an external authentication namespace, with no credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExternalIdentity {
    /// Stable issuer namespace, such as a local peer-key authority or an SSO issuer.
    pub issuer: String,
    /// Immutable subject at that issuer, not a mutable display name.
    pub subject: String,
}

/// Public contributor directory entry, scoped by the enclosing workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Contributor {
    /// Stable contributor identity.
    pub id: ContributorId,
    /// Presentation label, never an authorization key.
    pub display_name: String,
    /// Verified identity links visible to the requesting audience.
    pub identities: Vec<ExternalIdentity>,
}

/// Attribution resolved by an authenticated adapter, not proof of authentication.
///
/// A receiving authority MUST verify these fields against the authenticated
/// connection. Forwarders preserve the contributor instead of substituting the
/// session owner, host owner or relay's service identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContributorIdentity {
    /// Contributor to whom this request is attributed.
    pub contributor_id: ContributorId,
    /// Verified identity used for this submission; no token material.
    pub authenticated_as: ExternalIdentity,
}

/// Complete deduplication key, shared by coordination and runtime adapters.
///
/// A key covers all mutation kinds, not just one endpoint. Changing the payload,
/// deadline or authenticated subject under an existing key is a conflict.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestKey {
    /// Isolated workspace scope.
    pub workspace_id: WorkspaceId,
    /// Actual authenticated contributor.
    pub contributor_id: ContributorId,
    /// Stable identity of the logical mutation, retained on every retry.
    pub request_id: RequestId,
}

/// Request correlation and attribution, unchanged across retries and forwarding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestContext {
    /// Workspace to which all payload references are bound.
    pub workspace_id: WorkspaceId,
    /// Caller-generated identity created before the first submission.
    pub request_id: RequestId,
    /// Authenticated contributor, independent of resource ownership.
    pub contributor: ContributorIdentity,
    /// Deadline for first coordination receipt, using the authority's clock.
    /// Receipt before this deadline permits later runtime delivery and execution.
    pub expires_at: Timestamp,
}

impl RequestContext {
    /// Extract the scope in which a retry must resolve to the original outcome.
    #[must_use]
    pub fn key(&self) -> RequestKey {
        RequestKey {
            workspace_id: self.workspace_id.clone(),
            contributor_id: self.contributor.contributor_id.clone(),
            request_id: self.request_id.clone(),
        }
    }
}
