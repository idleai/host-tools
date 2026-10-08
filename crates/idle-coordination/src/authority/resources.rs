use std::collections::{BTreeMap, BTreeSet};

use idle_protocol::v1::{
    Record,
    identity::{Revision, Timestamp},
    resources::{Availability, ComputeHost, Health, ModelProvider, ProviderKind},
    standalone::RepositorySnapshot,
    workspace_config::WorkspaceConfiguration,
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::state::{MAX_ENTITIES, State};

/// Revision history shared by each authored resource and its runtime publication.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ResourceRevisions {
    pub hosts: BTreeMap<String, Revision>,
    pub providers: BTreeMap<String, Revision>,
}

impl ResourceRevisions {
    pub(super) fn validate(&self) -> Result<()> {
        for revisions in [&self.hosts, &self.providers] {
            if revisions.len() > MAX_ENTITIES
                || revisions.iter().any(|(id, revision)| {
                    revision.0 == 0 || revision.0 == u64::MAX || super::validation::id(id).is_err()
                })
            {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }

    pub(super) fn initialized(&self, config: &WorkspaceConfiguration) -> bool {
        config
            .hosts
            .hosts
            .iter()
            .all(|host| self.hosts.contains_key(&host.id.0))
            && config
                .providers
                .providers
                .iter()
                .all(|provider| self.providers.contains_key(&provider.id.0))
    }
}

impl State {
    pub(super) fn advance_host_revision(&mut self, id: &str) -> Result<Revision> {
        advance(
            &mut self.resource_revisions.hosts,
            &mut self.hosts,
            id,
            self.sequence,
        )
    }

    pub(super) fn advance_provider_revision(&mut self, id: &str) -> Result<Revision> {
        advance(
            &mut self.resource_revisions.providers,
            &mut self.providers,
            id,
            self.sequence,
        )
    }

    pub(super) fn sync_resources(&mut self, config: &WorkspaceConfiguration) -> Result<()> {
        let previous = self
            .repository_files
            .as_ref()
            .map(|files| &files.configuration);
        let hosts = changed(
            previous.map_or(&[], |config| config.hosts.hosts.as_slice()),
            &config.hosts.hosts,
            |host| &host.id.0,
            &self.resource_revisions.hosts,
        );
        let providers = changed(
            previous.map_or(&[], |config| config.providers.providers.as_slice()),
            &config.providers.providers,
            |provider| &provider.id.0,
            &self.resource_revisions.providers,
        );
        for id in hosts {
            let _revision = self.advance_host_revision(&id)?;
        }
        for id in providers {
            let _revision = self.advance_provider_revision(&id)?;
        }
        Ok(())
    }

    pub(super) fn advance_resource_access(&mut self) -> Result<()> {
        let Some(files) = &self.repository_files else {
            return Ok(());
        };
        // Different audiences can switch between the declaration and publication
        // when grants or memberships change. Both representations advance together.
        let hosts: Vec<_> = files
            .configuration
            .hosts
            .hosts
            .iter()
            .filter(|host| self.hosts.contains_key(&host.id.0))
            .map(|host| host.id.0.clone())
            .collect();
        let providers: Vec<_> = files
            .configuration
            .providers
            .providers
            .iter()
            .filter(|provider| self.providers.contains_key(&provider.id.0))
            .map(|provider| provider.id.0.clone())
            .collect();
        for id in hosts {
            let _revision = self.advance_host_revision(&id)?;
        }
        for id in providers {
            let _revision = self.advance_provider_revision(&id)?;
        }
        Ok(())
    }

    pub(super) fn configured_resources(
        &self,
        snapshot: &mut RepositorySnapshot,
        config: &WorkspaceConfiguration,
    ) -> Result<()> {
        let health = Health {
            availability: Availability::Unknown,
            observed_at: Timestamp(0),
            valid_until: Timestamp(1),
        };
        // Definitions never enter authority grant tables or authorize runtime operations.
        let owner = format!("configured:{}", config.manifest.id.0);
        for host in &config.hosts.hosts {
            if snapshot
                .hosts
                .iter()
                .any(|record| record.value.id == host.id)
            {
                continue;
            }
            snapshot.hosts.push(Record {
                revision: configured_revision(
                    &self.resource_revisions.hosts,
                    &host.id.0,
                    self.hosts.contains_key(&host.id.0),
                )?,
                value: ComputeHost {
                    id: host.id.clone(),
                    name: host.name.clone(),
                    owner: owner.as_str().into(),
                    capabilities: Vec::new(),
                    health: health.clone(),
                    routes: host.routes.clone(),
                },
            });
        }
        for provider in &config.providers.providers {
            if snapshot
                .providers
                .iter()
                .any(|record| record.value.id == provider.id)
            {
                continue;
            }
            snapshot.providers.push(Record {
                revision: configured_revision(
                    &self.resource_revisions.providers,
                    &provider.id.0,
                    self.providers.contains_key(&provider.id.0),
                )?,
                value: ModelProvider {
                    id: provider.id.clone(),
                    name: provider.name.clone(),
                    owner: owner.as_str().into(),
                    kind: provider
                        .host_id
                        .as_ref()
                        .map_or(ProviderKind::External, |host| ProviderKind::Local {
                            host_id: host.clone(),
                            runtime_id: "unconnected".into(),
                        }),
                    health: health.clone(),
                    routes: provider.routes.clone(),
                },
            });
        }
        Ok(())
    }
}

fn advance<T>(
    revisions: &mut BTreeMap<String, Revision>,
    records: &mut BTreeMap<String, Record<T>>,
    id: &str,
    sequence: u64,
) -> Result<Revision> {
    if !revisions.contains_key(id) && revisions.len() >= MAX_ENTITIES {
        return Err(Error::Invalid);
    }
    let previous = revisions
        .get(id)
        .map_or(0, |revision| revision.0)
        .max(records.get(id).map_or(0, |record| record.revision.0))
        .max(sequence);
    // Reserve the next revision for a declaration shown after access expires.
    // A subsequent publication or grant change must advance past that value.
    let revision = Revision(
        previous
            .checked_add(2)
            .filter(|next| *next < u64::MAX)
            .ok_or(Error::Invalid)?,
    );
    let _old = revisions.insert(id.to_owned(), revision);
    if let Some(record) = records.get_mut(id) {
        record.revision = revision;
    }
    Ok(revision)
}

fn configured_revision(
    revisions: &BTreeMap<String, Revision>,
    id: &str,
    published: bool,
) -> Result<Revision> {
    let revision = revisions.get(id).ok_or(Error::Invalid)?.0;
    revision
        .checked_add(u64::from(published))
        .map(Revision)
        .ok_or(Error::Invalid)
}

fn changed<T: PartialEq>(
    before: &[T],
    after: &[T],
    id: fn(&T) -> &str,
    revisions: &BTreeMap<String, Revision>,
) -> Vec<String> {
    let before: BTreeMap<_, _> = before.iter().map(|value| (id(value), value)).collect();
    let after: BTreeMap<_, _> = after.iter().map(|value| (id(value), value)).collect();
    let ids: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    ids.into_iter()
        .filter(|id| !revisions.contains_key(*id) || before.get(id) != after.get(id))
        .map(str::to_owned)
        .collect()
}
