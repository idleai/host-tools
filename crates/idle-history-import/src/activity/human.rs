//! Archive envelopes retain their own addresses; editor activities use live IDs.

use editchain_core::{
    activity::{Entity, ItemId, Kind, Link, Operation, OriginalRef},
    BlobRef, ContentId, Op, OpId, Payload,
};
use editchain_store::{BlobResolution, BlobSource};
use idle_editor_capture::{wire::EditorEvent, CaptureBlobs, EditorReplay};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::io;

use crate::{BlobSink, ImportError};

#[derive(Debug, Default)]
pub(super) struct Human {
    replay: EditorReplay,
}

#[derive(Deserialize)]
struct Archive<'a> {
    format: Option<&'a str>,
    source: Option<&'a str>,
    #[serde(borrow)]
    event: &'a RawValue,
}

fn event(bytes: &[u8]) -> Option<(EditorEvent, &RawValue)> {
    let archive: Archive<'_> = serde_json::from_slice(bytes).ok()?;
    if archive.format != Some("editchain-human-history") && archive.source != Some("vscode.editor")
    {
        return None;
    }
    Some((
        serde_json::from_str(archive.event.get()).ok()?,
        archive.event,
    ))
}

impl Human {
    pub(super) fn observe(
        &mut self,
        bytes: &[u8],
        blobs: &mut dyn BlobSink,
    ) -> Result<(), ImportError> {
        if let Some((event, _raw)) = event(bytes) {
            self.replay
                .observe(&event, &mut Blobs(blobs))
                .map_err(|error| {
                    ImportError::OpSink(format!("editor archive conversion: {error}"))
                })?;
        }
        Ok(())
    }

    pub(super) fn activities(
        &self,
        original: &Operation,
        bytes: &[u8],
        blobs: &mut dyn BlobSink,
    ) -> Result<Vec<Op>, ImportError> {
        let Some((event, raw)) = event(bytes) else {
            return Ok(Vec::new());
        };
        let operations = self
            .replay
            .convert(&event, raw.get().as_bytes(), &mut Blobs(blobs))
            .map_err(|error| ImportError::OpSink(format!("editor archive conversion: {error}")))?;
        let mut output = Vec::new();
        if let Some(raw) = operations.first() {
            let id = OpId::from_bytes(blake3::derive_key(
                "idle.editor.archive-link.v1",
                original.id.as_bytes(),
            ));
            let mut link = Operation::new(
                id,
                ItemId::derive("idle.editor.archive-link.v1", id.as_bytes()),
                original.recorder,
                Kind::Link(Link {
                    from: Entity::Operation(original.id),
                    relation: "OccurrenceOf".into(),
                    to: vec![Entity::Operation(raw.id)],
                    content: Payload::Empty,
                }),
            );
            link.parents = vec![original.id, raw.id];
            link.original = Some(OriginalRef {
                operation: original.id,
                converter: super::CONTRACT.into(),
            });
            output.push(
                link.into_op()
                    .map_err(|error| ImportError::OpSink(error.to_string()))?,
            );
        }
        for operation in operations {
            output.push(
                operation
                    .into_op()
                    .map_err(|error| ImportError::OpSink(error.to_string()))?,
            );
        }
        Ok(output)
    }
}

struct Blobs<'a>(&'a mut dyn BlobSink);

impl BlobSource for Blobs<'_> {
    fn read_content(&self, id: ContentId) -> io::Result<BlobResolution> {
        self.0
            .read_content(id)
            .map(|bytes| bytes.map_or(BlobResolution::Missing, BlobResolution::Found))
            .map_err(io::Error::other)
    }
}

impl CaptureBlobs for Blobs<'_> {
    fn retain(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
        self.0.put(bytes).map_err(io::Error::other)
    }
}

pub(super) fn previous_conversion(record: &Operation) -> bool {
    record.original.as_ref().is_some_and(|original| {
        ["editchain.human-activity.v1", "editchain.human-author.v1"]
            .iter()
            .any(|namespace| {
                record.id
                    == OpId::from_bytes(blake3::derive_key(
                        namespace,
                        original.operation.as_bytes(),
                    ))
            })
    })
}
