use std::collections::{BTreeMap, BTreeSet};

use idle_protocol::v1::{
    standalone::ViewDefinition,
    workspace_config::{
        HostDefinitions, ProjectionDefinition, ProviderDefinitions, WorkspaceConfiguration,
        WorkspaceManifest,
    },
};
use serde::de::DeserializeOwned;

use crate::{Error, Result};

pub(super) fn decode(documents: &BTreeMap<String, String>) -> Result<WorkspaceConfiguration> {
    let manifest: WorkspaceManifest =
        serde_json::from_str(documents.get("workspace.json").ok_or(Error::Invalid)?)?;
    if manifest.schema_version != 1 {
        return Err(Error::Version);
    }
    id(&manifest.id.0)?;
    label(&manifest.name)?;
    let settings = object(documents.get("settings.json"))?;
    let agent_rules = object(documents.get("agent-rules.json"))?;
    let control = object(documents.get("control.json"))?;
    let hosts: HostDefinitions = optional(documents.get("hosts.json"))?;
    let providers: ProviderDefinitions = optional(documents.get("providers.json"))?;
    let mut ids = BTreeSet::new();
    for host in &hosts.hosts {
        id(&host.id.0)?;
        label(&host.name)?;
        routes(&host.routes)?;
        if !ids.insert(&host.id.0) {
            return Err(Error::Invalid);
        }
    }
    ids.clear();
    for provider in &providers.providers {
        id(&provider.id.0)?;
        label(&provider.name)?;
        routes(&provider.routes)?;
        if !ids.insert(&provider.id.0) {
            return Err(Error::Invalid);
        }
        if let Some(reference) = &provider.credential_ref {
            label(reference)?;
        }
        if let Some(host) = &provider.host_id {
            id(&host.0)?;
        }
    }
    let mut projections = Vec::new();
    let mut digest = blake3::Hasher::new();
    for (name, content) in documents {
        let _digest = digest
            .update(name.as_bytes())
            .update(&[0])
            .update(content.as_bytes())
            .update(&[0]);
        if name.starts_with("projections/") {
            let view: ProjectionDefinition = serde_json::from_str(content)?;
            if view.schema_version != 1 {
                return Err(Error::Version);
            }
            if projection_path(&view.id)? != *name {
                return Err(Error::Invalid);
            }
            label(&view.title)?;
            let json = serde_json::to_string(&view.definition)?;
            let _validated = object(Some(&json))?;
            projections.push(ViewDefinition {
                id: view.id,
                title: view.title,
                kind: view.kind,
                schema_version: view.schema_version,
                json,
            });
        }
    }
    Ok(WorkspaceConfiguration {
        manifest,
        revision: digest.finalize().to_hex().to_string(),
        settings,
        agent_rules,
        control,
        hosts,
        providers,
        projections,
    })
}

pub(crate) fn projection_document(
    view: &ViewDefinition,
    previous: Option<&String>,
) -> Result<String> {
    let extensions = previous
        .map(|text| serde_json::from_str::<ProjectionDefinition>(text))
        .transpose()?
        .map_or_else(BTreeMap::new, |view| view.extensions);
    super::pretty(&ProjectionDefinition {
        schema_version: view.schema_version,
        id: view.id.clone(),
        title: view.title.clone(),
        kind: view.kind,
        definition: serde_json::from_str(&view.json)?,
        extensions,
    })
}

pub(crate) fn projection_path(id: &str) -> Result<String> {
    self::id(id)?;
    let stem = id.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && suffix.bytes().all(|c| (b'1'..=b'9').contains(&c))
            })
        });
    let portable = !reserved
        && id.len() <= 128
        && !id.starts_with('.')
        && id.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.')
        });
    let name = if portable {
        id.to_owned()
    } else {
        format!("~{}", blake3::hash(id.as_bytes()))
    };
    Ok(format!("projections/{name}.json"))
}

fn object(text: Option<&String>) -> Result<Option<String>> {
    if let Some(text) = text
        && (text.len() > 256 * 1024
            || !serde_json::from_str::<serde_json::Value>(text)?.is_object())
    {
        return Err(Error::Invalid);
    }
    Ok(text.cloned())
}

fn optional<T: DeserializeOwned + Default>(text: Option<&String>) -> Result<T> {
    text.map(|text| serde_json::from_str(text).map_err(Error::from))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn label(value: &str) -> Result<()> {
    crate::authority::validation::label(value).map_err(|_error| Error::Invalid)
}

fn id(value: &str) -> Result<()> {
    crate::authority::validation::id(value).map_err(|_error| Error::Invalid)
}

fn routes(routes: &[idle_protocol::v1::resources::ConnectionRoute]) -> Result<()> {
    if routes.len() > 32 {
        return Err(Error::Invalid);
    }
    for route in routes {
        label(&route.protocol)?;
        label(&route.reference)?;
    }
    Ok(())
}
