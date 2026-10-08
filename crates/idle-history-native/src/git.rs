//! Read-only Git objects from the checkout explicitly installed by the host.

mod command;

use std::{collections::BTreeMap, io, path::PathBuf};

use editchain_git::{RefSnapshot, RepositoryDiscovery, RepositoryHandle, open_repository};
use idle_history::{query::QueryResult, timeline::Target};
use serde::{Deserialize, Serialize};

use crate::history::service::Binding;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Observation {
    pub root: PathBuf,
    pub repository: Option<u64>,
    pub head: Option<String>,
    pub labels: BTreeMap<String, Vec<String>>,
    pub gap: Option<String>,
}

pub(crate) fn open(root: &std::path::Path) -> io::Result<RepositoryHandle> {
    let discovery = RepositoryDiscovery::from_path(root)
        .map_err(|error| io::Error::other(error.to_string()))?;
    open_repository(&discovery).map_err(|error| io::Error::other(error.to_string()))
}

pub(crate) fn observe(binding: &Binding) -> Option<Observation> {
    let root = binding.repository_directory.as_ref()?;
    let mut result = Observation {
        root: root.clone(),
        repository: None,
        head: None,
        labels: BTreeMap::new(),
        gap: None,
    };
    if let Err(error) = capture(&mut result) {
        result.gap = Some(format!("Git history is unavailable: {error}"));
    }
    Some(result)
}

fn capture(result: &mut Observation) -> io::Result<()> {
    let handle = open(&result.root)?;
    result.repository = Some(handle.discovery.id.0);
    result.head = handle
        .repo
        .head()
        .map_err(io::Error::other)?
        .id()
        .map(|id| id.to_string());
    for (oid, names) in RefSnapshot::capture(&handle)
        .map_err(io::Error::other)?
        .entries()
    {
        let labels = names
            .iter()
            .map(|name| {
                let name = String::from_utf8_lossy(name);
                name.strip_prefix("refs/heads/")
                    .or_else(|| name.strip_prefix("refs/tags/"))
                    .unwrap_or(&name)
                    .to_owned()
            })
            .collect();
        drop(result.labels.insert(oid.to_string(), labels));
    }
    if let Some(head) = &result.head {
        result
            .labels
            .entry(head.clone())
            .or_default()
            .insert(0, "HEAD".into());
    }
    Ok(())
}

pub(crate) fn document(binding: &Binding, repository: &str, oid: &str) -> io::Result<QueryResult> {
    if !(Target::Commit {
        repository: repository.into(),
        oid: oid.into(),
    })
    .is_valid()
    {
        return Err(io::Error::other("Invalid commit destination."));
    }
    let root = binding
        .repository_directory
        .as_ref()
        .ok_or_else(|| io::Error::other("This history connection has no bound Git checkout."))?;
    let handle = open(root)?;
    if handle.discovery.id.0.to_string() != repository {
        return Err(io::Error::other(
            "The commit belongs to a different repository.",
        ));
    }
    let object = editchain_core::GitOid::from_hex(oid)
        .ok_or_else(|| io::Error::other("Invalid commit hash."))?;
    let _commit = editchain_git::resolve_commit(&handle, &object).map_err(io::Error::other)?;
    let content = command::show(root, oid)?;
    Ok(QueryResult::Commit {
        repository: repository.into(),
        oid: oid.into(),
        content,
    })
}
