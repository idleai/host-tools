//! Convert source observations without mutating earlier operations or authors.

mod files;
mod notes;

use editchain_core::{
    ContentId, Payload,
    activity::{
        Author, AuthorRole, ItemId, Kind, NativeId, Operation, Original, OriginalRef, Session,
        SessionAction,
    },
};

use crate::{
    CaptureBlobs, identity,
    state::State,
    wire::{EditorEvent, EditorEventKind},
};

pub(crate) const CONVERTER: &str = "idle.vscode.schema3.v1";

pub(crate) fn payload(bytes: &[u8], blobs: &mut dyn CaptureBlobs) -> crate::Result<Payload> {
    Ok(Payload::Blob(blobs.retain(bytes)?))
}

pub(crate) fn content(bytes: &[u8]) -> ContentId {
    ContentId::Hash256(*blake3::hash(bytes).as_bytes())
}

pub(crate) fn inline(text: &str) -> Payload {
    Payload::Inline(text.as_bytes().to_vec())
}

pub(crate) fn operation(event: &EditorEvent, lane: &str, item: ItemId, kind: Kind) -> Operation {
    let mut operation = Operation::new(
        identity::event(event, lane),
        item,
        identity::recorder(event),
        kind,
    );
    operation.session = Some(identity::session(event));
    operation.time_ms = Some(event.time_ms);
    operation.sequence = Some(event.sequence);
    operation.parents.push(identity::event(event, "raw"));
    operation.original = Some(OriginalRef {
        operation: identity::event(event, "raw"),
        converter: CONVERTER.into(),
    });
    operation
}

pub(crate) fn original(
    event: &EditorEvent,
    raw: &[u8],
    blobs: &mut dyn CaptureBlobs,
) -> crate::Result<Operation> {
    let id = identity::event(event, "raw");
    let mut operation = operation(
        event,
        "raw",
        ItemId::derive("idle.vscode.raw.v1", id.as_bytes()),
        Kind::Original(Original {
            provider: "vscode.editor".into(),
            format: Some("vscode.editor/1".into()),
            native: vec![
                NativeId {
                    kind: "recorder-session".into(),
                    value: event.session.clone(),
                },
                NativeId {
                    kind: "sequence".into(),
                    value: event.sequence.to_string(),
                },
            ],
            location: None,
            bytes: payload(raw, blobs)?,
            hash: Some(*blake3::hash(raw).as_bytes()),
        }),
    );
    operation.parents.clear();
    operation.original = None;
    if event.sequence > 1 {
        operation.parents.push(identity::operation(
            &event.session,
            event.sequence.saturating_sub(1),
            "raw",
        ));
    }
    Ok(operation)
}

pub(crate) fn activities(
    event: &EditorEvent,
    state: &State,
    blobs: &mut dyn CaptureBlobs,
) -> crate::Result<Vec<Operation>> {
    let mut operations = Vec::new();
    if matches!(event.event, EditorEventKind::TrackingStarted { .. }) {
        operations.push(operation(
            event,
            "recorder",
            identity::recorder(event),
            Kind::Author(Author {
                label: inline("Idle VS Code recorder"),
                role: AuthorRole::System,
                native_role: Payload::Empty,
                metadata: inline(&serde_json::to_string(&event.event)?),
            }),
        ));
    }
    if (matches!(event.event, EditorEventKind::TrackingStarted { .. }) || event.user_name.is_some())
        && let Some(person) = identity::person(event)
    {
        operations.push(operation(
            event,
            "person",
            person,
            Kind::Author(Author {
                label: event.user_name.as_deref().map_or(Payload::Empty, inline),
                role: AuthorRole::Person,
                native_role: inline("unsigned-local-contributor"),
                metadata: inline(&serde_json::to_string(&event.identity)?),
            }),
        ));
    }
    let primary = match &event.event {
        EditorEventKind::TrackingStarted { .. } | EditorEventKind::TrackingStopped => operation(
            event,
            "activity",
            identity::session(event),
            Kind::Session(Session {
                action: if matches!(event.event, EditorEventKind::TrackingStopped) {
                    SessionAction::Ended
                } else {
                    SessionAction::Started
                },
                label: Payload::Empty,
                settings: inline(&serde_json::to_string(&event.event)?),
                participants: identity::person(event).into_iter().collect(),
                parent: None,
                initiated_by: None,
            }),
        ),
        EditorEventKind::DocumentSnapshot { .. }
        | EditorEventKind::DocumentChanged { .. }
        | EditorEventKind::DocumentSaved { .. }
        | EditorEventKind::DocumentRenamed { .. }
        | EditorEventKind::EditorOpened { .. }
        | EditorEventKind::EditorClosed { .. }
        | EditorEventKind::CodeRead { .. }
        | EditorEventKind::CodeExposure { .. } => files::convert(event, state, blobs)?,
        EditorEventKind::WorkspaceContext { .. }
        | EditorEventKind::TrackingGap { .. }
        | EditorEventKind::HumanEdit { .. }
        | EditorEventKind::HumanEditBatch { .. }
        | EditorEventKind::ObservedEditBatch { .. }
        | EditorEventKind::EditorActivated { .. }
        | EditorEventKind::SelectionChanged { .. }
        | EditorEventKind::VisibleRangesChanged { .. } => notes::convert(event, state)?,
    };
    operations.push(primary);
    Ok(operations)
}
