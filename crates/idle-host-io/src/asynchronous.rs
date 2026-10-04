//! Async adapters with the same boundaries as the blocking frame codec.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Read one frame, returning `None` only at a clean frame boundary.
/// # Errors
/// Returns truncated input, zero or excessive lengths, or transport errors.
pub async fn read_frame(
    input: &mut (impl AsyncRead + Unpin),
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    let [first, rest @ ..] = &mut header;
    match input.read_exact(std::slice::from_mut(first)).await {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => {
            let _read = result?;
        }
    }
    let _read = input.read_exact(rest).await?;
    let mut bytes = vec![0; super::length(header, maximum)?];
    let _read = input.read_exact(&mut bytes).await?;
    Ok(Some(bytes))
}

/// Write and flush one bounded frame without changing its payload.
/// # Errors
/// Returns invalid lengths or transport errors.
pub async fn write_frame(
    output: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
    maximum: usize,
) -> io::Result<()> {
    output
        .write_all(&super::header(bytes.len(), maximum)?)
        .await?;
    output.write_all(bytes).await?;
    output.flush().await
}
