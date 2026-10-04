//! Rebuildable revision and Git-context lookup, independent of hash ordering.

use editchain_core::{
    ContentId, OpId,
    activity::{FileAction, ItemId, Kind, Operation},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Revision {
    pub(crate) content: ContentId,
    pub(crate) operation: OpId,
    sequence: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    revisions: BTreeMap<ItemId, Revision>,
    contexts: BTreeMap<ItemId, BTreeMap<u64, OpId>>,
    changes: BTreeMap<OpId, ItemId>,
    dwell: BTreeMap<ItemId, u64>,
    disputed: BTreeSet<ItemId>,
}

impl State {
    pub(crate) fn mark_disputed(&mut self, recorder: ItemId) {
        let _inserted = self.disputed.insert(recorder);
    }

    pub(crate) fn disputed(&self, recorder: ItemId) -> bool {
        self.disputed.contains(&recorder)
    }

    pub(crate) fn needs_recovery(&self) -> bool {
        !self.disputed.is_empty()
    }

    pub(crate) fn apply(&mut self, operation: &Operation) {
        if let Kind::Session(session) = &operation.kind
            && session.action == editchain_core::activity::SessionAction::Started
            && let editchain_core::Payload::Inline(settings) = &session.settings
            && let Ok(settings) = serde_json::from_slice::<serde_json::Value>(settings)
            && let Some(dwell) = settings.get("dwell_ms").and_then(serde_json::Value::as_u64)
        {
            let _old = self.dwell.insert(operation.item, dwell);
        }
        if let Kind::File(file) = &operation.kind {
            if file.action == FileAction::Change {
                let _old = self.changes.insert(operation.id, operation.item);
            }
            if matches!(file.action, FileAction::Snapshot | FileAction::Change)
                && let (Some(item), Some(content), Some(sequence)) =
                    (file.revision, file.after, operation.sequence)
            {
                let revision = Revision {
                    content,
                    operation: operation.id,
                    sequence,
                };
                let entry = self.revisions.entry(item).or_insert(revision);
                if sequence < entry.sequence {
                    *entry = revision;
                }
            }
        }
        if let Kind::Note(note) = &operation.kind
            && matches!(&note.code, editchain_core::Payload::Inline(bytes) if bytes == b"idle.editor.git-context")
            && let (Some(session), Some(sequence)) = (operation.session, operation.sequence)
        {
            let _old = self
                .contexts
                .entry(session)
                .or_default()
                .insert(sequence, operation.id);
        }
    }

    pub(crate) fn revision(&self, item: ItemId) -> Option<Revision> {
        self.revisions.get(&item).copied()
    }

    pub(crate) fn change(&self, operation: OpId) -> Option<ItemId> {
        self.changes.get(&operation).copied()
    }

    pub(crate) fn dwell(&self, session: ItemId) -> Option<u64> {
        self.dwell.get(&session).copied()
    }

    pub(crate) fn context(&self, session: ItemId, sequence: u64) -> Option<OpId> {
        self.contexts
            .get(&session)?
            .range(..sequence)
            .next_back()
            .map(|(_, id)| *id)
    }
}
