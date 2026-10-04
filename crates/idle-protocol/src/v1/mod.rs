//! Version 1 of the coordination JSON protocol.
//!
//! Every top-level [`api::Request`], [`api::Response`] and [`events::Event`]
//! carries [`ApiVersion`]. Unknown versions and enum variants must be rejected;
//! clients must not advance a recovery cursor past an undecodable event.

pub mod api;
pub mod configuration;
pub mod control;
pub mod events;
pub mod grants;
pub mod identity;
pub mod membership;
pub mod projections;
pub mod repository;
pub mod resources;
pub mod sessions;
pub mod standalone;
pub mod workspace;

use serde::{Deserialize, Serialize};

/// Required protocol discriminator, independent of the Cargo package version.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ApiVersion {
    /// The v1 JSON representation.
    #[serde(rename = "1")]
    V1,
}

/// A metadata value and its authority-assigned revision.
///
/// Revisions increase for a given entity, including deletion/recreation. They
/// are neither runtime input revisions nor event positions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Record<T> {
    /// Revision used for optimistic concurrency.
    pub revision: identity::Revision,
    /// Current entity data in the enclosing workspace scope.
    pub value: T,
}

/// Required precondition for a metadata write; there is no blind overwrite.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum WriteCondition {
    /// Create only if the entity has never existed in this scope.
    Absent,
    /// Replace only the stated current revision.
    Revision(identity::Revision),
}

/// Proposed metadata change; the provider assigns the resulting revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Change<T> {
    /// Concurrency precondition checked atomically with the write.
    pub expected: WriteCondition,
    /// Proposed value, subject to authorization and domain validation.
    pub value: T,
}
