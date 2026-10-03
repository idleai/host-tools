//! File and stdin adapters; JSON objects, arrays and JSONL share one decoder.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    sync::mpsc,
    time::Duration,
};

use super::error::{Failure, Result};

const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn reader(path: &Path) -> Result<Box<dyn Read>> {
    if path == Path::new("-") {
        Ok(Box::new(io::stdin()))
    } else {
        Ok(Box::new(File::open(path)?))
    }
}

pub(super) fn bytes(path: &Path) -> Result<Vec<u8>> {
    let maximum =
        u64::try_from(MAX_INPUT_BYTES).map_err(|error| Failure::input(error.to_string()))?;
    let mut bytes = Vec::new();
    let _count = reader(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(Failure::input("input exceeds the 64 MiB object limit"));
    }
    Ok(bytes)
}

pub(super) fn stdin_chunks() -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(2);
    let _reader = std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut bytes = vec![0; 64 * 1024];
            let result = input.read(&mut bytes).map(|count| {
                bytes.truncate(count);
                bytes
            });
            let ended = !result.as_ref().is_ok_and(|bytes| !bytes.is_empty());
            if sender.send(result).is_err() || ended {
                break;
            }
        }
    });
    receiver
}

pub(super) fn provider_bytes(
    path: &Path,
    cancellation: &idle_history_import::cancellation::ImportCancellation,
) -> Result<Vec<u8>> {
    if path != Path::new("-") {
        return bytes(path);
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
