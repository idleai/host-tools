//! Late input receipts target the exact earlier file observations.

use crate::{
    identity,
    state::State,
    wire::{EditorEvent, EditorEventKind},
};
use editchain_core::{
    Payload,
    activity::{ItemId, Kind, Note, NoteKind, Operation},
};

pub(super) fn convert(event: &EditorEvent, state: &State) -> crate::Result<Operation> {
    let (category, code, changes, group) = match &event.event {
        EditorEventKind::TrackingGap { .. } => {
            (NoteKind::Gap, "idle.editor.capture-gap", Vec::new(), None)
        }
        EditorEventKind::WorkspaceContext { .. } => {
            (NoteKind::Label, "idle.editor.git-context", Vec::new(), None)
        }
        EditorEventKind::HumanEdit { change, .. } => (
            NoteKind::Label,
            "idle.editor.input-attribution",
            vec![*change],
            None,
        ),
        EditorEventKind::HumanEditBatch { edits, group } => (
            NoteKind::Label,
            "idle.editor.input-attribution",
            edits.iter().map(|edit| edit.change).collect(),
            *group,
        ),
        EditorEventKind::ObservedEditBatch { changes, group } => (
            NoteKind::Label,
            "idle.editor.observed-edit",
            changes.clone(),
            Some(*group),
        ),
        EditorEventKind::EditorActivated { .. }
        | EditorEventKind::SelectionChanged { .. }
        | EditorEventKind::VisibleRangesChanged { .. } => (
            NoteKind::Comment,
            "idle.editor.observation",
            Vec::new(),
            None,
        ),
        EditorEventKind::TrackingStarted { .. }
        | EditorEventKind::TrackingStopped
        | EditorEventKind::DocumentSnapshot { .. }
        | EditorEventKind::DocumentChanged { .. }
        | EditorEventKind::DocumentSaved { .. }
        | EditorEventKind::DocumentRenamed { .. }
        | EditorEventKind::EditorOpened { .. }
        | EditorEventKind::EditorClosed { .. }
        | EditorEventKind::CodeExposure { .. }
        | EditorEventKind::CodeRead { .. } => return Err("event is not an editor note".into()),
    };
    let targets: Vec<_> = changes
        .iter()
        .map(|change| identity::operation(&event.session, *change, "activity"))
        .collect();
    let mut items = Vec::new();
    for target in &targets {
        items.push(
            state
                .change(*target)
                .ok_or("input receipt must target an available document change")?,
        );
    }
    if items.is_empty() {
        items.push(identity::session(event));
    }
    let item = match group {
        Some(group) => ItemId::derive(
            "idle.vscode.edit-group.v1",
            format!("{}\0{group}", event.session).as_bytes(),
        ),
        None => ItemId::derive(
            "idle.vscode.note.v1",
            identity::event(event, "activity").as_bytes(),
        ),
    };
    let mut operation = super::operation(
        event,
        "activity",
        item,
        Kind::Note(Note {
            category,
            targets: targets.clone(),
            items,
            version: 1,
            content: Payload::Inline(serde_json::to_vec(&event.event)?),
            code: super::inline(code),
        }),
    );
    operation.parents.extend(targets);
    if code == "idle.editor.input-attribution" {
        operation.author = identity::person(event);
    }
    Ok(operation)
}
