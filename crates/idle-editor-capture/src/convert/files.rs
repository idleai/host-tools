//! Exact UTF-8 contents and native UTF-16 coordinates stay distinct.

use crate::{
    CaptureBlobs, identity,
    state::State,
    wire::{EditorEvent, EditorEventKind, EditorRange},
};
use editchain_core::{
    ByteRange, FileEdit, Payload,
    activity::{ChangeState, File, FileAction, ItemId, Kind, Operation, TextEdit, TextRange},
};
use editchain_store::BlobResolution;

pub(super) fn convert(
    event: &EditorEvent,
    state: &State,
    blobs: &mut dyn CaptureBlobs,
) -> crate::Result<Operation> {
    let mut file = empty();
    let mut causes = Vec::new();
    let mut parents = Vec::new();
    let document = match &event.event {
        EditorEventKind::DocumentSnapshot { document, text } => {
            file.action = FileAction::Snapshot;
            let _payload = super::payload(text.as_bytes(), blobs)?;
            file.after = Some(super::content(text.as_bytes()));
            Some(document)
        }
        EditorEventKind::DocumentChanged {
            document,
            before_version,
            before,
            after,
            changes,
            ..
        } => {
            file.action = FileAction::Change;
            file.change = Some(ChangeState::Applied);
            let _before = super::payload(before.as_bytes(), blobs)?;
            let after_bytes = super::payload(after.as_bytes(), blobs)?;
            file.before = Some(super::content(before.as_bytes()));
            file.after = Some(super::content(after.as_bytes()));
            file.edit = FileEdit::ReplaceBytes {
                range: ByteRange {
                    start: 0,
                    end: u64::try_from(before.len())?,
                },
                bytes: after_bytes,
            };
            let previous = identity::revision(event, document, *before_version);
            causes.push(previous);
            let known = state
                .revision(previous)
                .filter(|revision| revision.sequence < event.sequence)
                .ok_or("source revision is unavailable; capture a baseline first")?;
            if Some(known.content) != file.before {
                return Err("source revision content differs from the captured baseline".into());
            }
            parents.push(known.operation);
            file.text_edits = changes
                .iter()
                .map(|change| TextEdit {
                    offset_utf16: u64::from(change.offset),
                    length_utf16: u64::from(change.length),
                    text: super::inline(&change.text),
                })
                .collect();
            Some(document)
        }
        EditorEventKind::DocumentSaved { document } => {
            file.action = FileAction::Save;
            Some(document)
        }
        EditorEventKind::CodeExposure {
            document,
            ranges,
            duration_ms,
            ..
        }
        | EditorEventKind::CodeRead {
            document,
            ranges,
            duration_ms,
            ..
        } => {
            file.action = if matches!(event.event, EditorEventKind::CodeRead { .. }) {
                FileAction::Read
            } else {
                FileAction::View
            };
            if file.action == FileAction::Read
                && state
                    .dwell(identity::session(event))
                    .is_none_or(|dwell| *duration_ms < dwell)
            {
                return Err("reading interval did not reach the recorded dwell threshold".into());
            }
            file.duration_ms = Some(*duration_ms);
            file.text_ranges = ranges
                .iter()
                .map(|range| TextRange {
                    start: range.start,
                    end: range.end,
                })
                .collect();
            let item = identity::revision(event, document, document.version);
            let known = state
                .revision(item)
                .filter(|revision| revision.sequence < event.sequence)
                .ok_or("viewed revision is unavailable; capture a baseline first")?;
            let BlobResolution::Found(bytes) = blobs.read_content(known.content)? else {
                return Err("viewed revision bytes are unavailable".into());
            };
            let text = std::str::from_utf8(&bytes)?;
            file.ranges = ranges
                .iter()
                .map(|range| byte_range(text, range))
                .collect::<crate::Result<_>>()?;
            parents.push(known.operation);
            Some(document)
        }
        EditorEventKind::DocumentRenamed { from, to } => {
            file.action = FileAction::Rename;
            file.name = super::inline(from);
            file.renamed_to = Some(super::inline(to));
            file.change = Some(ChangeState::Applied);
            file.path = identity::path(from);
            None
        }
        EditorEventKind::EditorOpened { uri, path, .. }
        | EditorEventKind::EditorClosed { uri, path, .. } => {
            file.action = if matches!(event.event, EditorEventKind::EditorOpened { .. }) {
                FileAction::Open
            } else {
                FileAction::Close
            };
            let name = path.as_deref().unwrap_or(uri);
            file.path = identity::path(name);
            file.name = super::inline(name);
            None
        }
        EditorEventKind::WorkspaceContext { .. }
        | EditorEventKind::TrackingStarted { .. }
        | EditorEventKind::TrackingStopped
        | EditorEventKind::TrackingGap { .. }
        | EditorEventKind::HumanEdit { .. }
        | EditorEventKind::HumanEditBatch { .. }
        | EditorEventKind::ObservedEditBatch { .. }
        | EditorEventKind::EditorActivated { .. }
        | EditorEventKind::SelectionChanged { .. }
        | EditorEventKind::VisibleRangesChanged { .. } => {
            return Err("event is not a file observation".into());
        }
    };
    if let Some(document) = document {
        let revision = identity::revision(event, document, document.version);
        file.revision = Some(revision);
        let name = document.path.as_deref().unwrap_or(&document.uri);
        file.name = super::inline(name);
        file.path = identity::path(name);
        if let Some(known) = state
            .revision(revision)
            .filter(|revision| revision.sequence <= event.sequence)
        {
            if file.after.is_some_and(|content| content != known.content) {
                return Err("revision identity was reused with different content".into());
            }
            if file.after.is_none() {
                file.after = Some(known.content);
            }
            if file.action == FileAction::Save {
                parents.push(known.operation);
            }
        } else if file.action == FileAction::Save {
            return Err("saved revision is unavailable".into());
        }
    }
    if let Some(context) = state.context(identity::session(event), event.sequence) {
        parents.push(context);
    }
    let item = file.revision.unwrap_or_else(|| {
        ItemId::derive(
            "idle.vscode.file-event.v1",
            identity::event(event, "activity").as_bytes(),
        )
    });
    let mut operation = super::operation(event, "activity", item, Kind::File(file));
    operation.causes = causes;
    operation.parents.extend(parents);
    Ok(operation)
}

fn empty() -> File {
    File {
        action: FileAction::Snapshot,
        path: identity::path(""),
        name: Payload::Empty,
        renamed_to: None,
        revision: None,
        before: None,
        after: None,
        edit: FileEdit::None,
        text_edits: Vec::new(),
        change: None,
        caused_by: None,
        ranges: Vec::new(),
        text_ranges: Vec::new(),
        duration_ms: None,
    }
}

fn byte_range(text: &str, range: &EditorRange) -> crate::Result<ByteRange> {
    Ok(ByteRange {
        start: position(text, range.start)?,
        end: position(text, range.end)?,
    })
}

fn position(text: &str, [line, column]: [u32; 2]) -> crate::Result<u64> {
    let mut offset = 0_usize;
    for _ in 0..line {
        let newline = text
            .get(offset..)
            .and_then(|rest| rest.find('\n'))
            .ok_or("editor range line is outside the revision")?;
        offset = offset.saturating_add(newline).saturating_add(1);
    }
    let rest = text
        .get(offset..)
        .ok_or("editor range offset is outside the revision")?;
    let line_text = rest.split('\n').next().unwrap_or("");
    let line_text = line_text.strip_suffix('\r').unwrap_or(line_text);
    let change = crate::wire::EditorChange {
        offset: column,
        length: 0,
        text: String::new(),
    };
    let range = change
        .byte_range(line_text)
        .ok_or("editor range splits a UTF-16 character or exceeds the line")?;
    Ok(u64::try_from(offset.saturating_add(range.start))?)
}
