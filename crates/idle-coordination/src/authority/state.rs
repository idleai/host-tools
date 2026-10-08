use std::collections::{BTreeMap, VecDeque};

use idle_protocol::v1::{
    Record,
    api::{ApiResult, Request},
    configuration::{ConfigurationDocument, ConfigurationValue},
    control::ControlOwnership,
    grants::Grant,
    identity::{ContributorId, ControlEpoch, Revision, StreamId},
    membership::{Membership, MembershipStatus, Role},
    resources::{ComputeHost, ModelProvider},
    sessions::Session,
    standalone::{ChangeNotice, Mutation, MutationResult, ViewDefinition},
    workspace::{CoordinationMode, Workspace},
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::adoption::Handoff;

pub(super) const STATE_KEY: &str = "authority";
pub(super) const MAX_ENTITIES: usize = 4096;
pub(super) const MAX_RECEIPTS: usize = 4096;
pub(super) const MAX_NOTICES: usize = 256;
pub(super) const MAX_DEADLINE_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StoredResult {
    pub request: Request<Mutation>,
    pub result: ApiResult<MutationResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct State {
    pub version: u16,
    pub owner: ContributorId,
    pub workspace: Record<Workspace>,
    pub memberships: BTreeMap<String, Record<Membership>>,
    pub configuration: BTreeMap<ConfigurationDocument, Record<ConfigurationValue>>,
    pub views: BTreeMap<String, Record<ViewDefinition>>,
    pub sessions: BTreeMap<String, Record<Session>>,
    pub hosts: BTreeMap<String, Record<ComputeHost>>,
    pub providers: BTreeMap<String, Record<ModelProvider>>,
    pub grants: BTreeMap<String, Record<Grant>>,
    pub control: ControlOwnership,
    pub sequence: u64,
    pub stream: StreamId,
    pub notices: VecDeque<(u64, ChangeNotice)>,
    pub receipts: Vec<StoredResult>,
    pub clock_floor: u64,
    pub handoff: Option<Handoff>,
    #[serde(default)]
    pub repository_files: Option<Box<crate::workspace_config::Observation>>,
    #[serde(default)]
    pub resource_revisions: super::resources::ResourceRevisions,
}

impl State {
    pub(super) fn new(workspace: Workspace, owner: ContributorId) -> Result<Self> {
        if !matches!(workspace.mode, CoordinationMode::Standalone { .. }) {
            return Err(Error::Invalid);
        }
        let membership = Record {
            revision: Revision(1),
            value: Membership {
                contributor_id: owner.clone(),
                role: Role::Owner,
                status: MembershipStatus::Active,
            },
        };
        let control = ControlOwnership {
            workspace_id: workspace.id.clone(),
            last_epoch: ControlEpoch(0),
            lease: None,
        };
        Ok(Self {
            version: 1,
            owner: owner.clone(),
            workspace: Record {
                revision: Revision(1),
                value: workspace,
            },
            memberships: BTreeMap::from([(owner.0, membership)]),
            configuration: BTreeMap::new(),
            views: BTreeMap::new(),
            sessions: BTreeMap::new(),
            hosts: BTreeMap::new(),
            providers: BTreeMap::new(),
            grants: BTreeMap::new(),
            control,
            sequence: 0,
            stream: StreamId(uuid::Uuid::new_v4().to_string()),
            notices: VecDeque::new(),
            receipts: Vec::new(),
            clock_floor: 0,
            handoff: None,
            repository_files: None,
            resource_revisions: super::resources::ResourceRevisions::default(),
        })
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Version);
        }
        self.validate_binding()?;
        self.resource_revisions.validate()?;
        validate_records(&self.memberships, |value| &value.contributor_id.0)?;
        validate_records(&self.views, |value| &value.id)?;
        validate_records(&self.sessions, |value| &value.id.0)?;
        validate_records(&self.hosts, |value| &value.id.0)?;
        validate_records(&self.providers, |value| &value.id.0)?;
        validate_records(&self.grants, |value| &value.id.0)?;
        for record in self.configuration.values() {
            if record.revision.0 == 0 {
                return Err(Error::Invalid);
            }
            super::validation::configuration(&record.value).map_err(|_error| Error::Invalid)?;
        }
        if self.control.workspace_id != self.workspace.value.id
            || self.workspace.revision.0 == 0
            || self.control.lease.as_ref().is_some_and(|lease| {
                lease.fence.epoch != self.control.last_epoch
                    || lease.fence.workspace_id != self.workspace.value.id
                    || lease.fence.epoch.0 == 0
                    || lease.expires_at <= lease.acquired_at
            })
            || self.receipts.len() > MAX_RECEIPTS
            || self.notices.len() > MAX_NOTICES
            || self.memberships.get(&self.owner.0).is_none_or(|record| {
                record.value.role != Role::Owner || record.value.status != MembershipStatus::Active
            })
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    fn validate_binding(&self) -> Result<()> {
        match &self.workspace.value.mode {
            CoordinationMode::Standalone { .. } => {
                super::validation::workspace(&self.workspace.value)
                    .map_err(|_error| Error::Invalid)?;
                if self
                    .handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.receipt.is_some())
                {
                    return Err(Error::Invalid);
                }
            }
            CoordinationMode::Managed { repositories } => {
                let handoff = self.handoff.as_ref().ok_or(Error::Invalid)?;
                let receipt = handoff.receipt.as_ref().ok_or(Error::Invalid)?;
                handoff.package.validate_receipt(receipt)?;
                let original = &handoff.package.snapshot.workspace.value;
                let CoordinationMode::Standalone { repository } = &original.mode else {
                    return Err(Error::Invalid);
                };
                if repositories != std::slice::from_ref(repository)
                    || self.workspace.value.id != original.id
                    || self.workspace.value.chain != original.chain
                    || self.control.last_epoch != receipt.last_epoch
                    || self.control.lease.is_some()
                {
                    return Err(Error::Invalid);
                }
            }
        }
        if let Some(handoff) = &self.handoff {
            if handoff.package.version != 1
                || handoff.package.snapshot.workspace.value.id != self.workspace.value.id
                || handoff.package.snapshot.workspace.value.chain != self.workspace.value.chain
            {
                return Err(Error::Invalid);
            }
            handoff.package.history.validate()?;
        }
        Ok(())
    }

    pub(super) fn record_change(&mut self, notice: ChangeNotice) -> Result<()> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Invalid)?;
        if notice == ChangeNotice::Access {
            self.stream = StreamId(uuid::Uuid::new_v4().to_string());
        }
        self.notices.push_back((self.sequence, notice));
        while self.notices.len() > MAX_NOTICES {
            let _removed = self.notices.pop_front();
        }
        Ok(())
    }
}

fn validate_records<T>(
    records: &BTreeMap<String, Record<T>>,
    key: impl Fn(&T) -> &str,
) -> Result<()> {
    if records.len() > MAX_ENTITIES
        || records.iter().any(|(id, record)| {
            record.revision.0 == 0 || id != key(&record.value) || super::validation::id(id).is_err()
        })
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
