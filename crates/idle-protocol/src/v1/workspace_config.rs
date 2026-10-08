//! Portable authored workspace definitions stored under `.idle/workspace`.
//!
//! These documents declare configuration, never live authorization or availability.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    identity::{HostId, ProviderId, WorkspaceId},
    projections::ProjectionKind,
    resources::ConnectionRoute,
    standalone::ViewDefinition,
};

/// Tracked workspace identity, independent of a checkout's local history binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspaceManifest {
    /// Document family version. This implementation supports one.
    pub schema_version: u32,
    /// Stable identity copied with the repository; never an access credential.
    pub id: WorkspaceId,
    /// Human-readable workspace name.
    pub name: String,
    /// Preserve fields written by newer compatible editors.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// A configured host, independent of a daemon's authenticated publication.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HostDefinition {
    /// Intended host identity.
    pub id: HostId,
    /// Display name.
    pub name: String,
    /// Public lookup references; credentials remain in the connecting host.
    #[serde(default)]
    pub routes: Vec<ConnectionRoute>,
}

/// An external or host-served provider declaration, without credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProviderDefinition {
    /// Intended provider identity.
    pub id: ProviderId,
    /// Display name.
    pub name: String,
    /// Serving host for a local provider; absent for external providers.
    #[serde(default)]
    pub host_id: Option<HostId>,
    /// Name resolved by the local credential adapter, never a secret value.
    #[serde(default)]
    pub credential_ref: Option<String>,
    /// Public lookup references, without authentication material.
    #[serde(default)]
    pub routes: Vec<ConnectionRoute>,
}

/// Contents of `hosts.json`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HostDefinitions {
    /// Configured hosts; duplicate identities are invalid.
    pub hosts: Vec<HostDefinition>,
}

/// Contents of `providers.json`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProviderDefinitions {
    /// Configured providers; duplicate identities are invalid.
    pub providers: Vec<ProviderDefinition>,
}

/// Human-editable contents of `projections/<id>.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectionDefinition {
    /// Definition format version.
    pub schema_version: u32,
    /// Stable identity, matching the filename.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Supplied projection destination.
    pub kind: ProjectionKind,
    /// Query, filters and layout; retain all fields through compatible edits.
    pub definition: serde_json::Map<String, serde_json::Value>,
    /// Additional compatible document fields.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Validated authored configuration returned by a standalone host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspaceConfiguration {
    /// Tracked identity and schema version.
    pub manifest: WorkspaceManifest,
    /// Content digest identifying this exact set of files, including formatting.
    pub revision: String,
    /// Complete settings JSON object, if authored.
    pub settings: Option<String>,
    /// Complete agent-rules JSON object, if authored.
    pub agent_rules: Option<String>,
    /// Controller configuration JSON object; execution belongs to Evo.
    pub control: Option<String>,
    /// Host declarations; these do not confer compute access.
    pub hosts: HostDefinitions,
    /// Provider declarations; these do not establish serving health.
    pub providers: ProviderDefinitions,
    /// Saved definitions; computed projection results remain rebuildable.
    pub projections: Vec<ViewDefinition>,
}

#[cfg(all(test, feature = "schema"))]
mod tests {
    use super::{HostDefinitions, ProjectionDefinition, ProviderDefinitions, WorkspaceManifest};

    #[test]
    fn workspace_file_schemas_match_the_exported_contracts() {
        let published: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/workspace-config-v1.json"))
                .expect("published workspace schemas are JSON");
        let current = serde_json::json!({
            "workspace.json": schemars::schema_for!(WorkspaceManifest),
            "hosts.json": schemars::schema_for!(HostDefinitions),
            "providers.json": schemars::schema_for!(ProviderDefinitions),
            "projections/<id>.json": schemars::schema_for!(ProjectionDefinition),
        });
        assert_eq!(
            published, current,
            "regenerate reviewed workspace schemas after contract changes"
        );
    }
}
