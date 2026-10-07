//! Export schemas for the authored workspace files.

use std::{error::Error, fs, io};

use idle_protocol::v1::workspace_config::{
    HostDefinitions, ProjectionDefinition, ProviderDefinitions, WorkspaceManifest,
};
use serde as _;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected a schema output path")
    })?;
    let schemas = serde_json::json!({
        "workspace.json": schemars::schema_for!(WorkspaceManifest),
        "hosts.json": schemars::schema_for!(HostDefinitions),
        "providers.json": schemars::schema_for!(ProviderDefinitions),
        "projections/<id>.json": schemars::schema_for!(ProjectionDefinition),
    });
    fs::write(path, serde_json::to_string_pretty(&schemas)?)?;
    Ok(())
}

#[cfg(feature = "reflection")]
use facet as _;
