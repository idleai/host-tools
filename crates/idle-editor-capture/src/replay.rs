//! Archive conversion shares the live recorder's identities and semantic rules.

use editchain_core::activity::Operation;

use crate::{CaptureBlobs, convert, state::State, wire::EditorEvent};

/// Small revision and session context gathered before converting archive records.
/// Source order across files does not affect the resulting operation identities.
#[derive(Debug, Default)]
pub struct EditorReplay {
    state: State,
}

impl EditorReplay {
    /// Observe validated source metadata and retain referenced buffer contents.
    /// Supply the complete available source context, including accepted prefixes,
    /// before calling [`Self::convert`]. This method does not emit operations.
    /// # Errors
    /// Returns observation validation or content storage errors.
    pub fn observe(
        &mut self,
        event: &EditorEvent,
        blobs: &mut dyn CaptureBlobs,
    ) -> crate::Result<()> {
        event.validate()?;
        self.state.observe(event, blobs)
    }

    /// Convert one event using the same bytes, IDs and rules as live capture.
    /// The raw slice must be the event JSON, excluding any archive envelope.
    /// # Errors
    /// Returns invalid input, unavailable revision or blob storage errors.
    pub fn convert(
        &self,
        event: &EditorEvent,
        raw: &[u8],
        blobs: &mut dyn CaptureBlobs,
    ) -> crate::Result<Vec<Operation>> {
        event.validate()?;
        let mut operations = vec![convert::original(event, raw, blobs)?];
        operations.extend(convert::activities(event, &self.state, blobs)?);
        Ok(operations)
    }
}
