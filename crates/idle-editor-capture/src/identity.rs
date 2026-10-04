//! Full observation and logical identities use separate namespaces.

use editchain_core::{OpId, PathId, activity::ItemId};

use crate::wire::{EditorDocument, EditorEvent};

pub(crate) fn operation(session: &str, sequence: u64, lane: &str) -> OpId {
    let mut hash = blake3::Hasher::new_derive_key("idle.vscode.observation.v1");
    let _hash = hash.update(session.as_bytes());
    let _hash = hash.update(&sequence.to_le_bytes());
    let _hash = hash.update(lane.as_bytes());
    OpId::from_bytes(*hash.finalize().as_bytes())
}

pub(crate) fn event(event: &EditorEvent, lane: &str) -> OpId {
    operation(&event.session, event.sequence, lane)
}

pub(crate) fn session(event: &EditorEvent) -> ItemId {
    ItemId::derive("idle.vscode.session.v1", event.session.as_bytes())
}

pub(crate) fn recorder(event: &EditorEvent) -> ItemId {
    ItemId::derive("idle.vscode.recorder.v1", event.session.as_bytes())
}

pub(crate) fn person(event: &EditorEvent) -> Option<ItemId> {
    event
        .identity
        .as_ref()
        .map(|identity| ItemId::derive("idle.vscode.unsigned-person.v1", identity.guid.as_bytes()))
}

pub(crate) fn revision(event: &EditorEvent, document: &EditorDocument, version: u64) -> ItemId {
    revision_id(&event.session, &document.id, version)
}

/// Identity of one recorded editor buffer occurrence, independent of its bytes.
#[must_use]
pub fn revision_id(session: &str, document: &str, version: u64) -> ItemId {
    ItemId::derive(
        "idle.vscode.revision.v1",
        format!("{session}\0{document}\0{version}").as_bytes(),
    )
}

pub(crate) fn path(name: &str) -> PathId {
    let hash = blake3::hash(name.as_bytes());
    let mut bytes = [0; 8];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes()) {
        *target = *source;
    }
    PathId(u64::from_le_bytes(bytes))
}
