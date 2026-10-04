//! Framed repository reads for one immutable installation; no provider semantics in hosts.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Binding, Credentials, Reader, Snapshot};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    body: Read,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    credentials: Option<Credentials>,
    #[serde(default)]
    refresh_github: bool,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    body: Result<Snapshot, String>,
}

/// Serve bounded length-prefixed JSON while the host owns the process.
/// Each request supplies the current host credential; no secret is a process argument.
///
/// # Errors
/// Returns invalid installation, framing, output size or transport errors.
pub async fn serve(
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    binding: Binding,
) -> io::Result<()> {
    let mut reader = Reader::new(binding)?;
    let mut input = input;
    loop {
        let mut header = [0; 4];
        let Some((first, rest)) = header.split_first_mut() else {
            return Err(io::Error::other("Invalid repository frame header."));
        };
        if input.read(std::slice::from_mut(first)).await? == 0 {
            return Ok(());
        }
        let _read = input.read_exact(rest).await?;
        let length = usize::try_from(u32::from_le_bytes(header)).map_err(io::Error::other)?;
        if !(1..=16 * 1024).contains(&length) {
            return Err(io::Error::other("Invalid repository request length."));
        }
        let mut bytes = vec![0; length];
        let _read = input.read_exact(&mut bytes).await?;
        let request: Request = serde_json::from_slice(&bytes)
            .map_err(|_error| io::Error::other("Invalid repository request."))?;
        let response = Response {
            id: request.id,
            body: reader
                .read(
                    request.body.credentials.as_ref(),
                    request.body.refresh_github,
                )
                .await
                .map_err(|error| error.to_string()),
        };
        let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other(
                "Repository snapshot exceeds the response limit.",
            ));
        }
        output
            .write_all(
                &u32::try_from(bytes.len())
                    .map_err(io::Error::other)?
                    .to_le_bytes(),
            )
            .await?;
        output.write_all(&bytes).await?;
        output.flush().await?;
    }
}
