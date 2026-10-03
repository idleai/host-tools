//! Conditional settings/rules writes, compatible with the f27 editor adapter.

use serde::{Deserialize, Serialize};

use super::Change;

/// Independent settings and agent-rules revision scopes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ConfigurationDocument {
    /// General repository/workspace settings.
    Settings,
    /// Rules interpreted and enforced by Evo.
    AgentRules,
}

/// Complete configuration text, retaining fields unknown to a client.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigurationValue {
    /// Document format; version one contains a JSON object.
    pub schema_version: u32,
    /// Exact JSON text; coordination never interprets runtime policy fields.
    pub json: String,
}

/// Provider-neutral body already emitted by the f27 configuration adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigurationWrite {
    /// Independent revision scope.
    pub document: ConfigurationDocument,
    /// Explicit create-if-absent or revision precondition.
    pub change: Change<ConfigurationValue>,
}
