//! Provider cancellation around shared bounded CLI inputs.

use super::error::{Failure, Result};
use editchain_cli_support::input::{bytes, stdin_chunks, MAX_INPUT_BYTES};
use std::{path::Path, sync::mpsc, time::Duration};

pub(super) fn provider_bytes(
    path: &Path,
    cancellation: &idle_history_import::cancellation::ImportCancellation,
) -> Result<Vec<u8>> {
    if path != Path::new("-") {
        return bytes(path).map_err(Into::into);
    }
    let input = stdin_chunks();
    let mut bytes = Vec::new();
    loop {
        cancellation.check(path)?;
        match input.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(chunk)) if chunk.is_empty() => return Ok(bytes),
            Ok(Ok(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > MAX_INPUT_BYTES {
                    return Err(Failure::input("input exceeds the 64 MiB object limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::new(1, "stdin reader disconnected"))
            }
        }
    }
}
