//! Framed repository reads for one immutable installation; no provider semantics in hosts.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

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
    while let Some(bytes) = idle_host_io::asynchronous::read_frame(&mut input, 16 * 1024).await? {
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
        idle_host_io::asynchronous::write_frame(&mut output, &bytes, 8 * 1024 * 1024).await?;
    }
    Ok(())
}
