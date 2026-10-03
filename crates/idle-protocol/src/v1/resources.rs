//! Workspace-bound discovery of shared hosts, providers and served models.

use serde::{Deserialize, Serialize};

use super::{
    Change,
    identity::{ContributorId, HostId, ModelId, ProviderId, Revision, RuntimeId, Timestamp},
};

/// Observed reachability, not permission to use a resource.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// No recent authoritative observation.
    Unknown,
    /// Recently reported healthy.
    Available,
    /// Temporarily unreachable; identities and running sessions remain intact.
    Unavailable,
}

/// Resource health with an explicit freshness limit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Health {
    /// Last observed state.
    pub availability: Availability,
    /// Producer's observation time.
    pub observed_at: Timestamp,
    /// After this time clients display unknown/unavailable, not healthy.
    pub valid_until: Timestamp,
}

/// Discovery reference resolved through an authenticated transport adapter.
///
/// This carries neither credentials nor a cloud-specific tunnel/relay type.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConnectionRoute {
    /// Adapter protocol identifier, negotiated by consumers.
    pub protocol: String,
    /// Opaque, non-secret lookup reference; not a bearer capability.
    pub reference: String,
}

/// Advertised host capability; grants and runtime policy still gate use.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    /// Hosts agent sessions.
    Sessions,
    /// Supports remote file operations.
    Files,
    /// Supports remote process operations.
    Processes,
    /// Can install and serve models.
    LocalModels,
}

/// A host's publication in this workspace; other workspaces need separate bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComputeHost {
    /// Reusable host identity.
    pub id: HostId,
    /// Owner/provisioning contributor, distinct from connected users.
    pub owner: ContributorId,
    /// Display label.
    pub name: String,
    /// Advertised capabilities.
    pub capabilities: Vec<HostCapability>,
    /// Observed availability.
    pub health: Health,
    /// Authorized discovery references.
    pub routes: Vec<ConnectionRoute>,
}

/// External API or Evo-served local model source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderKind {
    /// External provider; credential handling belongs to its host adapter.
    External,
    /// Local serving has no dependency on managed or external-provider sign-in.
    Local {
        /// Serving compute host.
        host_id: HostId,
        /// Runtime publishing and serving the models.
        runtime_id: RuntimeId,
    },
}

/// Provider metadata visible through one workspace binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelProvider {
    /// Reusable provider identity.
    pub id: ProviderId,
    /// Actual provider owner.
    pub owner: ContributorId,
    /// Display label.
    pub name: String,
    /// External or local serving source.
    pub kind: ProviderKind,
    /// Observed availability.
    pub health: Health,
    /// Credential-free routing references.
    pub routes: Vec<ConnectionRoute>,
}

/// Published model capability, not a provider SDK type.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    /// Text generation.
    Text,
    /// Tool calling.
    Tools,
    /// Image input.
    Images,
}

/// Model publication; installation and inference remain runtime operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Model {
    /// Model identity within `provider_id`.
    pub id: ModelId,
    /// Provider exposing this model in the enclosing workspace.
    pub provider_id: ProviderId,
    /// Display label.
    pub name: String,
    /// Advertised modalities and actions.
    pub capabilities: Vec<ModelCapability>,
    /// Observed serving health.
    pub health: Health,
}

/// Exact resource binding being addressed; never an implicit global lookup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceRef {
    /// Compute host binding.
    Host {
        /// Host identity.
        host_id: HostId,
    },
    /// Provider binding.
    Provider {
        /// Provider identity.
        provider_id: ProviderId,
    },
    /// Model publication binding.
    Model {
        /// Publishing provider.
        provider_id: ProviderId,
        /// Model identity within the provider.
        model_id: ModelId,
    },
}

/// Register/update publications or remove a workspace binding only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ResourceCommand {
    /// Register or update a compute host publication.
    PutHost(Change<ComputeHost>),
    /// Register or update a provider publication.
    PutProvider(Change<ModelProvider>),
    /// Publish model capabilities/health from an authenticated runtime/provider.
    PutModel(Change<Model>),
    /// Detach from this workspace without deleting the shared resource elsewhere.
    Detach {
        /// Binding to detach.
        resource: ResourceRef,
        /// Required current binding revision.
        expected_revision: Revision,
    },
}
